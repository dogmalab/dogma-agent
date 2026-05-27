//! # dogma-v2-core — Runtime asíncrono, Tool Loop, State Management
//!
//! Este crate implementa el núcleo del agente Dogma 2.0:
//!
//! * **Runtime** — Loop principal de IA (RSI) con trait para proveedores
//!   LLM OpenAI-Compatibles dinámicos.
//! * **Tools** — Las 3 herramientas de supervivencia:
//!   `read_file`, `write_file`, `execute_script`.
//! * **State** — Session Manager y adaptadores sobre `dogma-vdb` para
//!   almacenar todo el estado como nodos de un grafo vectorial.

pub mod runtime;
pub mod state;
pub mod tools;

pub use runtime::provider::LLMProvider;
pub use runtime::loop_handle::RuntimeLoop;
pub use state::session::SessionManager;
pub use state::compressor::Compressor;
pub use tools::{Tool, ToolResult};
