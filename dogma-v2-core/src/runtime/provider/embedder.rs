//! # HttpEmbedder — Embedder via OpenAI-compatible `/embeddings`
//!
//! Implementa `dogma_vdb::embedding::Embedder` contra el endpoint
//! `/embeddings` del mismo proveedor usado para chat (OpenAI, Ollama,
//! OpenRouter, etc.). Usa `ureq` (cliente síncrono) porque el trait
//! `Embedder` es síncrono y no puede esperar dentro de un runtime tokio.
//!
//! La dimensión se detecta en la primera respuesta y se cachea.

use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use tracing::debug;

use super::ProviderConfig;

/// Timeout por petición de embeddings (30 segundos).
const EMBED_TIMEOUT_SECS: u64 = 30;

/// Embedder HTTP síncrono contra un endpoint OpenAI-compatible.
pub struct HttpEmbedder {
    agent: ureq::Agent,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    dimension: Mutex<Option<usize>>,
}

impl std::fmt::Debug for HttpEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpEmbedder")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("has_api_key", &self.api_key.is_some())
            .finish()
    }
}

impl HttpEmbedder {
    /// Crea un embedder a partir de la configuración del proveedor.
    ///
    /// `embedding_model` es el modelo de embeddings (ej:
    /// `text-embedding-3-small`). Distinto del modelo de chat.
    pub fn new(config: &ProviderConfig, embedding_model: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(EMBED_TIMEOUT_SECS))
            .build();

        let endpoint = format!("{}/embeddings", config.base_url.trim_end_matches('/'));

        debug!("HttpEmbedder initialized: {endpoint}, model={embedding_model}");

        Self {
            agent,
            endpoint,
            model: embedding_model,
            api_key: config.api_key.clone(),
            dimension: Mutex::new(None),
        }
    }

    /// Construye un `HttpEmbedder` solo si la configuración lo permite.
    ///
    /// Devuelve `None` si no hay `base_url` o no hay modelo de embeddings.
    /// Sin modelo, la búsqueda semántica queda deshabilitada (vacío) en vez
    /// de fallar.
    pub fn build_optional(config: &ProviderConfig, embedding_model: Option<&str>) -> Option<Self> {
        if config.base_url.is_empty() {
            return None;
        }
        let model = embedding_model.unwrap_or("").trim();
        if model.is_empty() {
            debug!("No embedding model configured — semantic search disabled");
            return None;
        }
        Some(Self::new(config, model.to_string()))
    }
}

impl dogma_vdb::embedding::Embedder for HttpEmbedder {
    fn embed(&self, text: &str) -> dogma_vdb::error::Result<Vec<f32>> {
        let mut batch = self.embed_batch(&[text])?;
        Ok(batch.pop().unwrap_or_default())
    }

    fn dimension(&self) -> usize {
        (*self.dimension.lock()).unwrap_or(0)
    }

    fn embed_batch(&self, texts: &[&str]) -> dogma_vdb::error::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let mut req = self.agent.post(&self.endpoint);
        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {key}"));
        }

        let resp = req
            .send_json(body)
            .map_err(|e| vdb_err(format!("embedding request failed: {e}")))?;

        let json: Value = resp
            .into_json()
            .map_err(|e| vdb_err(format!("invalid embedding response: {e}")))?;

        let data = json
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| vdb_err("embedding response missing 'data'".to_string()))?;

        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let emb = item
                .get("embedding")
                .and_then(Value::as_array)
                .ok_or_else(|| vdb_err("embedding item missing 'embedding'".to_string()))?;
            let vec: Vec<f32> = emb
                .iter()
                .filter_map(Value::as_f64)
                .map(|x| x as f32)
                .collect();
            if vec.is_empty() {
                return Err(vdb_err("empty embedding vector".to_string()));
            }
            out.push(vec);
        }

        if let Some(first) = out.first() {
            let mut dim = self.dimension.lock();
            if dim.is_none() {
                *dim = Some(first.len());
            }
        }

        Ok(out)
    }
}

/// Construye un error `dogma_vdb::error::Error::Internal`.
fn vdb_err(msg: String) -> dogma_vdb::error::Error {
    dogma_vdb::error::Error::Internal(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dogma_vdb::embedding::Embedder;

    /// Servidor HTTP mínimo que responde al endpoint /embeddings.
    ///
    /// Solo se usa en tests; no depende de red externa.
    fn spawn_embedding_server() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                use std::io::{Read, Write};
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);

                let body = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[0.1,0.2,0.3]},{"object":"embedding","index":1,"embedding":[0.4,0.5,0.6]}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        (addr, handle)
    }

    fn test_config(addr: std::net::SocketAddr) -> ProviderConfig {
        ProviderConfig {
            base_url: format!("http://{addr}/v1"),
            model: "chat-model".into(),
            api_key: Some("sk-test".into()),
            ..Default::default()
        }
    }

    #[test]
    fn test_build_optional_requires_model() {
        let config = ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            ..Default::default()
        };
        assert!(HttpEmbedder::build_optional(&config, None).is_none());
        assert!(HttpEmbedder::build_optional(&config, Some("  ")).is_none());
        assert!(HttpEmbedder::build_optional(&config, Some("text-embedding-3-small")).is_some());
    }

    #[test]
    fn test_build_optional_requires_base_url() {
        let config = ProviderConfig::default();
        assert!(HttpEmbedder::build_optional(&config, Some("text-embedding-3-small")).is_none());
    }

    #[test]
    fn test_embed_and_batch() {
        let (addr, server) = spawn_embedding_server();
        let config = test_config(addr);
        let embedder = HttpEmbedder::new(&config, "test-embed".into());

        let batch = embedder.embed_batch(&["hello", "world"]).expect("batch");
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0].len(), 3);
        assert_eq!(batch[1], vec![0.4, 0.5, 0.6]);

        let single = embedder.embed("solo").expect("embed");
        assert_eq!(single.len(), 3);
        assert_eq!(embedder.dimension(), 3);

        drop(server);
    }

    #[test]
    fn test_embed_empty_batch() {
        let embedder = HttpEmbedder::build_optional(
            &test_config("127.0.0.1:1".parse().unwrap()),
            Some("m".into()),
        )
        .expect("built");
        assert!(embedder.embed_batch(&[]).expect("empty").is_empty());
    }
}
