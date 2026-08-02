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
use crate::state::compressor::SemanticMatch;
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
    /// Ventana de contexto del modelo en tokens (para el % de uso).
    pub context_window: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_tool_iterations: 25,
            context_compression: true,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            context_management: true,
            context_recent_turns: 5,
            context_max_relevant: 5,
            context_relevance_threshold: 0.3,
            context_window: 128_000,
        }
    }
}

/// Estado interno del loop.
#[derive(Debug)]
struct LoopState {
    iteration: u32,
    messages: Vec<Message>,
    /// Tokens totales consumidos en la sesión (acumulado).
    session_tokens: u64,
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

/// Extrae los últimos N turnos del historial de forma segura.
///
/// Un "turno" = User message + Assistant response (2 mensajes).
/// Usa skip/take para evitar pánicos por out-of-bounds.
fn extract_recent_turns(history: &[Message], recent_turns: usize) -> &[Message] {
    let max_messages = recent_turns * 2;
    if history.len() <= max_messages {
        history
    } else {
        &history[history.len() - max_messages..]
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
    /// Contexto del sistema detectado (OS, project, git).
    system_context: crate::state::system_context::SystemContext,
    /// Memoria persistente del usuario (key-value store).
    user_memory: Option<Arc<RwLock<crate::state::user_memory::UserMemory>>>,
    /// Colección del workspace (código indexado con SML).
    /// Fuente única de verdad técnica — baja entropía.
    workspace_collection: Option<Arc<RwLock<dogma_vdb::collection::Collection>>>,
    /// Plan activo actual (si existe).
    active_plan: Option<crate::models::plan::Plan>,
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
        let system_context = crate::state::system_context::SystemContext::detect();
        Self {
            provider,
            tools: Arc::new(RwLock::new(tools)),
            session: Arc::new(RwLock::new(session)),
            config,
            state: RwLock::new(LoopState {
                iteration: 0,
                messages: Vec::new(),
                session_tokens: 0,
            }),
            event_tx,
            system_context,
            user_memory: None,
            workspace_collection: None,
            active_plan: None,
        }
    }

    /// Conecta la memoria del usuario al runtime.
    pub fn with_user_memory(
        mut self,
        user_memory: Arc<RwLock<crate::state::user_memory::UserMemory>>,
    ) -> Self {
        self.user_memory = Some(user_memory);
        self
    }

    /// Conecta la colección del workspace (código indexado con SML).
    ///
    /// Esta colección es la fuente única de verdad técnica — baja entropía.
    /// Se busca aquí para Axiomas y reglas del codebase.
    pub fn with_workspace_collection(
        mut self,
        workspace: Arc<RwLock<dogma_vdb::collection::Collection>>,
    ) -> Self {
        self.workspace_collection = Some(workspace);
        self
    }

    /// Establece el plan activo actual.
    pub fn set_active_plan(&mut self, plan: crate::models::plan::Plan) {
        self.active_plan = Some(plan);
    }

    /// Construye el system prompt dinámico combinando las 4 capas de memoria.
    fn build_system_prompt(&self) -> String {
        let mut prompt = self.config.system_prompt.clone();

        // CAPA 2: System Context (OS, project, git)
        prompt.push('\n');
        prompt.push_str(&self.system_context.to_prompt_section());

        // CAPA 3: User Memory (key-value store)
        if let Some(ref um) = self.user_memory {
            let um_section = um.read().to_prompt_section();
            if !um_section.is_empty() {
                prompt.push('\n');
                prompt.push_str(&um_section);
            }
        }

        prompt
    }

    /// Construye contexto de continuidad: estado actual de ejecución.
    ///
    /// Incluye:
    /// - Último tool result (si existe)
    /// - Último error (si lo hubo)
    /// - Plan activo (si existe)
    fn build_continuity_context(&self, _session_id: &str) -> Option<String> {
        let state = self.state.read();
        let mut continuity = String::new();

        // Último tool result
        if let Some(tr) = state
            .messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Tool && !m.content.is_empty())
        {
            let preview = if tr.content.len() > 200 {
                &tr.content[..200]
            } else {
                &tr.content
            };
            continuity.push_str(&format!("Last tool result: {preview}\n"));
        }

        // Último error
        if let Some(err) = state
            .messages
            .iter()
            .rev()
            .find(|m| m.content.contains("[error]") || m.content.contains("Error:"))
        {
            let preview = if err.content.len() > 200 {
                &err.content[..200]
            } else {
                &err.content
            };
            continuity.push_str(&format!("Last error: {preview}\n"));
        }

        // Plan activo
        if let Some(ref plan) = self.active_plan {
            continuity.push_str(&format!(
                "Active plan: {} ({} steps)\n",
                plan.task,
                plan.steps.len()
            ));
        }

        if continuity.is_empty() {
            None
        } else {
            Some(format!("### EXECUTION STATE:\n{continuity}"))
        }
    }

