//! # UI — Módulo de interfaz de terminal
//!
//! Arquitectura modular:
//! - `renderer` — orquestador principal (init, draw, events)
//! - `chat` — renderizado del chat + markdown
//! - `spinner` — rune cycle animation
//! - `status_bar` — barra de estado inferior
//! - `tools` — display de tool calls
//! - `markdown` — parser de markdown
//! - `input` — captura de teclado
//! - `resize` — manejo de SIGWINCH

mod chat;
mod input;
pub(crate) mod markdown;
mod render;
mod resize;
mod spinner;
mod status_bar;
mod tools;

pub use chat::ChatRenderer;
pub use input::{InputEvent, spawn_input_reader};
pub use render::Renderer;
pub use spinner::Spinner;
pub use status_bar::StatusBar;
pub use tools::ToolDisplay;
