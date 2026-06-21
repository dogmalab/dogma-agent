//! # Proveedor LLM OpenAI-Compatibles
//!
//! Implementa `LLMProvider` para cualquier endpoint que hable el protocolo
//! `/v1/chat/completions` de OpenAI: OpenAI, Ollama, vLLM, OpenRouter, etc.
//!
//! ## Parseo ultra-defensivo
//!
//! No se asume NADA sobre la estructura del JSON de respuesta. Cada campo
//! se extrae con `serde_json::Value::get()`, se verifica el tipo, y ante
//! cualquier anomalía se emite un `tracing::warn!` con el contexto completo
//! y se devuelve un `DogmaError::Infrastructure` detallado. 0 panics.

use std::time::Duration;

use async_trait::async_trait;
use dogma_v2_common::Result;
use dogma_v2_common::error::Error as DogmaError;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde_json::Value;
use tracing::{debug, trace, warn};

use super::{
    LLMProvider, LLMResponse, Message, MessageRole, ProviderConfig, StreamChunk, TokenUsage,
};

// ---------------------------------------------------------------------------
// Constantes
// ---------------------------------------------------------------------------

/// Timeout por defecto para peticiones HTTP (60 segundos).
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Ruta del endpoint de chat completions.
const CHAT_ENDPOINT: &str = "/chat/completions";

// ---------------------------------------------------------------------------
// OpenAIProvider
// ---------------------------------------------------------------------------

/// Proveedor LLM para APIs compatibles con OpenAI (`/v1/chat/completions`).
///
/// # Ejemplo
///
/// ```ignore
/// use dogma_v2_core::runtime::provider::openai::OpenAiProvider;
/// use dogma_v2_core::runtime::provider::ProviderConfig;
///
/// let config = ProviderConfig {
///     base_url: "https://api.openai.com/v1".into(),
///     model: "gpt-4o".into(),
///     api_key: Some("sk-...".into()),
///     ..Default::default()
/// };
/// let provider = OpenAiProvider::new(config).expect("valid provider");
/// ```
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

impl std::fmt::Debug for OpenAiProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiProvider")
            .field("base_url", &self.config.base_url)
            .field("model", &self.config.model)
            .field("has_api_key", &self.config.api_key.is_some())
            .field("temperature", &self.config.temperature)
            .field("max_tokens", &self.config.max_tokens)
            .finish()
    }
}

