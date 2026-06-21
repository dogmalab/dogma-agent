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

use crate::models::events::AgentEvent;
use crate::runtime::provider::{LLMProvider, LLMResponse, Message, MessageRole, TokenUsage};
use crate::state::session::SessionManager;
use crate::tools::{Tool, ToolRegistry};
use dogma_v2_common::Result;
use dogma_vdb::doc::Document;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Default system prompt injected at the start of every session.
const DEFAULT_SYSTEM_PROMPT: &str = "\
    You are Dogma, an AI coding assistant with persistent session memory \
    and tool execution capabilities.\n\n\
    MEMORY: Your full conversation history is preserved across turns. \
    Use `search_memory` to find relevant context from any past \
    conversation.\n\n\
    TOOLS:\n\
    - `read_file`, `write_file`, `execute_script` — basic file and code operations\n\
    - `search_memory` — semantic search across all past conversations\n\
    - `plan` — create structured plans for complex tasks (use FIRST for complex work)\n\
    - `delegate_task` — spawn sub-agents for isolated execution (with optional skills)\n\
    - `install_skill` — install dynamic capabilities from skills.sh\n\n\
    WORKFLOW: For complex tasks, start by calling `plan` to create a structured \
    breakdown, then execute each step using the appropriate tools. Use \
    `delegate_task` for steps that need focused, independent sub-agents. \
    Think step by step and use tools strategically.";

/// Configuración del runtime loop.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Máximo de iteraciones de tool calls antes de forzar respuesta.
    pub max_tool_iterations: u32,
    /// Habilitar compresión de contexto.
    pub context_compression: bool,
    /// System prompt inyectado al inicio de cada sesión.
    pub system_prompt: String,
    /// Habilitar context management semántico (búsqueda en dogma-vdb).
    pub context_management: bool,
    /// Número de turnos recientes que siempre se mantienen.
    pub context_recent_turns: usize,
    /// Número máximo de mensajes relevantes a inyectar.
    pub context_max_relevant: usize,
    /// Umbral de similitud para considerar relevante (0.0–1.0).
    pub context_relevance_threshold: f32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 25,
            context_compression: true,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            context_management: false,
            context_recent_turns: 5,
            context_max_relevant: 5,
            context_relevance_threshold: 0.3,
        }
    }
}

/// Estado interno del loop.
#[derive(Debug)]
struct LoopState {
    iteration: u32,
    messages: Vec<Message>,
}

/// Convierte un `Document` de dogma-vdb a un `Message` del provider.
///
/// Solo convierte documentos con `node_type` igual a "Message".
/// Los ToolResult se saltan porque DeepSeek requiere que los mensajes
/// con `role: "tool"` estén precedidos por un assistant message con
/// `tool_calls`, y no almacenamos los tool_calls en dogma-vdb.
fn document_to_message(doc: &Document) -> Option<Message> {
    let node_type = doc.metadata_val("node_type")?;

    match node_type {
        "Message" => {
            let role_str = doc.metadata_val("role").unwrap_or("user");
            let role = match role_str {
                "system" => MessageRole::System,
                "user" => MessageRole::User,
                "assistant" => MessageRole::Assistant,
                "tool" => MessageRole::Tool,
                _ => MessageRole::User,
            };
            let mut msg = Message::new(role, doc.text.clone());

            // Restaurar extra_fields si existen (ej: reasoning_content)
            if let Some(extra_json) = doc.metadata_val("extra_fields") {
                if let Ok(extra_fields) =
                    serde_json::from_str::<Vec<(String, serde_json::Value)>>(extra_json)
                {
                    for (key, val) in extra_fields {
                        msg = msg.with_extra_field(&key, val);
                    }
                }
            }

            Some(msg)
        }
        _ => None,
    }
}

