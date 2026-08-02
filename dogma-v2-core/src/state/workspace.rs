//! # Workspace — Indexación del código fuente con SML
//!
//! Indexa el workspace actual en una colección dogma-vdb usando
//! `SmartChunker` + `SmlCompiler`. Cada chunk se convierte en un
//! documento con metadata `sml` (SIMIL) y su embedding, listo para
//! búsqueda semántica por el `RuntimeLoop`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dogma_vdb::collection::Collection;
use dogma_vdb::doc::Document;
use dogma_vdb::embedding::Embedder;
use dogma_vdb::smart_chunker::{ChunkStrategy, SmartChunker};
use dogma_vdb::sml::{SmlCompiler, serialize};
use tracing::{debug, info, warn};

use crate::state::session::collection_config;

/// Directorios que nunca se indexan.
const IGNORED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".cache",
    "dist",
    "build",
    "vendor",
    ".idea",
    ".vscode",
    "venv",
    ".venv",
    ".dogma",
];

/// Extensiones de archivo que se indexan.
const INDEXED_EXTS: &[&str] = &[
    "rs", "py", "js", "ts", "jsx", "tsx", "go", "md", "toml", "json", "yaml", "yml", "sh", "txt",
    "html", "css", "c", "h", "cpp", "hpp",
];

/// Límite de tamaño por archivo (512 KB) para no indexar binarios ni gigantes.
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Indexa archivos de un directorio en una colección con embeddings + SML.
pub struct WorkspaceIndexer {
    chunker: SmartChunker,
    compiler: SmlCompiler,
    embedder: Arc<dyn Embedder>,
}

impl WorkspaceIndexer {
    /// Crea un indexador con el embedder dado.
    pub fn new(embedder: Arc<dyn Embedder>) -> Self {
        Self {
            chunker: SmartChunker::default(),
            compiler: SmlCompiler::new(),
            embedder,
        }
    }

    /// Indexa `root` recursivamente. Devuelve el nº de documentos insertados.
    pub fn index_dir(&self, root: &Path, collection: &mut Collection) -> usize {
        let mut files = Vec::new();
        self.collect_files(root, &mut files);
        debug!(
            "Workspace index: {} candidate files under {}",
            files.len(),
            root.display()
        );

        let mut total = 0;
        for path in &files {
            total += self.index_file(path, collection);
        }
        info!(
            "Workspace indexed: {total} chunks from {} files",
            files.len()
        );
        total
    }

    /// Recorre el árbol de directorios recogiendo archivos indexables.
    fn collect_files(&self, dir: &Path, out: &mut Vec<PathBuf>) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                warn!("Cannot read dir {}: {e}", dir.display());
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if IGNORED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                self.collect_files(&path, out);
            } else if path.is_file() {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !INDEXED_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                if fs::metadata(&path)
                    .map(|m| m.len() > MAX_FILE_BYTES)
                    .unwrap_or(true)
                {
                    continue;
                }
                out.push(path);
            }
        }
    }

    /// Indexa un único archivo: chunk → SML → embeddings → insert.
    fn index_file(&self, path: &Path, collection: &mut Collection) -> usize {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                warn!("Cannot read {}: {e}", path.display());
                return 0;
            }
        };

        let strategy = ChunkStrategy::from_path(path);
        let chunks = self.chunker.chunk_text(&text, strategy);
        if chunks.is_empty() {
            return 0;
        }

        let nodes = self.compiler.compile_batch(&chunks, &text);
        let sml_per_chunk: Vec<String> = nodes.iter().map(serialize).collect();

        let rel = path.to_string_lossy().to_string();
        let base_id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");

        let mut docs = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            let mut meta = HashMap::new();
            meta.insert("source".to_string(), rel.clone());
            meta.insert("language".to_string(), strategy.name().to_string());
            if let Some(ref s) = chunk.structure {
                meta.insert("structure".to_string(), s.clone());
            }
            meta.insert("level".to_string(), chunk.level.to_string());
            meta.insert("start_line".to_string(), chunk.start_line.to_string());
            meta.insert("end_line".to_string(), chunk.end_line.to_string());
            meta.insert("node_type".to_string(), "Chunk".to_string());
            meta.insert("sml".to_string(), sml_per_chunk[i].clone());

            docs.push(
                Document::builder(format!("{base_id}-{i}"), chunk.text.clone())
                    .metadatas(meta)
                    .build(),
            );
        }

        let texts: Vec<&str> = docs.iter().map(|d| d.text.as_str()).collect();
        let embeddings = match self.embedder.embed_batch(&texts) {
            Ok(e) => e,
            Err(e) => {
                warn!("Embedding failed for {}: {e}", path.display());
                return 0;
            }
        };

        let embedded: Vec<Document> = docs
            .into_iter()
            .zip(embeddings)
            .map(|(doc, emb)| {
                Document::builder(&doc.id, &doc.text)
                    .embedding(emb)
                    .metadatas(doc.metadata)
                    .build()
            })
            .collect();

        match collection.insert_batch(&embedded) {
            Ok(_) => embedded.len(),
            Err(e) => {
                warn!("Insert failed for {}: {e}", path.display());
                0
            }
        }
    }
}