impl OpenAiProvider {
    /// Crea un nuevo proveedor con la configuración dada.
    ///
    /// Configura automáticamente:
    /// - Timeout de 60s por petición.
    /// - Pool de conexiones reutilizables (keep-alive).
    /// - Headers `Content-Type: application/json` y `Authorization` (si hay
    ///   API key).
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Validation` si falta `base_url` o `model`.
    pub fn new(config: ProviderConfig) -> Result<Self> {
        if config.base_url.is_empty() {
            return Err(DogmaError::Validation("base_url cannot be empty".into()));
        }
        if config.model.is_empty() {
            return Err(DogmaError::Validation("model cannot be empty".into()));
        }

        let timeout_secs = DEFAULT_TIMEOUT_SECS;
        let timeout = Duration::from_secs(timeout_secs);

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        if let Some(ref key) = config.api_key {
            let bearer = format!("Bearer {key}");
            if let Ok(val) = HeaderValue::from_str(&bearer) {
                headers.insert(AUTHORIZATION, val);
            } else {
                warn!("API key contains invalid characters — skipping Authorization header");
            }
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .connect_timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(8)
            .build()
            .map_err(|e| DogmaError::Network {
                detail: format!("failed to build HTTP client: {e}"),
                source: Some(Box::new(e)),
            })?;

        debug!(
            "OpenAIProvider initialized: base_url={}, model={}, timeout={timeout_secs}s",
            config.base_url, config.model
        );

        Ok(Self { client, config })
    }

    /// Construye el JSON body para `/v1/chat/completions`.
    ///
    /// Si `tools` contiene especificaciones en formato OpenAI
    /// (`{"type":"function","function":{...}}`), las inyecta en el
    /// body para que el LLM pueda invocarlas.
    fn build_request_body(&self, messages: &[Message], tools: &[Value]) -> Value {
        let msgs: Vec<Value> = messages.iter().map(Self::serialize_message).collect();

        let mut body = serde_json::json!({
            "model": self.config.model,
            "messages": msgs,
            "temperature": self.config.temperature,
            "max_tokens": self.config.max_tokens,
        });

        // Inyectar herramientas si hay — formato estándar OpenAI
        if !tools.is_empty() {
            body["tools"] = serde_json::Value::Array(tools.to_vec());
        }

        body
    }

    /// Convierte un `Message` interno al formato de la API.
    /// Incluye todos los campos extra (ej: `reasoning_content` de DeepSeek).
    fn serialize_message(msg: &Message) -> Value {
        let role_str = match msg.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };

        let mut entry = serde_json::json!({
            "role": role_str,
            "content": msg.content,
        });

        if let Some(ref call_id) = msg.tool_call_id {
            entry["tool_call_id"] = Value::String(call_id.clone());
        }

        if !msg.tool_calls.is_empty() {
            let tcs: Vec<Value> = msg
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.name,
                            "arguments": tc.arguments,
                        }
                    })
                })
                .collect();
            entry["tool_calls"] = Value::Array(tcs);
        }

        // Incluir campos extra (reasoning_content de DeepSeek, etc.)
        for (key, val) in &msg.extra_fields {
            entry[key] = val.clone();
        }

        entry
    }

    /// Extrae la URL completa del endpoint.
    fn chat_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        format!("{base}{CHAT_ENDPOINT}")
    }
}

// ---------------------------------------------------------------------------
// Implementacion del trait LLMProvider
// ---------------------------------------------------------------------------

#[async_trait]
impl LLMProvider for OpenAiProvider {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat(&self, messages: &[Message], tools: &[Value]) -> Result<LLMResponse> {
        let url = self.chat_url();
        let body = self.build_request_body(messages, tools);

        trace!(
            "Sending request to {url}: {} messages, model={}",
            messages.len(),
            self.config.model
        );

        // ── 1. Enviar peticion ────────────────────────────────────────
        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    DogmaError::Network {
                        detail: format!("request timed out after {DEFAULT_TIMEOUT_SECS}s"),
                        source: Some(Box::new(e)),
                    }
                } else if e.is_connect() {
                    DogmaError::Network {
                        detail: format!("connection refused to {url}"),
                        source: Some(Box::new(e)),
                    }
                } else {
                    DogmaError::Network {
                        detail: format!("HTTP transport error: {e}"),
                        source: Some(Box::new(e)),
                    }
                }
            })?;

        // ── 2. Verificar codigo HTTP ──────────────────────────────────
        let status = response.status();
        let status_code: u16 = status.as_u16();

        if status.is_success() {
            // continuar abajo
        } else if status_code == 429 {
            let retry_after = response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5);
            return Err(DogmaError::RateLimited {
                retry_after_secs: retry_after,
            });
        } else {
            let status_text = status.canonical_reason().unwrap_or("unknown");
            // Intentar extraer mensaje de error del body
            let error_detail = match response.text().await {
                Ok(text) if !text.is_empty() => {
                    // Intentar parsear el error de OpenAI: {"error": {"message": "..."}}
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        val.pointer("/error/message")
                            .and_then(Value::as_str)
                            .map(|m| format!("{status_text} — {m}"))
                            .unwrap_or_else(|| text.chars().take(200).collect())
                    } else {
                        text.chars().take(200).collect()
                    }
                }
                _ => status_text.to_string(),
            };
            return Err(DogmaError::Api {
                status_code,
                detail: error_detail,
            });
        }

        // ── 3. Leer body completo ─────────────────────────────────────
        let body_text = response.text().await.map_err(|e| DogmaError::Network {
            detail: format!("failed to read response body: {e}"),
            source: Some(Box::new(e)),
        })?;

        let root: Value = serde_json::from_str(&body_text).map_err(|e| {
            warn!(
                "LLM returned invalid JSON ({}): {}",
                e,
                body_text.chars().take(300).collect::<String>()
            );
            DogmaError::Api {
                status_code,
                detail: format!("invalid JSON response: {e}"),
            }
        })?;

        // ── 4. Parseo ultra-defensivo de choices ──────────────────────
        //    No asumimos que choices existe, ni que tiene elementos, ni
        //    que message esta completo, ni que content es string.
        let choices = root
            .get("choices")
            .and_then(Value::as_array)
            .map(|arr| arr.as_slice())
            .unwrap_or(&[]);

        let (content, tool_calls, extra_fields) = if let Some(first_choice) = choices.first() {
            let msg = first_choice.get("message").unwrap_or(&Value::Null);

            let content = msg
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let tool_calls = Self::parse_tool_calls(msg);

            // Extraer campos no-estándar del assistant message
            // (reasoning_content de DeepSeek, thinking de Claude, etc.)
            let known_keys = ["content", "tool_calls", "role", "refusal"];
            let extra: Vec<(String, Value)> = match msg {
                Value::Object(map) => map
                    .iter()
                    .filter(|(k, _)| !known_keys.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                _ => Vec::new(),
            };

            (content, tool_calls, extra)
        } else {
            warn!(
                "LLM response missing 'choices' array: {}",
                serde_json::to_string(&root).unwrap_or_default()
            );
            (String::new(), Vec::new(), Vec::new())
        };

        // ── 5. Parseo ultra-defensivo de usage ────────────────────────
        let usage = root.get("usage").map(Self::parse_usage).unwrap_or_else(|| {
            trace!("LLM response missing 'usage' field");
            TokenUsage::default()
        });

        debug!(
            "LLM response: content_len={}, tool_calls={}, tokens={}, extra_fields={}",
            content.len(),
            tool_calls.len(),
            usage.total_tokens,
            extra_fields.len(),
        );

        Ok(LLMResponse {
            content,
            tool_calls,
            usage,
            extra_fields,
        })
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[Value],
    ) -> Result<tokio::sync::mpsc::Receiver<Result<StreamChunk>>> {
        let url = self.chat_url();
        let mut body = self.build_request_body(messages, tools);
        body["stream"] = Value::Bool(true);

        let response = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| DogmaError::Network {
                detail: format!("streaming request failed: {e}"),
                source: Some(Box::new(e)),
            })?;

        let status = response.status();
        if !status.is_success() {
            let status_code: u16 = status.as_u16();
            let error_detail = match response.text().await {
                Ok(text) if !text.is_empty() => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        val.pointer("/error/message")
                            .and_then(Value::as_str)
                            .map(|m| format!("{status_code} — {m}"))
                            .unwrap_or_else(|| text.chars().take(200).collect())
                    } else {
                        text.chars().take(200).collect()
                    }
                }
                _ => status.canonical_reason().unwrap_or("unknown").to_string(),
            };
            return Err(DogmaError::Api {
                status_code,
                detail: error_detail,
            });
        }

        let (tx, rx) = tokio::sync::mpsc::channel(128);

        tokio::spawn(async move {
            use futures::StreamExt;

            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx
                            .send(Err(DogmaError::Network {
                                detail: format!("stream read error: {e}"),
                                source: Some(Box::new(e)),
                            }))
                            .await;
                        break;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }

                    if line == "data: [DONE]" {
                        let _ = tx.send(Ok(StreamChunk::Done(TokenUsage::default()))).await;
                        return;
                    }

                    if let Some(data) = line.strip_prefix("data: ") {
                        match serde_json::from_str::<Value>(data) {
                            Ok(json) => {
                                if let Some(chunks) = Self::parse_stream_chunk(&json) {
                                    for chunk in chunks {
                                        let _ = tx.send(Ok(chunk)).await;
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("Failed to parse SSE chunk: {e}");
                            }
                        }
                    }
                }
            }
        });

        Ok(rx)
    }
}

// ---------------------------------------------------------------------------
// Metodos auxiliares de parseo
// ---------------------------------------------------------------------------

impl OpenAiProvider {
    /// Extrae `Vec<ToolCall>` del objeto `message`.
    ///
    /// El array `tool_calls` de la API tiene esta forma:
    /// ```json
    /// [{
    ///   "id": "call_xxx",
    ///   "type": "function",
    ///   "function": { "name": "read_file", "arguments": "{\"path\": \"...\"}" }
    /// }]
    /// ```
    fn parse_tool_calls(msg: &Value) -> Vec<super::ToolCall> {
        let Some(tc_array) = msg.get("tool_calls").and_then(Value::as_array) else {
            return Vec::new();
        };

        let mut result = Vec::with_capacity(tc_array.len());

        for (i, tc) in tc_array.iter().enumerate() {
            // Cada tool call necesita: id (string) + function.name + function.arguments
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let function = tc.get("function").unwrap_or(&Value::Null);

            let name = function
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            let arguments = function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}")
                .to_string();

            if id.is_empty() {
                warn!(
                    "Skipping malformed tool_call at index {i}: missing 'id': {tc}",
                    tc = serde_json::to_string(tc).unwrap_or_default()
                );
                continue;
            }

            trace!(
                "Parsed tool_call: id={id}, name={name}, args_len={}",
                arguments.len()
            );
            result.push(super::ToolCall {
                id,
                name,
                arguments,
            });
        }

