//! # Session Manager — Estado como grafos vectoriales
//!
//! Cada sesión se modela como una colección de nodos dentro de un
//! archivo `.vdb`. Las relaciones entre nodos (NEXT, TRIGGERED) se
//! codifican en los metadatos de cada documento.
//!
//! ## Estructura de un nodo
//!
//! ```json
//! {
//!   "id": "session-<uuid>",
//!   "text": "User message or tool result",
//!   "metadata": {
//!     "node_type": "Session | Message | ToolResult",
//!     "role": "user | assistant | tool",
//!     "sequence": "0",
//!     "parent_id": "...",
//!     "edge_type": "NEXT | TRIGGERED",
//!     "session_id": "...",
//!     "created_at": "ISO-8601"
//!   }
//! }
//! ```

use crate::runtime::provider::MessageRole;
use dogma_v2_common::Result;
use dogma_vdb::collection::Collection;
use dogma_vdb::doc::Document;
use dogma_vdb::embedding::Embedder;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Gestiona las sesiones del agente como nodos en dogma-vdb.
pub struct SessionManager {
    /// Colección vdb que almacena todos los nodos de sesión.
    collection: Collection,
    /// Directorio base para los archivos .vdb.
    #[allow(dead_code)]
    base_path: PathBuf,
    /// Embedder opcional para búsqueda semántica.
    /// Si no está configurado, `search_similar()` devuelve vacío.
    embedder: Option<Arc<dyn Embedder>>,
}

