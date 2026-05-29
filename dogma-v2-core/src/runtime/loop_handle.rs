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
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 10,
            context_compression: true,
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

    /// Añade una herramienta al registro.
    pub fn register_tool(&self, tool: Box<dyn Tool>) {
        let mut tools = self.tools.write();
        tools.register(tool);
    }

    /// Devuelve un clon del handle compartido al SessionManager.
    /// Útil para construir herramientas que necesitan acceso a la sesión
    /// (ej: SearchMemoryTool).
    pub fn session_handle(&self) -> Arc<RwLock<SessionManager>> {
        Arc::clone(&self.session)
    }

    /// Devuelve una referencia al registro de herramientas.
    pub fn tool_registry(&self) -> Arc<RwLock<ToolRegistry>> {
        Arc::clone(&self.tools)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_loop_creation() {
        // Solo verifica que el runtime se puede crear con config por defecto
        let config = LoopConfig::default();
        assert_eq!(config.max_tool_iterations, 10);
        assert!(config.context_compression);
    }
}