    /// Busca contexto relevante en DOS colecciones con anti-entropía:
    ///
    /// 1. Session Collection → SOLO mensajes de ESTA sesión + memorias globales
    /// 2. Workspace Collection → Código/SML como fuente única de verdad
    ///
    /// La asimetría previene deriva semántica: workspace es estático/limpio,
    /// sesiones son dinámicas/ruidosas. Solo memorias globales marcadas
    /// explícitamente cruzan la frontera de sesión.
    fn search_relevant(&self, prompt: &str, session_id: &str) -> Vec<SemanticMatch> {
        let session = self.session.read();
        let embedder = match session.embedder() {
            Some(e) => e,
            None => return Vec::new(),
        };

        let embedding = match embedder.embed(prompt) {
            Ok(e) if !e.is_empty() => e,
            _ => return Vec::new(),
        };

        let k = self.config.context_max_relevant * 2;

        // Búsqueda 1: Session Collection → RESTRINGIDA
        // Solo mensajes de ESTA sesión o marcados como "is_global_memory"
        let session_results = session.search_filtered_raw(&embedding, k, &|doc| {
            let same_session = doc.metadata_val("session_id") == Some(session_id);
            let is_global = doc.metadata_val("is_global_memory") == Some("true");
            let is_message = matches!(
                doc.metadata_val("node_type"),
                Some("Message") | Some("ToolResult") | Some("Chunk")
            );
            is_message && (same_session || is_global)
        });

        // Búsqueda 2: Workspace Collection → SML como verdad técnica
        // La Collection de workspace devuelve ScoredDocument, los convertimos
        // a SemanticMatch con session_id="workspace" para distinguirlos.
        let workspace_results: Vec<SemanticMatch> = if let Some(ref ws) = self.workspace_collection
        {
            let ws = ws.read();
            ws.search_filtered(&embedding, k, &|doc| {
                doc.metadata_val("sml").is_some() || !doc.text.is_empty()
            })
            .into_iter()
            .map(|sd| SemanticMatch {
                node_id: sd.document.id,
                content: sd.document.text,
                score: sd.score,
                session_id: "workspace".to_string(),
                created_at: None,
                parent_id: None,
            })
            .collect()
        } else {
            Vec::new()
        };

        // Fusionar + rankear por score
        // session_results ya es Vec<SemanticMatch>, workspace_results también
        let mut all = session_results;
        all.extend(workspace_results);
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.truncate(k);

        all.into_iter()
            .filter(|m| m.score >= self.config.context_relevance_threshold)
            .take(self.config.context_max_relevant)
            .collect()
    }

    /// Formatea contexto relevante con conciencia SML.
    ///
    /// Si un documento es del workspace y tiene metadata["sml"],
    /// inyecta el símbolo denso. Si es de sesión, formatea como referencia.
    fn format_relevant_context(docs: &[SemanticMatch]) -> String {
        if docs.is_empty() {
            return String::new();
        }

        let mut ctx = String::from("### RELEVANT CONTEXT:\n\n");
        for (i, m) in docs.iter().enumerate() {
            let is_workspace = m.session_id == "workspace";

            if is_workspace {
                ctx.push_str(&format!(
                    "[{i}] (source: {}, score: {:.2})\n",
                    m.node_id, m.score
                ));
                ctx.push_str(&m.content);
                ctx.push_str("\n\n");
            } else {
                let preview_len = m.content.len().min(300);
                ctx.push_str(&format!(
                    "[{i}] (session: {}, score: {:.2})\n{}\n\n",
                    &m.session_id[..m.session_id.len().min(20)],
                    m.score,
                    &m.content[..preview_len],
                ));
            }
        }
        ctx
    }

