//! # Runtime — Loop principal de IA y trait de proveedores LLM
//!
//! El runtime expone un trait genérico `LLMProvider` para proveedores
//! OpenAI-Compatibles, eliminando implementaciones hardcodeadas por
//! proveedor. El `RuntimeLoop` orquesta el ciclo RSI:
//!
//! 1. Recibe un prompt del usuario.
//! 2. Lo envía al LLM vía el provider activo.
//! 3. Inspecciona la respuesta en busca de tool calls.
//! 4. Ejecuta las herramientas y realimenta el resultado al LLM.
//! 5. Repite hasta que el LLM responde con un mensaje final.

pub mod loop_handle;
pub mod provider;
pub mod sub_agent;
pub mod wasm_sandbox;
