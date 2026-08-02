//! # web_extract — Extract content from web pages
//!
//! Uses the Exa API to extract clean text content from URLs.
//! Returns markdown-formatted content. Requires `EXA_API_KEY` env var.

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

/// Web content extraction tool backed by the Exa API.
pub struct WebExtractTool {
    api_key: String,
    client: reqwest::Client,
}

impl WebExtractTool {
    pub fn new(api_key: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { api_key, client }
    }
}

#[async_trait]
impl Tool for WebExtractTool {
    fn name(&self) -> &'static str {
        "web_extract"
    }

    fn description(&self) -> &'static str {
        "Extract content from web pages as clean text. Returns the main content of each URL."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "urls": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of URLs to extract content from"
                }
            },
            "required": ["urls"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let urls = args
            .get("urls")
            .and_then(Value::as_array)
            .ok_or_else(|| "missing required argument: urls".to_string())?;

        if urls.is_empty() {
            return Err("urls array is empty".to_string());
        }

        if urls.len() > 10 {
            return Err("maximum 10 URLs per extraction".to_string());
        }

        let url_strs: Vec<String> = urls
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();

        let response = self
            .client
            .post("https://api.exa.ai/get_contents")
            .header("x-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "urls": url_strs,
                "text": true
            }))
            .send()
            .await
            .map_err(|e| format_extract_error(0, &e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>();
            return Err(format_extract_error(status.as_u16(), &body));
        }

        let data: Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse Exa response: {e}"))?;

        // Normalize results
        let mut documents = Vec::new();
        if let Some(results) = data.get("results").and_then(Value::as_array) {
            for result in results {
                let content = result.get("text").and_then(Value::as_str).unwrap_or("");

                documents.push(serde_json::json!({
                    "url": result.get("url").and_then(Value::as_str).unwrap_or(""),
                    "title": result.get("title").and_then(Value::as_str).unwrap_or(""),
                    "content": content,
                }));
            }
        }

        let output = serde_json::json!({
            "success": true,
            "data": { "documents": documents }
        });

        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }
}

fn format_extract_error(status: u16, body: &str) -> String {
    match status {
        0 => format!("Exa content extraction failed: network/timeout error. Check internet and EXA_API_KEY. Details: {body}"),
        401 | 403 => format!("Exa content extraction failed: invalid or expired API key. Set a valid EXA_API_KEY. (HTTP {status})"),
        429 => format!("Exa content extraction failed: rate limited. Wait a moment and retry. (HTTP 429)"),
        _ => format!("Exa content extraction error (HTTP {status}): {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_missing_urls() {
        let tool = WebExtractTool::new("test-key".into());
        let result = tool.call(&json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument"));
    }

    #[tokio::test]
    async fn test_empty_urls() {
        let tool = WebExtractTool::new("test-key".into());
        let result = tool.call(&json!({"urls": []})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[tokio::test]
    async fn test_too_many_urls() {
        let tool = WebExtractTool::new("test-key".into());
        let urls: Vec<String> = (0..11)
            .map(|i| format!("https://example.com/{i}"))
            .collect();
        let result = tool.call(&json!({"urls": urls})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum 10"));
    }
}
