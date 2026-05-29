//! # Compressor — Compresión determinista y semántica
//!
//! ## Compresión determinista
//!
//! Analiza las aristas de la sesión en dogma-vdb y poda payloads de
//! herramientas masivas, reemplazándolos con resúmenes estructurales:
//!
//! * Tool results > 500 chars → `[Tool output: 2,304 bytes, exit 0]`
//! * Múltiples tool calls seguidas → resumen agregado.
//!
//! ## Compresión semántica
//!
//! Realiza búsquedas de similitud de coseno (vía mmap de dogma-vdb)
//! para re-inyectar contexto antiguo relevante bajo demanda en
//! menos de 5 ms.

use crate::state::session::SessionManager;
use dogma_v2_common::Result;
use dogma_vdb::doc::Document;
use tracing::debug;

/// Umbral para considerar un payload como "masivo".
const LARGE_PAYLOAD_THRESHOLD: usize = 500;

/// Número máximo de tool results consecutivos antes de resumir.
const MAX_CONSECUTIVE_TOOLS: usize = 3;

/// El compresor de contexto.
pub struct Compressor {
    #[allow(dead_code)]
    session: SessionManager,
}

impl Compressor {
    /// Crea un nuevo compresor.
    pub fn new(session: SessionManager) -> Self {
        Self { session }
    }

    /// Comprime los nodos de una sesión de forma determinista.
    ///
    /// Reemplaza payloads grandes con resúmenes estructurales y
    /// acorta secuencias largas de tool calls.
    ///
    /// # Errors
    ///
    /// Devuelve error si no se puede leer la sesión.
    pub fn compress_deterministic(
        &self,
        _session_id: &str,
        nodes: &[Document],
    ) -> Result<Vec<CompressedNode>> {
        let mut compressed: Vec<CompressedNode> = Vec::with_capacity(nodes.len());
        let mut tool_run: Vec<&Document> = Vec::new();

        for node in nodes {
            let node_type = node.metadata_val("node_type").unwrap_or("");

            if node_type == "ToolResult" {
                tool_run.push(node);
                if tool_run.len() >= MAX_CONSECUTIVE_TOOLS {
                    compressed.push(self.summarize_tool_run(&tool_run));
                    tool_run.clear();
                }
                continue;
            }

            // Flush pending tool run
            if !tool_run.is_empty() {
                compressed.push(self.summarize_tool_run(&tool_run));
                tool_run.clear();
            }

            if node_type == "Message" && node.text.len() > LARGE_PAYLOAD_THRESHOLD {
                compressed.push(CompressedNode {
                    node_id: node.id.clone(),
                    summary: format!(
                        "[Large message: {} bytes, starting with: {}]",
                        node.text.len(),
                        truncate(&node.text, 100)
                    ),
                    original_size: node.text.len(),
                });
            } else {
                compressed.push(CompressedNode {
                    node_id: node.id.clone(),
                    summary: node.text.clone(),
                    original_size: node.text.len(),
                });
            }
        }

        // Flush remaining tool run
        if !tool_run.is_empty() {
            compressed.push(self.summarize_tool_run(&tool_run));
        }

        debug!(
            "Deterministic compression: {} → {} nodes",
            nodes.len(),
            compressed.len()
        );
        Ok(compressed)
    }

    /// Busca contexto semánticamente relevante de sesiones anteriores.
    ///
    /// # Errors
    ///
    /// Devuelve error si la búsqueda vectorial falla.
    pub async fn search_semantic(&self, _query: &str, _limit: usize) -> Result<Vec<SemanticMatch>> {
        // FIXME: Requiere integración con el embedder de dogma-vdb.
        // Por ahora es un placeholder que devuelve resultados vacíos.
        //
        // La implementación real hará:
        // 1. Generar embedding del query via Embedder
        // 2. Collection::search(embedding, limit) para encontrar
        //    nodos similares
        // 3. Devolver los textos originales como contexto

        debug!("Semantic search requested but embedder not yet connected");
        Ok(Vec::new())
    }

    /// Genera un resumen para un grupo de tool calls consecutivas.
    fn summarize_tool_run(&self, tools: &[&Document]) -> CompressedNode {
        let total_bytes: usize = tools.iter().map(|t| t.text.len()).sum();
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.metadata_val("tool_name"))
            .collect();

        CompressedNode {
            node_id: tools[0].id.clone(),
            summary: format!(
                "[Tool run: {} tools ({}), total {} bytes]",
                tools.len(),
                tool_names.join(", "),
                total_bytes
            ),
            original_size: total_bytes,
        }
    }
}

/// Un nodo después de la compresión determinista.
#[derive(Debug, Clone)]
pub struct CompressedNode {
    /// ID del nodo original.
    pub node_id: String,
    /// Versión comprimida/resumida del contenido.
    pub summary: String,
    /// Tamaño original en bytes antes de comprimir.
    pub original_size: usize,
}

/// Un resultado de búsqueda semántica.
#[derive(Debug, Clone)]
pub struct SemanticMatch {
    /// ID del nodo encontrado.
    pub node_id: String,
    /// Texto original del nodo.
    pub content: String,
    /// Puntaje de similitud (0.0 - 1.0).
    pub score: f32,
    /// Sesión de origen.
    pub session_id: String,
}

/// Trunca un string a un máximo de caracteres, añadiendo "..." si
/// es necesario.
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        format!("{}...", &s[..max_chars])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dogma_vdb::doc::Document;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_long() {
        let long = "a".repeat(1000);
        let result = truncate(&long, 10);
        assert_eq!(result.len(), 13); // 10 + "..."
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_compress_small_messages() {
        let session = temp_session_manager();
        let compressor = Compressor::new(session);

        let docs = vec![
            Document::new("msg-1", "hello"),
            Document::new("msg-2", "world"),
        ];

        let result = compressor
            .compress_deterministic("session-1", &docs)
            .expect("compress");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].summary, "hello");
        assert_eq!(result[1].summary, "world");
    }

    #[test]
    fn test_compress_large_message() {
        let session = temp_session_manager();
        let compressor = Compressor::new(session);

        let large_text = "x".repeat(1000);
        let docs = vec![
            Document::builder("msg-1", &large_text)
                .metadata("node_type", "Message")
                .build(),
        ];

        let result = compressor
            .compress_deterministic("session-1", &docs)
            .expect("compress");
        assert_eq!(result.len(), 1);
        assert!(result[0].summary.contains("Large message"));
        assert!(result[0].summary.contains("1000"));
    }

    fn temp_session_manager() -> SessionManager {
        let dir = std::env::temp_dir().join(format!("dogma-test-{}", uuid::Uuid::new_v4()));
        SessionManager::open(dir).expect("open session manager")
    }
}
