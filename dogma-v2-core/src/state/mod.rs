//! # State — Session Manager y adaptadores sobre dogma-vdb
//!
//! Gestiona todo el estado del agente como nodos de un grafo vectorial
//! dentro de colecciones `dogma-vdb`.
//!
//! ## Modelo de grafos
//!
//! * Cada **sesión** es un nodo raíz `Session` con metadatos (timestamp,
//!   modelo, configuración).
//! * Cada **interacción** (mensaje) es un nodo `Message` apuntado por
//!   una arista `NEXT` desde el nodo anterior.
//! * Cada **ejecución de herramienta** es un nodo conectado por aristas
//!   `TRIGGERED` desde el mensaje que la originó.
//!
//! ## Compresor
//!
//! El compresor analiza las aristas de la sesión para:
//! * **Determinista**: Podar payloads de herramientas masivas,
//!   reemplazándolos con resúmenes estructurales.
//! * **Semántico**: Búsqueda de similitud de coseno (vía `dogma-vdb`)
//!   para re-inyectar contexto antiguo relevante bajo demanda.

pub mod compressor;
pub mod context_manager;
pub mod session;