    /// Construye el contexto inteligente con dimensiones:
    ///
    /// 1. IDENTITY — System prompt + context + user memory
    /// 2. KNOWLEDGE — Relevant context (dual search anti-entropía)
    /// 3. MEMORY — Recent turns (sliding window seguro)
    /// 4. USER INPUT — Prompt actual
    ///
    /// NOTA: Continuity context se maneja por separado en `run()`
    /// para evitar deadlock con self.state.
    fn build_intelligent_context(&self, prompt: &str, session_id: &str) -> Vec<Message> {
        let mut messages = Vec::new();

        // [1] IDENTITY
        messages.push(Message::new(
            MessageRole::System,
            self.build_system_prompt(),
        ));

        // [2] KNOWLEDGE — Relevant context
        if self.config.context_management {
            let relevant = self.search_relevant(prompt, session_id);
            if !relevant.is_empty() {
                let ctx = Self::format_relevant_context(&relevant);
                messages.push(Message::new(MessageRole::System, &ctx));
            }
        }

        // [3] MEMORY — Recent turns (sliding window)
        let history = self.load_session_history_inline(session_id);
        let recent = extract_recent_turns(&history, self.config.context_recent_turns);
        messages.extend(recent.iter().cloned());

        // [4] USER INPUT
        messages.push(Message::new(MessageRole::User, prompt));

        messages
    }

    /// Versión inline de load_session_history para usar dentro del runtime.
    fn load_session_history_inline(&self, session_id: &str) -> Vec<Message> {
        let session = self.session.read();
        load_session_history(&session, session_id)
    }

    /// Ejecuta el ciclo RSI con un prompt de entrada.
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Fatal` si hay un fallo de I/O al persistir el
    /// estado. Devuelve `Error::Infrastructure` si el provider falla.
    pub async fn run(&self, prompt: &str, session_id: &str) -> Result<String> {
        info!("Runtime loop starting for session {}", session_id);

        // Compute continuity BEFORE state lock (reads self.state)
        let continuity_ctx = self.build_continuity_context(session_id);

        // Construir contexto inteligente: 5 dimensiones (Identity, Knowledge,
        // Memory, Continuity, User Input) con anti-entropía y conciencia SML.
        {
            let mut state = self.state.write();
            state.iteration = 0;
            state.messages = self.build_intelligent_context(prompt, session_id);

            // Inject continuity context before user input (already computed)
            if let Some(ctx) = continuity_ctx {
                // Insertar antes del último mensaje (user input)
                let insert_pos = state.messages.len().saturating_sub(1);
                state
                    .messages
                    .insert(insert_pos, Message::new(MessageRole::System, &ctx));
            }
        }

        debug!(
            "Context loaded: {} previous messages + new prompt",
            self.state.read().messages.len() - 1
        );

        // Persist user message in session
        {
            let mut session = self.session.write();
            session.append_message(session_id, MessageRole::User, prompt, &[])?;
            let _ = session.embed_pending_messages(session_id);
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
            let _ = session.embed_pending_messages(session_id);
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

            // Acumular tokens de sesión y emitir StatusUpdate con el uso
            // real de contexto (fracción 0.0–1.0 de la ventana del modelo).
            if let Some(ref tx) = self.event_tx {
                let session_tokens = {
                    let mut state = self.state.write();
                    state.session_tokens = state
                        .session_tokens
                        .saturating_add(u64::from(response.usage.total_tokens));
                    state.session_tokens
                };
                let window = self.config.context_window.max(1);
                let context_used = if response.usage.prompt_tokens > 0 {
                    (f64::from(response.usage.prompt_tokens) / f64::from(window)).min(1.0) as f32
                } else {
                    0.0
                };
                let model = self.provider.config().model.clone();
                let _ = tx.try_send(AgentEvent::StatusUpdate {
                    context_used,
                    total_tokens: session_tokens,
                    model,
                });
            }

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