impl SessionManager {
    /// Abre (o crea) un gestor de sesiones.
    ///
    /// El archivo `.vdb` se crea en `base_path / sessions.vdb`.
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Io` si no se puede abrir o crear el archivo.
    pub fn open(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path: PathBuf = base_path.into();
        std::fs::create_dir_all(&base_path).map_err(|e| dogma_v2_common::error::Error::Io {
            path: base_path.clone(),
            source: e,
        })?;

        let vdb_path = base_path.join("sessions.vdb");
        let collection =
            Collection::open(&vdb_path).map_err(|e| dogma_v2_common::error::Error::Io {
                path: vdb_path,
                source: std::io::Error::other(e.to_string()),
            })?;

        info!("SessionManager opened at {}", base_path.display());
        Ok(Self {
            collection,
            base_path,
            embedder: None,
        })
    }

    /// Conecta un embedder para habilitar búsqueda semántica.
    #[must_use]
    pub fn with_embedder(mut self, embedder: Arc<dyn Embedder>) -> Self {
        self.embedder = Some(embedder);
        self
    }

    /// Crea una nueva sesión y devuelve su ID.
    ///
    /// # Errors
    ///
    /// Devuelve error de I/O si no se puede persistir el nodo raíz.
    pub fn create_session(&mut self, model: &str) -> Result<String> {
        let session_id = format!("session-{}", uuid::Uuid::new_v4());

        let doc = Document::builder(&session_id, format!("Session: {model}"))
            .metadata("node_type", "Session")
            .metadata("session_id", &session_id)
            .metadata("model", model)
            .metadata("sequence", "0")
            .metadata("created_at", chrono::Utc::now().to_rfc3339())
            .build();

        self.collection
            .insert(doc)
            .map_err(|e| dogma_v2_common::error::Error::StorageCorrupted(e.to_string()))?;

        debug!("Created session {session_id}");
        Ok(session_id)
    }

    /// Añade un mensaje a la sesión.
    ///
    /// # Errors
    ///
    /// Devuelve error de I/O si no se puede persistir.
    pub fn append_message(
        &mut self,
        session_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<String> {
        let node_id = format!("msg-{}", uuid::Uuid::new_v4());
        let seq = self.next_sequence(session_id)?;

        let role_str = match role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };

        let doc = Document::builder(&node_id, content)
            .metadata("node_type", "Message")
            .metadata("session_id", session_id)
            .metadata("role", role_str)
            .metadata("sequence", seq.to_string())
            .metadata("edge_type", "NEXT")
            .metadata("created_at", chrono::Utc::now().to_rfc3339())
            .build();

        self.collection
            .insert(doc)
            .map_err(|e| dogma_v2_common::error::Error::StorageCorrupted(e.to_string()))?;

        debug!("Appended message {node_id} to session {session_id}");
        Ok(node_id)
    }

    /// Añade el resultado de una herramienta a la sesión.
    ///
    /// # Errors
    ///
    /// Devuelve error de I/O si no se puede persistir.
    pub fn append_tool_result(
        &mut self,
        session_id: &str,
        tool_name: &str,
        tool_call_id: &str,
        result: &str,
    ) -> Result<String> {
        let node_id = format!("tool-{}", uuid::Uuid::new_v4());
        let seq = self.next_sequence(session_id)?;

        let doc = Document::builder(&node_id, result)
            .metadata("node_type", "ToolResult")
            .metadata("session_id", session_id)
            .metadata("tool_name", tool_name)
            .metadata("tool_call_id", tool_call_id)
            .metadata("sequence", seq.to_string())
            .metadata("edge_type", "TRIGGERED")
            .metadata("created_at", chrono::Utc::now().to_rfc3339())
            .build();

        self.collection
            .insert(doc)
            .map_err(|e| dogma_v2_common::error::Error::StorageCorrupted(e.to_string()))?;

        debug!("Appended tool result {node_id} to session {session_id}");
        Ok(node_id)
    }

    /// Recupera todos los nodos de una sesión ordenados por secuencia.
    ///
    /// # Errors
    ///
    /// Devuelve error si la colección no se puede leer.
    pub fn get_session_nodes(&self, session_id: &str) -> Result<Vec<Document>> {
        // FIXME: dogma-vdb no tiene un método de consulta por metadatos
        // aún. Esta es la firma preparada para cuando esté disponible.
        // Por ahora devolvemos una lista vacía.
        let _ = session_id;
        warn!("get_session_nodes: metadata filtering not yet implemented in dogma-vdb");
        Ok(Vec::new())
    }

    /// Devuelve el número de nodos en una sesión.
    pub fn session_node_count(&self, session_id: &str) -> Result<usize> {
        Ok(self.get_session_nodes(session_id)?.len())
    }

    /// Calcula el siguiente número de secuencia para una sesión.
    fn next_sequence(&self, session_id: &str) -> Result<u64> {
        let nodes = self.get_session_nodes(session_id)?;
        Ok(nodes.len() as u64)
    }

    /// Devuelve una referencia a la colección subyacente.
    pub fn collection(&self) -> &Collection {
        &self.collection
    }

    /// Busca contexto semánticamente similar en el historial de la sesión.
    ///
    /// Usa el embedder configurado para convertir `query` en vector,
    /// luego busca en dogma-vdb filtrando por `session_id`.
    /// Si no hay embedder, devuelve una lista vacía sin error.
    pub fn search_similar(
        &self,
        query: &str,
        session_id: &str,
        k: usize,
    ) -> Result<Vec<super::compressor::SemanticMatch>> {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => {
                debug!("Semantic search requested but no embedder configured");
                return Ok(Vec::new());
            }
        };

        let embedding = embedder.embed(query).map_err(|e| {
            dogma_v2_common::error::Error::Internal(format!("embedding failed: {e}"))
        })?;

        if embedding.is_empty() {
            debug!("Embedder returned empty vector — skipping search");
            return Ok(Vec::new());
        }

        let results = self
            .collection
            .search_filtered(&embedding, k, &|doc: &Document| -> bool {
                doc.metadata_val("session_id") == Some(session_id)
                    && matches!(
                        doc.metadata_val("node_type"),
                        Some("Message") | Some("ToolResult") | Some("Chunk")
                    )
            });

        Ok(results
            .into_iter()
            .map(|sd| {
                let created_at = sd.document.metadata_val("created_at").map(String::from);
                let parent_id = sd.document.metadata_val("parent_id").map(String::from);
                super::compressor::SemanticMatch {
                    node_id: sd.document.id,
                    content: sd.document.text,
                    score: sd.score,
                    session_id: session_id.to_string(),
                    created_at,
                    parent_id,
                }
            })
            .collect())
    }

    /// Busca contexto semánticamente similar en TODAS las sesiones
    /// (sin filtrar por `session_id`).
    ///
    /// Útil para la herramienta `search_memory` cuando el LLM quiere
    /// recuperar información de cualquier sesión pasada.
    pub fn search_similar_global(
        &self,
        query: &str,
        k: usize,
    ) -> Result<Vec<super::compressor::SemanticMatch>> {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => {
                debug!("Semantic search requested but no embedder configured");
                return Ok(Vec::new());
            }
        };

        let embedding = embedder.embed(query).map_err(|e| {
            dogma_v2_common::error::Error::Internal(format!("embedding failed: {e}"))
        })?;

        if embedding.is_empty() {
            debug!("Embedder returned empty vector — skipping search");
            return Ok(Vec::new());
        }

        let results = self
            .collection
            .search_filtered(&embedding, k, &|doc: &Document| -> bool {
                matches!(
                    doc.metadata_val("node_type"),
                    Some("Message") | Some("ToolResult") | Some("Chunk")
                )
            });

        Ok(results
            .into_iter()
            .map(|sd| {
                let session_id = sd.document.metadata_val("session_id").unwrap_or("").to_string();
                let created_at = sd.document.metadata_val("created_at").map(String::from);
                let parent_id = sd.document.metadata_val("parent_id").map(String::from);
                super::compressor::SemanticMatch {
                    node_id: sd.document.id,
                    content: sd.document.text,
                    score: sd.score,
                    session_id,
                    created_at,
                    parent_id,
                }
            })
            .collect())
    }

    /// Genera embeddings para mensajes que aún no los tienen.
    ///
    /// Escanea la colección en busca de documentos de la sesión sin
    /// embedding, los embeddea en batch, y actualiza cada documento.
    /// Devuelve la cantidad de documentos embeddeados.
    pub fn embed_pending_messages(&mut self, session_id: &str) -> Result<usize> {
        let embedder = match &self.embedder {
            Some(e) => e,
            None => {
                debug!("Embed requested but no embedder configured");
                return Ok(0);
            }
        };

        // Recoger documentos sin embedding
        let pending: Vec<Document> = self
            .collection
            .documents()
            .filter(|d| {
                d.metadata_val("session_id") == Some(session_id)
                    && !d.is_embedded()
            })
            .cloned()
            .collect();

        if pending.is_empty() {
            return Ok(0);
        }

        let texts: Vec<&str> = pending.iter().map(|d| d.text.as_str()).collect();
        let embeddings = embedder.embed_batch(&texts).map_err(|e| {
            dogma_v2_common::error::Error::Internal(format!("batch embedding failed: {e}"))
        })?;

        let embed_count = embeddings.len();

        for (doc, emb) in pending.into_iter().zip(embeddings) {
            let updated = Document::builder(&doc.id, &doc.text)
                .embedding(emb)
                .metadatas(doc.metadata.clone())
                .build();
            // update = delete + insert (atomic en Collection)
            self.collection
                .update(updated)
                .map_err(|e| dogma_v2_common::error::Error::StorageCorrupted(e.to_string()))?;
        }

        debug!("Embedded {} pending messages", embed_count);
        Ok(embed_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_session() {
        let dir = tempdir().expect("temp dir");
        let mut manager = SessionManager::open(dir.path()).expect("open session manager");
        let session_id = manager
            .create_session("test-model")
            .expect("create session");
        assert!(session_id.starts_with("session-"));
    }

    #[test]
    fn test_append_message() {
        let dir = tempdir().expect("temp dir");
        let mut manager = SessionManager::open(dir.path()).expect("open session manager");
        let session_id = manager
            .create_session("test-model")
            .expect("create session");
        let msg_id = manager
            .append_message(&session_id, MessageRole::User, "hello")
            .expect("append message");
        assert!(msg_id.starts_with("msg-"));
    }
}