/// Carga el historial de una sesión desde dogma-vdb y lo convierte a `Vec<Message>`.
///
/// Los mensajes se devuelven ordenados por secuencia (cronológico).
fn load_session_history(session: &SessionManager, session_id: &str) -> Vec<Message> {
    match session.get_session_nodes(session_id) {
        Ok(nodes) => {
            info!(
                "Loaded {} raw nodes from session {}",
                nodes.len(),
                session_id
            );
            for (i, node) in nodes.iter().enumerate() {
                let node_type = node.metadata_val("node_type").unwrap_or("?");
                let role = node.metadata_val("role").unwrap_or("?");
                let seq = node.metadata_val("sequence").unwrap_or("?");
                let text_preview: String = node.text.chars().take(50).collect();
                info!("  node[{i}]: type={node_type} role={role} seq={seq} text={text_preview}...");
            }
            let messages: Vec<Message> = nodes.iter().filter_map(document_to_message).collect();
            info!(
                "Converted {} messages from session {}",
                messages.len(),
                session_id
            );
            messages
        }
        Err(e) => {
            warn!("Failed to load session history: {e}");
            Vec::new()
        }
    }
}

/// El orquestador principal del ciclo IA.
pub struct RuntimeLoop {
    provider: Arc<dyn LLMProvider>,
    tools: Arc<RwLock<ToolRegistry>>,
    session: Arc<RwLock<SessionManager>>,
    config: LoopConfig,
    state: RwLock<LoopState>,
    /// Canal opcional para emitir eventos de la UI reactiva.
    event_tx: Option<mpsc::Sender<AgentEvent>>,
}

