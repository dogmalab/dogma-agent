//! # UI — Módulo de interfaz de terminal

mod input;
mod render;

pub use input::{InputEvent, spawn_input_reader};
pub use render::Renderer;
