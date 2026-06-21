//! # Trait de proveedor LLM OpenAI-Compatibles
//!
//! Cualquier proveedor (OpenAI, Anthropic, Ollama, OpenRouter, etc.)
//! que implemente `LLMProvider` puede ser inyectado en el `RuntimeLoop`
//! sin cambiar el código del runtime.

pub mod openai;

use async_trait::async_trait;
use dogma_v2_common::Result;
use serde::{Deserialize, Serialize};

/// Rol de un mensaje en la conversación.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Un mensaje individual dentro del historial de la conversación.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool calls solicitadas por el LLM (solo en mensajes assistant).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Campos adicionales no-estándar del proveedor (ej: reasoning_content
    /// de DeepSeek, thinking de Claude). Se serializan directamente en el
    /// mensaje assistant para preservar el estado del provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_fields: Vec<(String, serde_json::Value)>,
}

impl Message {
    /// Crea un nuevo mensaje con el rol y contenido dados.
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Vec::new(),
            extra_fields: Vec::new(),
        }
    }

    /// Marca este mensaje como resultado de una herramienta.
    #[must_use]
    pub fn with_tool_result(
        mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        self.tool_call_id = Some(tool_call_id.into());
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Asocia tool_calls a este mensaje (solo para role Assistant).
    #[must_use]
    pub fn with_tool_calls(mut self, tool_calls: Vec<ToolCall>) -> Self {
        self.tool_calls = tool_calls;
        self
    }

    /// Añade un campo extra no-estándar (ej: reasoning_content de DeepSeek).
    #[must_use]
    pub fn with_extra_field(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extra_fields.push((key.to_string(), value));
        self
    }
}

/// Configuración de un proveedor LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// URL base de la API (ej: `https://api.openai.com/v1`).
    pub base_url: String,
    /// Modelo a usar (ej: `gpt-4o`, `claude-sonnet-4`).
    pub model: String,
    /// API key (opcional, puede venir de variables de entorno).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Temperatura para el sampling.
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    /// Máximo de tokens a generar.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_temperature() -> f32 {
    0.7
}

fn default_max_tokens() -> u32 {
    4096
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            api_key: None,
            temperature: default_temperature(),
            max_tokens: default_max_tokens(),
        }
    }
}

/// Respuesta completa del LLM (no stream).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMResponse {
    /// Contenido textual de la respuesta.
    pub content: String,
    /// Tool calls solicitadas por el modelo (si aplica).
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// Uso de tokens reportado por el provider.
    #[serde(default)]
    pub usage: TokenUsage,
    /// Campos extra no-estándar del assistant message original
    /// (ej: reasoning_content de DeepSeek). Se reinyectan al
    /// construir el assistant message en el RuntimeLoop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_fields: Vec<(String, serde_json::Value)>,
}

/// Una tool call solicitada por el LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// ID único de esta invocación.
    pub id: String,
    /// Nombre de la herramienta a ejecutar.
    pub name: String,
    /// Argumentos como JSON string.
    pub arguments: String,
}

/// Estadísticas de uso de tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Trait que debe implementar cualquier proveedor LLM.
#[async_trait]
pub trait LLMProvider: Send + Sync {
    /// Envía un historial de mensajes al LLM y devuelve la respuesta.
    async fn chat(&self, messages: &[Message], tools: &[serde_json::Value]) -> Result<LLMResponse>;

    /// Envía un historial de mensajes y devuelve un stream de chunks.
    ///
    /// La implementación por defecto delega en `chat()` y envuelve el
    /// resultado en un stream de chunks.
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[serde_json::Value],
    ) -> Result<tokio::sync::mpsc::Receiver<Result<StreamChunk>>> {
        let response = self.chat(messages, tools).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);

        // Emit reasoning if present
        for (key, val) in &response.extra_fields {
            if key == "reasoning_content" {
                if let Some(s) = val.as_str() {
                    let _ = tx
                        .send(Ok(StreamChunk::ReasoningDelta(s.to_string())))
                        .await;
                }
            }
        }

        // Emit content
        let _ = tx
            .send(Ok(StreamChunk::ContentDelta(response.content)))
            .await;

        // Emit tool calls
        for (i, tc) in response.tool_calls.iter().enumerate() {
            let _ = tx
                .send(Ok(StreamChunk::ToolCallDelta {
                    index: i,
                    id: Some(tc.id.clone()),
                    name: Some(tc.name.clone()),
                    arguments_delta: tc.arguments.clone(),
                }))
                .await;
        }

        let _ = tx.send(Ok(StreamChunk::Done(response.usage))).await;
        drop(tx);
        Ok(rx)
    }

    /// Devuelve la configuración activa del proveedor.
    fn config(&self) -> &ProviderConfig;
}

/// Un chunk de una respuesta streaming.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    /// Delta de reasoning/thinking (DeepSeek `reasoning_content`).
    ReasoningDelta(String),
    /// Delta del contenido de la respuesta final.
    ContentDelta(String),
    /// Delta de una tool call (argumentos parciales).
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// Estadísticas de uso de tokens (solo en el último chunk).
    Done(TokenUsage),
}