impl RuntimeLoop {
    /// Crea un nuevo RuntimeLoop.
    ///
    /// * `event_tx` — Canal opcional para emitir eventos de progreso
    ///   hacia la interfaz (InlineUI). Pasar `None` si no se usa UI.
    pub fn new(
        provider: Arc<dyn LLMProvider>,
        tools: ToolRegistry,
        session: SessionManager,
        config: LoopConfig,
        event_tx: Option<mpsc::Sender<AgentEvent>>,
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
            event_tx,
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

        // Cargar historial de sesiones anteriores desde dogma-vdb
        let history = {
            let session = self.session.read();
            load_session_history(&session, session_id)
        };

        // Construir contexto: system prompt + historial + prompt actual
        {
            let mut state = self.state.write();
            state.iteration = 0;
            state.messages = history;

            // Si context management está habilitado, buscar contexto relevante
            if self.config.context_management && state.messages.len() > 1 {
                let recent = state.messages.len().min(self.config.context_recent_turns * 2);
                let recent_msgs = state.messages[state.messages.len() - recent..].to_vec();

                let cm = crate::state::context_manager::ContextManager::new(
                    crate::state::context_manager::ContextConfig {
                        recent_turns: self.config.context_recent_turns,
                        max_relevant: self.config.context_max_relevant,
                        relevance_threshold: self.config.context_relevance_threshold,
                    },
                );

                if cm.should_optimize(state.messages.len()) {
                    let session = self.session.read();
                    let search_fn = |embedding: &[f32], k: usize| {
                        session.search_similar_global_raw(embedding, k)
                    };

                    // Usar un embedder dummy si no hay embedder configurado
                    // En producción, el embedder se inyectará desde el CLI
                    if let Ok(relevant) = cm.build_context(
                        &recent_msgs,
                        session_id,
                        prompt,
                        &crate::state::session::NullEmbedder,
                        search_fn,
                    ) {
                        if !relevant.is_empty() {
                            let ctx_text = crate::state::context_manager::ContextManager::format_relevant_context(&relevant);
                            debug!(
                                "Injecting {} relevant messages into context",
                                relevant.len()
                            );
                            // Inyectar como mensaje de sistema adicional
                            state.messages.insert(
                                1,
                                crate::runtime::provider::Message::new(
                                    crate::runtime::provider::MessageRole::System,
                                    &ctx_text,
                                ),
                            );
                        }
                    }
                }
            }

            // Siempre inyectar system prompt al inicio del contexto
            state.messages.insert(
                0,
                Message::new(MessageRole::System, &self.config.system_prompt),
            );

            state.messages.push(Message::new(MessageRole::User, prompt));
        }

        debug!(
            "Context loaded: {} previous messages + new prompt",
            self.state.read().messages.len() - 1
        );

        // Persist user message in session
        {
            let mut session = self.session.write();
            session.append_message(session_id, MessageRole::User, prompt, &[])?;
        }

        let result = self.tool_loop(session_id).await;

        // Persist final result
        if let Ok(ref final_content) = result {
            let extra = {
                let state = self.state.read();
                state
                    .messages
                    .last()
                    .map(|m| m.extra_fields.clone())
                    .unwrap_or_default()
            };
            let mut session = self.session.write();
            session.append_message(session_id, MessageRole::Assistant, final_content, &extra)?;
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

            // Emitir evento de status (si hay UI conectada)
            if let Some(ref tx) = self.event_tx {
                let (msg_count, iteration) = {
                    let state = self.state.read();
                    (state.messages.len(), state.iteration)
                };
                let pct = if self.config.max_tool_iterations > 0 {
                    (iteration as f32 / self.config.max_tool_iterations as f32) * 100.0
                } else {
                    0.0
                };
                let _ = tx.try_send(AgentEvent::status(pct, msg_count as u64, String::new()));
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

            // Use streaming to emit chunks in real-time
            let mut stream_rx = self
                .provider
                .chat_stream(&messages, &tool_specs)
                .await
                .map_err(|e| {
                    error!("LLM provider error: {e}");
                    e
                })?;

            let mut content = String::new();
            let mut reasoning = String::new();
            let mut tool_calls = Vec::new();
            let mut usage = TokenUsage::default();
            let mut extra_fields = Vec::new();

            while let Some(chunk_result) = stream_rx.recv().await {
                match chunk_result {
                    Ok(super::provider::StreamChunk::ReasoningDelta(delta)) => {
                        reasoning.push_str(&delta);
                        if let Some(ref tx) = self.event_tx {
                            let _ = tx.try_send(AgentEvent::thinking_chunk(delta));
                        }
                    }
                    Ok(super::provider::StreamChunk::ContentDelta(delta)) => {
                        content.push_str(&delta);
                        if let Some(ref tx) = self.event_tx {
                            let _ = tx.try_send(AgentEvent::content_chunk(delta));
                        }
                    }
                    Ok(super::provider::StreamChunk::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta,
                    }) => {
                        // Accumulate tool call deltas
                        while tool_calls.len() <= index {
                            tool_calls.push(super::provider::ToolCall {
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                        }
                        if let Some(id) = id {
                            tool_calls[index].id = id;
                        }
                        if let Some(name) = name {
                            tool_calls[index].name = name;
                        }
                        tool_calls[index].arguments.push_str(&arguments_delta);
                    }
                    Ok(super::provider::StreamChunk::Done(u)) => {
                        usage = u;
                    }
                    Err(e) => {
                        error!("LLM stream error: {e}");
                        return Err(e);
                    }
                }
            }

            // Store reasoning_content in extra_fields for round-trip
            if !reasoning.is_empty() {
                extra_fields.push((
                    "reasoning_content".to_string(),
                    serde_json::Value::String(reasoning),
                ));
            }

            let response = LLMResponse {
                content,
                tool_calls,
                usage,
                extra_fields,
            };

            // Persist assistant response
            {
                let mut session = self.session.write();
                session.append_message(
                    session_id,
                    MessageRole::Assistant,
                    &response.content,
                    &response.extra_fields,
                )?;
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

    /// Devuelve el canal de eventos opcional para la UI reactiva.
    #[must_use]
    pub fn event_tx(&self) -> Option<mpsc::Sender<AgentEvent>> {
        self.event_tx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_loop_creation() {
        // Solo verifica que el runtime se puede crear con config por defecto
        let config = LoopConfig::default();
        assert_eq!(config.max_tool_iterations, 25);
        assert!(config.context_compression);
    }
}
