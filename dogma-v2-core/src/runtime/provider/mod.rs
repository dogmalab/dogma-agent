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
    ///
    /// `tools` son las especificaciones de herramientas en formato OpenAI
    /// (tipo `function` con name/description/parameters). Si está vacío,
    /// el LLM no tendrá herramientas disponibles.
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Network` si hay problemas de conexión,
    /// `Error::Api` si el proveedor devuelve un error HTTP,
    /// `Error::RateLimited` si se excede el rate limit.
    async fn chat(&self, messages: &[Message], tools: &[serde_json::Value]) -> Result<LLMResponse>;

    /// Envía un historial de mensajes y devuelve un stream de la respuesta.
    ///
    /// La implementación por defecto delega en `chat()` y envuelve el
    /// resultado en un stream de un solo elemento.
    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[serde_json::Value],
    ) -> Result<tokio::sync::mpsc::Receiver<std::result::Result<String, dogma_v2_common::Error>>>
    {
        let response = self.chat(messages, tools).await?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let _ = tx.send(Ok(response.content)).await;
        // Drop tx to close the receiver
        drop(tx);
        Ok(rx)
    }

    /// Devuelve la configuración activa del proveedor.
    fn config(&self) -> &ProviderConfig;
}
