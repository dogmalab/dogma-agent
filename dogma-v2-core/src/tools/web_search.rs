//! # web_search — Search the web for information
//!
//! Uses the Exa API to perform semantic web search. Returns URLs,
//! titles, and highlighted snippets. Requires `EXA_API_KEY` env var.

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::Value;

/// Web search tool backed by the Exa API.
pub struct WebSearchTool {
    api_key: String,
}

impl WebSearchTool {
    pub fn new(api_key: String) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web for information. Returns relevant URLs, titles, and snippets."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default: 5, max: 20)",
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, args: &Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing required argument: query".to_string())?;

        let num_results = args
            .get("num_results")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .min(20) as usize;

        let client = reqwest::Client::new();
        let response = client
            .post("https://api.exa.ai/search")
            .header("x-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "query": query,
                "numResults": num_results,
                "contents": { "highlights": true }
            }))
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| format!("web search request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Exa API error (status {status}): {body}"));
        }

        let data: Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse Exa response: {e}"))?;

        // Normalize to standard format
        let mut web_results = Vec::new();
        if let Some(results) = data.get("results").and_then(Value::as_array) {
            for (i, result) in results.iter().enumerate() {
                let highlights = result
                    .get("highlights")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();

                web_results.push(serde_json::json!({
                    "title": result.get("title").and_then(Value::as_str).unwrap_or(""),
                    "url": result.get("url").and_then(Value::as_str).unwrap_or(""),
                    "description": highlights,
                    "position": i + 1,
                }));
            }
        }

        let output = serde_json::json!({
            "success": true,
            "data": { "web": web_results }
        });

        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_missing_query() {
        let tool = WebSearchTool::new("test-key".into());
        let result = tool.call(&json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument"));
    }

    #[tokio::test]
    async fn test_tool_schema() {
        let tool = WebSearchTool::new("test-key".into());
        assert_eq!(tool.name(), "web_search");
        let params = tool.parameters();
        assert!(params.get("properties").unwrap().get("query").is_some());
    }
}
