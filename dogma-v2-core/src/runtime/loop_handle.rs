//! # RuntimeLoop — Orquestador del ciclo RSI
//!
//! El loop principal del agente:
//!
//! 1. Recibe un prompt del usuario.
//! 2. Construye el contexto con historial + tools disponibles.
//! 3. Envía al LLM y procesa la respuesta.
//! 4. Si hay tool calls, las ejecuta y realimenta al LLM.
//! 5. Repite hasta obtener una respuesta final.

use std::sync::Arc;

use crate::runtime::provider::{LLMProvider, LLMResponse, Message, MessageRole};
use crate::state::session::SessionManager;
use crate::tools::{Tool, ToolRegistry};
use dogma_v2_common::Result;
use parking_lot::RwLock;
use tracing::{debug, error, info, warn};

/// Configuración del runtime loop.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Máximo de iteraciones de tool calls antes de forzar respuesta.
    pub max_tool_iterations: u32,
    /// Habilitar compresión de contexto.
    pub context_compression: bool,
    /// Ventana deslizante: máximo de mensajes calientes en el payload del LLM.
    /// Los mensajes más antiguos se evictan de la ventana caliente pero
    /// permanecen persistidos en dogma-vdb para búsqueda semántica.
    pub max_hot_messages: usize,
    /// Habilitar búsqueda semántica previa al prompt para inyectar
    /// contexto histórico relevante como mensaje system.
    pub semantic_lookup: bool,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 10,
            context_compression: true,
            max_hot_messages: 8,
            semantic_lookup: true,
        }
    }
}

/// Estado interno del loop.
#[derive(Debug)]
struct LoopState {
    iteration: u32,
    messages: Vec<Message>,
}

/// El orquestador principal del ciclo IA.
pub struct RuntimeLoop {
    provider: Arc<dyn LLMProvider>,
    tools: Arc<RwLock<ToolRegistry>>,
    session: Arc<RwLock<SessionManager>>,
    config: LoopConfig,
    state: RwLock<LoopState>,
}

impl RuntimeLoop {
    /// Crea un nuevo RuntimeLoop.
    pub fn new(
        provider: Arc<dyn LLMProvider>,
        tools: ToolRegistry,
        session: SessionManager,
        config: LoopConfig,
    ) -> Self {
        Self {
            provider,
            tools: Arc::new(RwLock::new(tools)),
            session: Arc::new(RwLock::new(session)),
            config,
            state: RwLock::new(LoopState {
                iteration: 0,
                messages: Vec::new(),
            }),
        }
    }