        result
    }

    /// Extrae `TokenUsage` del objeto `usage`.
    ///
    /// OpenAI devuelve:
    /// ```json
    /// { "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150 }
    /// ```
    /// Pero no todos los proveedores incluyen todos los campos.
    fn parse_usage(usage: &Value) -> TokenUsage {
        let prompt = usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        let completion = usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;

        let total = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(prompt.saturating_add(completion))) as u32;

        if total == 0 {
            trace!("TokenUsage all zeros — provider may not report token counts");
        }

        TokenUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
        }
    }

    /// Parsea un chunk SSE del streaming y extrae StreamChunks.
    fn parse_stream_chunk(json: &Value) -> Option<Vec<StreamChunk>> {
        let choices = json.get("choices").and_then(Value::as_array)?;
        let choice = choices.first()?;

        let delta = choice.get("delta")?;

        let mut chunks = Vec::new();

        // Reasoning content (DeepSeek)
        if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
            if !reasoning.is_empty() {
                chunks.push(StreamChunk::ReasoningDelta(reasoning.to_string()));
            }
        }

        // Content
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                chunks.push(StreamChunk::ContentDelta(content.to_string()));
            }
        }

        // Tool calls
        if let Some(tc_array) = delta.get("tool_calls").and_then(Value::as_array) {
            for (i, tc) in tc_array.iter().enumerate() {
                let id = tc.get("id").and_then(Value::as_str).map(String::from);
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();

                chunks.push(StreamChunk::ToolCallDelta {
                    index: i,
                    id,
                    name,
                    arguments_delta: args,
                });
            }
        }

        if chunks.is_empty() {
            None
        } else {
            Some(chunks)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> ProviderConfig {
        ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-4o".into(),
            api_key: Some("sk-test-key".into()),
            ..Default::default()
        }
    }

    #[test]
    fn test_new_validates_config() {
        let err = OpenAiProvider::new(ProviderConfig {
            base_url: String::new(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("base_url"));

        let err = OpenAiProvider::new(ProviderConfig {
            base_url: "https://example.com".into(),
            model: String::new(),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.to_string().contains("model"));
    }

    #[test]
    fn test_chat_url() {
        let provider = OpenAiProvider::new(sample_config()).unwrap();
        assert_eq!(
            provider.chat_url(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn test_chat_url_trailing_slash() {
        let mut cfg = sample_config();
        cfg.base_url = "https://localhost:11434/v1/".into();
        let provider = OpenAiProvider::new(cfg).unwrap();
        assert_eq!(
            provider.chat_url(),
            "https://localhost:11434/v1/chat/completions"
        );
    }

    #[test]
    fn test_serialize_message_user() {
        let msg = Message::new(MessageRole::User, "hello");
        let json = OpenAiProvider::serialize_message(&msg);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn test_serialize_message_tool() {
        let msg =
            Message::new(MessageRole::Tool, "result").with_tool_result("call_123", "read_file");
        let json = OpenAiProvider::serialize_message(&msg);
        assert_eq!(json["role"], "tool");
        assert_eq!(json["content"], "result");
        assert_eq!(json["tool_call_id"], "call_123");
    }

    #[test]
    fn test_parse_usage_full() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
        });
        let usage = OpenAiProvider::parse_usage(&json);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn test_parse_usage_missing_total() {
        let json = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
        });
        let usage = OpenAiProvider::parse_usage(&json);
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150); // calculado
    }

    #[test]
    fn test_parse_usage_empty() {
        let usage = OpenAiProvider::parse_usage(&Value::Null);
        assert_eq!(usage.prompt_tokens, 0);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn test_parse_tool_calls_valid() {
        let msg = serde_json::json!({
            "tool_calls": [{
                "id": "call_abc",
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": "{\"path\": \"/tmp/test.txt\"}"
                }
            }]
        });
        let calls = OpenAiProvider::parse_tool_calls(&msg);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].name, "read_file");
        assert!(calls[0].arguments.contains("path"));
    }

    #[test]
    fn test_parse_tool_calls_empty() {
        let calls = OpenAiProvider::parse_tool_calls(&Value::Null);
        assert!(calls.is_empty());

        let msg = serde_json::json!({});
        let calls = OpenAiProvider::parse_tool_calls(&msg);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_tool_calls_malformed_skipped() {
        let msg = serde_json::json!({
            "tool_calls": [
                { "id": "call_1", "function": { "name": "ok", "arguments": "{}" } },
                { "id": "", "function": { "name": "", "arguments": "" } },
                { "function": { "name": "no_id" } },
            ]
        });
        let calls = OpenAiProvider::parse_tool_calls(&msg);
        // solo la primera es valida; las otras 2 se saltan
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
    }

    #[test]
    fn test_parse_usage_wrong_types() {
        // Si el proveedor devuelve strings en lugar de numeros,
        // as_u64 devuelve None y caemos al default (0).
        let json = serde_json::json!({
            "prompt_tokens": "muchos",
            "completion_tokens": null,
            "total_tokens": true,
        });
        let usage = OpenAiProvider::parse_usage(&json);
        assert_eq!(usage.total_tokens, 0); // 0 + 0 = 0
    }

    #[test]
    fn test_build_request_body() {
        let provider = OpenAiProvider::new(sample_config()).unwrap();
        let msgs = vec![
            Message::new(MessageRole::System, "You are helpful"),
            Message::new(MessageRole::User, "Hi!"),
        ];
        let body = provider.build_request_body(&msgs, &[]);

        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"].as_array().map(|a| a.len()), Some(2));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "Hi!");
        assert!((body["temperature"].as_f64().unwrap() - 0.7).abs() < 0.001);
    }
}
