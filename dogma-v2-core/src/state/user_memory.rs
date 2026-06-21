//! # User Memory — Key-value store persistente
//!
//! Almacena preferencias, hábitos y datos del usuario en dogma-vdb.
//! El agente puede leer y escribir esta memoria via tools.
//!
//! ## Formato en dogma-vdb
//!
//! Cada entrada es un documento con:
//! - `id`: la key (ej: "temp_dir")
//! - `text`: el valor (ej: "/tmp/my_project")
//! - `metadata`: categoría ("system", "preference", "knowledge")

use dogma_v2_common::Result;
use dogma_vdb::collection::Collection;
use dogma_vdb::doc::Document;
use std::path::Path;
use tracing::info;

/// Convierte un error de dogma-vdb a un error de dogma-v2-common.
fn vdb_err(e: dogma_vdb::error::Error) -> dogma_v2_common::Error {
    dogma_v2_common::Error::Internal(format!("dogma-vdb: {e}"))
}

/// Key-value store persistente para datos del usuario.
///
/// Cada entrada se almacena como un documento en dogma-vdb con:
/// - `id` = key
/// - `text` = value
/// - `metadata.category` = "system" | "preference" | "knowledge"
pub struct UserMemory {
    collection: Collection,
}

impl UserMemory {
    /// Abre o crea la colección de user memory.
    pub fn open(path: &Path) -> Result<Self> {
        let collection = Collection::open(path).map_err(vdb_err)?;
        Ok(Self { collection })
    }

    /// Lee un valor por key.
    pub fn get(&self, key: &str) -> Option<String> {
        self.collection
            .documents()
            .find(|d| d.id == key)
            .map(|d| d.text.clone())
    }

    /// Guarda o actualiza un valor (upsert).
    pub fn set(&mut self, key: &str, value: &str, category: &str) -> Result<()> {
        // Verificar si ya existe
        let existing = self.get(key);

        if existing.is_some() {
            // Actualizar: eliminar el viejo y crear el nuevo
            self.collection.delete(&[key]).map_err(vdb_err)?;
        }

        let doc = Document::builder(key, value)
            .metadata("category", category)
            .build();

        self.collection.insert(doc).map_err(vdb_err)?;
        info!("User memory: set '{key}' = '{value}' (category: {category})");
        Ok(())
    }

    /// Elimina un valor por key.
    ///
    /// Retorna `true` si el key existía.
    pub fn remove(&mut self, key: &str) -> Result<bool> {
        let deleted = self.collection.delete(&[key]).map_err(vdb_err)?;
        if deleted > 0 {
            info!("User memory: removed '{key}'");
        }
        Ok(deleted > 0)
    }

    /// Lista todas las keys con sus valores.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        self.collection
            .documents()
            .map(|d| {
                let category = d
                    .metadata_val("category")
                    .unwrap_or("unknown")
                    .to_string();
                (d.id.clone(), d.text.clone(), category)
            })
            .collect()
    }

    /// Lista solo las keys.
    pub fn keys(&self) -> Vec<String> {
        self.collection.documents().map(|d| d.id.clone()).collect()
    }

    /// Busca valores por contenido (búsqueda simple por sub-string).
    ///
    /// Para búsqueda semántica, usar `search_similar` del SessionManager.
    pub fn search(&self, query: &str) -> Vec<(String, String)> {
        let query_lower = query.to_lowercase();
        self.collection
            .documents()
            .filter(|d| {
                d.id.to_lowercase().contains(&query_lower)
                    || d.text.to_lowercase().contains(&query_lower)
            })
            .map(|d| (d.id.clone(), d.text.clone()))
            .collect()
    }

    /// Número de entradas almacenadas.
    pub fn len(&self) -> usize {
        self.collection.len()
    }

    /// Retorna `true` si no hay entradas.
    pub fn is_empty(&self) -> bool {
        self.collection.is_empty()
    }

    /// Genera una sección de texto para inyectar en el system prompt.
    ///
    /// Solo incluye las primeras 10 entradas para no saturar el contexto.
    pub fn to_prompt_section(&self) -> String {
        let entries = self.entries();
        if entries.is_empty() {
            return String::new();
        }

        let mut section = String::from("USER MEMORY:\n");
        for (key, value, category) in entries.iter().take(10) {
            section.push_str(&format!("  - {key}: {value} [{category}]\n"));
        }
        if entries.len() > 10 {
            section.push_str(&format!(
                "  ... and {} more entries\n",
                entries.len() - 10
            ));
        }
        section
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_user_memory() -> (tempfile::TempDir, UserMemory) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("user_memory.vdb");
        let mem = UserMemory::open(&path).unwrap();
        (dir, mem)
    }

    #[test]
    fn test_set_and_get() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("temp_dir", "/tmp/work", "system").unwrap();
        assert_eq!(mem.get("temp_dir"), Some("/tmp/work".into()));
    }

    #[test]
    fn test_upsert() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("key", "value1", "system").unwrap();
        mem.set("key", "value2", "system").unwrap();
        assert_eq!(mem.get("key"), Some("value2".into()));
        assert_eq!(mem.len(), 1);
    }

    #[test]
    fn test_remove() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("key", "value", "system").unwrap();
        assert!(mem.remove("key").unwrap());
        assert!(mem.get("key").is_none());
        assert!(!mem.remove("key").unwrap()); // already removed
    }

    #[test]
    fn test_entries() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("a", "1", "system").unwrap();
        mem.set("b", "2", "preference").unwrap();
        let entries = mem.entries();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_keys() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("x", "1", "system").unwrap();
        mem.set("y", "2", "system").unwrap();
        let mut keys = mem.keys();
        keys.sort();
        assert_eq!(keys, vec!["x", "y"]);
    }

    #[test]
    fn test_search() {
        let (_dir, mut mem) = make_user_memory();
        mem.set("temp_dir", "/tmp/work", "system").unwrap();
        mem.set("editor", "vscode", "preference").unwrap();

        let results = mem.search("tmp");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "temp_dir");
    }

    #[test]
    fn test_to_prompt_section() {
        let (_dir, mut mem) = make_user_memory();
        assert!(mem.to_prompt_section().is_empty());

        mem.set("key", "value", "system").unwrap();
        let section = mem.to_prompt_section();
        assert!(section.contains("USER MEMORY"));
        assert!(section.contains("key: value"));
    }

    #[test]
    fn test_len_and_is_empty() {
        let (_dir, mut mem) = make_user_memory();
        assert!(mem.is_empty());
        assert_eq!(mem.len(), 0);

        mem.set("a", "1", "system").unwrap();
        assert!(!mem.is_empty());
        assert_eq!(mem.len(), 1);
    }
}