    /// Ejecuta el ciclo RSI con un prompt de entrada.
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Fatal` si hay un fallo de I/O al persistir el
    /// estado. Devuelve `Error::Infrastructure` si el provider falla.
    pub async fn run(&self, prompt: &str, session_id: &str) -> Result<String> {
        info!("Runtime loop starting for session {}", session_id);

        // Reset state for this run
        {
            let mut state = self.state.write();
            state.iteration = 0;
            state.messages.clear();
            state.messages.push(Message::new(MessageRole::User, prompt));
        }

        // Persist user message in session
        {
            let mut session = self.session.write();
            session.append_message(session_id, MessageRole::User, prompt)?;
        }

        let result = self.tool_loop(session_id).await;

        // Persist final result
        if let Ok(ref final_content) = result {
            let mut session = self.session.write();
            session.append_message(session_id, MessageRole::Assistant, final_content)?;
        }

        result
    }

    /// Bucle interno que alterna entre LLM y tool calls.
    async fn tool_loop(&self, session_id: &str) -> Result<String> {
        loop {
            // Check iteration limit
            {
                let state = self.state.read();
                if state.iteration >= self.config.max_tool_iterations {
                    warn!(
                        "Max tool iterations ({}) reached, forcing final response",
                        self.config.max_tool_iterations
                    );
                    return Ok("Max iterations reached. Please refine your request.".into());
                }
            }

            // Apply context compression if enabled
            if self.config.context_compression {
                self.maybe_compress_context().await;
            }

            // Call LLM
            let messages = {
                let state = self.state.read();
                state.messages.clone()
            };

            // ── Ventana deslizante ─────────────────────────────────
            let messages = if messages.len() > self.config.max_hot_messages {
                Self::apply_sliding_window(&messages, self.config.max_hot_messages)
            } else {
                messages
            };

            // ── Contexto semántico ─────────────────────────────────
            let messages = if self.config.semantic_lookup && self.config.context_compression {
                self.inject_semantic_context(&messages, session_id).await
            } else {
                messages
            };

            // Extraer tool specs del registro local y pasarlas al provider
            let tool_specs = {
                let tools = self.tools.read();
                tools.tool_specs()
            };

            debug!(
                "Sending {} messages + {} tools to LLM",
                messages.len(),
                tool_specs.len()
            );

            let response: LLMResponse =
                self.provider
                    .chat(&messages, &tool_specs)
                    .await
                    .map_err(|e| {
                        error!("LLM provider error: {e}");
                        e
                    })?;

            // Persist assistant response
            {
                let mut session = self.session.write();
                session.append_message(session_id, MessageRole::Assistant, &response.content)?;
            }

            // If no tool calls, we're done
            if response.tool_calls.is_empty() {
                info!("No tool calls — returning final response");
                return Ok(response.content);
            }

            // Process tool calls
            let tool_calls = response.tool_calls.clone();

            // Increment iteration and add assistant message under state lock,
            // then release before any async work
            {
                let mut state = self.state.write();
                state.iteration += 1;
                let mut msg = Message::new(MessageRole::Assistant, &response.content);
                if !response.tool_calls.is_empty() {
                    msg = msg.with_tool_calls(response.tool_calls.clone());
                }
                for (key, val) in &response.extra_fields {
                    msg = msg.with_extra_field(key, val.clone());
                }
                state.messages.push(msg);
            }

            for tc in &tool_calls {
                info!("Executing tool: {} (id={})", tc.name, tc.id);

                // Get tool reference under tools lock, release before async call
                let tool_ref = {
                    let tools = self.tools.read();
                    tools.get_tool(&tc.name)
                };

                let tool_result: String = match tool_ref {
                    Some(tool) => match serde_json::from_str(&tc.arguments) {
                        Ok(args) => match tool.call(&args).await {
                            Ok(output) => output,
                            Err(e) => {
                                error!("Tool {} failed: {e}", tc.name);
                                format!("error: {e}")
                            }
                        },
                        Err(e) => {
                            let msg = format!("error: invalid arguments for {}: {}", tc.name, e);
                            error!("{msg}");
                            msg
                        }
                    },
                    None => {
                        let msg = format!("tool not found: {}", tc.name);
                        error!("{msg}");
                        msg
                    }
                };

                // Persist tool result under session lock
                {
                    let mut session = self.session.write();
                    session.append_tool_result(session_id, &tc.name, &tc.id, &tool_result)?;
                }

                // Add result to local state
                {
                    let mut state = self.state.write();
                    state.messages.push(
                        Message::new(MessageRole::Tool, &tool_result)
                            .with_tool_result(&tc.id, &tc.name),
                    );
                }
            }
        }
    }

    /// Intenta comprimir el contexto si ha superado el umbral.
    async fn maybe_compress_context(&self) {
        let msg_count = {
            let state = self.state.read();
            state.messages.len()
        };

        // Umbral simple: comprimir si hay más de 20 mensajes
        if msg_count > 20 {
            debug!("Context has {msg_count} messages, applying compression");
            // Nota: la compresión real se implementará en el compresor
            // del módulo `state::compressor`. Por ahora registramos
            // la intención.
        }
    }

    /// Aplica la ventana deslizante: conserva los últimos `max` mensajes,
    /// pero siempre preserva los mensajes `System`.
    fn apply_sliding_window(messages: &[Message], max: usize) -> Vec<Message> {
        // Separar system del resto
        let systems: Vec<Message> = messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .cloned()
            .collect();

        let count_systems = systems.len();

        // Si hay más system messages que el límite, tomar los últimos
        let (systems, remaining_slots) = if count_systems >= max {
            (systems[count_systems - max..].to_vec(), 0)
        } else {
            (systems, max - count_systems)
        };

        if remaining_slots == 0 {
            return systems;
        }

        // Tomar los últimos `remaining_slots` mensajes no-system
        let non_systems: Vec<Message> = messages
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .cloned()
            .collect();

        let start = non_systems.len().saturating_sub(remaining_slots);
        let mut result = systems;
        result.extend_from_slice(&non_systems[start..]);
        result
    }

    /// Busca contexto semánticamente similar en la sesión y lo inyecta
    /// como mensaje `System` al inicio del array si hay resultados.
    async fn inject_semantic_context(&self, messages: &[Message], session_id: &str) -> Vec<Message> {
        // Tomar el último mensaje de usuario como query de búsqueda
        let user_query = match messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
        {
            Some(m) => m.content.as_str(),
            None => return messages.to_vec(),
        };

        let matches = {
            let session = self.session.read();
            session
                .search_similar(user_query, session_id, 5)
                .unwrap_or_default()
        };

        if matches.is_empty() {
            return messages.to_vec();
        }

        // Formatear como contexto histórico
        let context: String = matches
            .iter()
            .map(|m| {
                format!(
                    "[Previous context (score: {:.2})]\n{}",
                    m.score, m.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let context_msg = Message::new(
            MessageRole::System,
            format!("Relevant context from earlier in this session:\n{context}"),
        );

        let mut result = vec![context_msg];
        result.extend(messages.iter().cloned());
        result
    }

    /// Añade una herramienta al registro.
    pub fn register_tool(&self, tool: Box<dyn Tool>) {
        let mut tools = self.tools.write();
        tools.register(tool);
    }

    /// Devuelve una referencia al registro de herramientas.
    pub fn tool_registry(&self) -> Arc<RwLock<ToolRegistry>> {
        Arc::clone(&self.tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, content: &str) -> Message {
        Message::new(role, content)
    }

    #[test]
    fn test_sliding_window_under_limit() {
        let msgs = vec![
            msg(MessageRole::User, "hi"),
            msg(MessageRole::Assistant, "hello"),
        ];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 10);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "hi");
    }

    #[test]
    fn test_sliding_window_trims_oldest() {
        let msgs = vec![
            msg(MessageRole::User, "prompt1"),
            msg(MessageRole::Assistant, "resp1"),
            msg(MessageRole::User, "prompt2"),
            msg(MessageRole::Assistant, "resp2"),
            msg(MessageRole::User, "prompt3"),
        ];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 3);
        assert_eq!(result.len(), 3);
        // Debe mantener los 3 más recientes
        assert_eq!(result[0].content, "prompt2");
        assert_eq!(result[1].content, "resp2");
        assert_eq!(result[2].content, "prompt3");
    }

    #[test]
    fn test_sliding_window_preserves_system() {
        let msgs = vec![
            msg(MessageRole::System, "context"),
            msg(MessageRole::User, "prompt1"),
            msg(MessageRole::Assistant, "resp1"),
            msg(MessageRole::User, "prompt2"),
        ];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 3);
        assert_eq!(result.len(), 3);
        // System siempre se preserva
        assert_eq!(result[0].role, MessageRole::System);
        assert_eq!(result[0].content, "context");
        // Últimos 2 mensajes no-system
        assert_eq!(result[1].content, "resp1");
        assert_eq!(result[2].content, "prompt2");
    }

    #[test]
    fn test_sliding_window_system_only() {
        let msgs = vec![
            msg(MessageRole::System, "ctx1"),
            msg(MessageRole::System, "ctx2"),
            msg(MessageRole::System, "ctx3"),
            msg(MessageRole::User, "prompt"),
        ];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 2);
        assert_eq!(result.len(), 2);
        // Toma los últimos 2 system messages (no room for non-system)
        assert_eq!(result[0].content, "ctx2");
        assert_eq!(result[1].content, "ctx3");
    }

    #[test]
    fn test_sliding_window_exact_limit() {
        let msgs = vec![
            msg(MessageRole::User, "a"),
            msg(MessageRole::Assistant, "b"),
            msg(MessageRole::User, "c"),
        ];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 3);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_sliding_window_empty() {
        let msgs: Vec<Message> = vec![];
        let result = RuntimeLoop::apply_sliding_window(&msgs, 8);
        assert!(result.is_empty());
    }
}
