//! # State — Session Manager, System Context, User Memory y adaptadores sobre dogma-vdb
//!
//! Gestiona todo el estado del agente en 4 capas de memoria:
//!
//! 1. **Session Context** — historial de conversación en `sessions.vdb`
//! 2. **User Memory** — key-value persistente en `user_memory.vdb`
//! 3. **System Context** — OS, project, git (auto-detectado)
//! 4. **Context Manager** — selección semántica de contexto relevante

pub mod compressor;
pub mod context_manager;
pub mod session;
pub mod system_context;
pub mod user_memory;