/// Abre (o crea) la colección del workspace en `base_path / workspace.vdb`.
///
/// Usa la misma configuración que las sesiones (HNSW + cosine) para
/// mantener dimensiones y métrica consistentes.
///
/// # Errors
///
/// Devuelve `Error::Io` si no se puede abrir o crear el archivo.
pub fn open_workspace_collection(base_path: &Path) -> dogma_v2_common::Result<Collection> {
    std::fs::create_dir_all(base_path).map_err(|e| dogma_v2_common::error::Error::Io {
        path: base_path.to_path_buf(),
        source: e,
    })?;
    let vdb_path = base_path.join("workspace.vdb");
    Collection::open_with_config(&vdb_path, &collection_config()).map_err(|e| {
        dogma_v2_common::error::Error::Io {
            path: vdb_path,
            source: std::io::Error::other(e.to_string()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dogma_vdb::error::Error as VdbError;
    use tempfile::TempDir;

    /// Embedder determinista para tests: vector de 8 dims basado en el texto.
    struct DenseEmbedder;

    impl Embedder for DenseEmbedder {
        fn embed(&self, text: &str) -> std::result::Result<Vec<f32>, VdbError> {
            let mut v = Vec::with_capacity(8);
            for i in 0..8 {
                let byte = text.as_bytes().get(i).copied().unwrap_or(0);
                v.push((byte as f32 / 255.0) + (i as f32 * 0.01));
            }
            Ok(v)
        }

        fn dimension(&self) -> usize {
            8
        }
    }

    fn make_temp_workspace() -> TempDir {
        let dir = TempDir::new().expect("temp dir");
        fs::write(
            dir.path().join("main.rs"),
            "pub fn main() {\n    println!(\"hi\");\n}\n",
        )
        .expect("write rs");
        fs::write(
            dir.path().join("lib.rs"),
            "pub struct Config {\n    pub name: String,\n}\n\nimpl Config {\n    pub fn new() -> Self {\n        Self { name: String::new() }\n    }\n}\n",
        )
        .expect("write lib");
        fs::write(
            dir.path().join("README.md"),
            "# Project\n\nThis is the project readme.\n",
        )
        .expect("write md");
        fs::write(dir.path().join("skip.sh"), "#!/bin/sh\necho hi\n").expect("write sh");
        fs::create_dir_all(dir.path().join("target")).expect("target dir");
        fs::write(
            dir.path().join("target/ignored.rs"),
            "pub fn ignored() {}\n",
        )
        .expect("write ignored");
        dir
    }

    #[test]
    fn test_collect_files_ignores_target_and_exts() {
        let dir = make_temp_workspace();
        let indexer = WorkspaceIndexer::new(Arc::new(DenseEmbedder));
        let mut files = Vec::new();
        indexer.collect_files(dir.path(), &mut files);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"README.md".to_string()));
        assert!(
            !names.contains(&"ignored.rs".to_string()),
            "target/ must be ignored"
        );
        assert_eq!(
            names.len(),
            4,
            "expected main.rs, lib.rs, README.md, skip.sh"
        );
    }

    #[test]
    fn test_index_dir_embeds_and_inserts() {
        let dir = make_temp_workspace();
        let mut collection = open_workspace_collection(dir.path()).expect("open collection");
        let indexer = WorkspaceIndexer::new(Arc::new(DenseEmbedder));
        let count = indexer.index_dir(dir.path(), &mut collection);
        assert!(
            count >= 3,
            "should index at least the code files, got {count}"
        );
        assert!(collection.len() >= count);

        let docs: Vec<&Document> = collection.documents().collect();
        let has_sml = docs.iter().any(|d| d.metadata_val("sml").is_some());
        assert!(has_sml, "at least one chunk should carry SML metadata");
        let has_source = docs.iter().any(|d| {
            d.metadata_val("source")
                .map(|s| s.ends_with("main.rs"))
                .unwrap_or(false)
        });
        assert!(has_source, "source metadata should reference main.rs");
    }
}
