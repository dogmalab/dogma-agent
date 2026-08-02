//! # web_extract — Extract content from web pages
//!
//! Extrae el contenido principal de URLs. Por defecto hace `GET` directo
//! y extrae texto legible del HTML (sin API key). Si `EXA_API_KEY` está
//! definida, usa la API de Exa (`/get_contents`).

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::time::Duration;

/// Límite de texto extraído por página.
const MAX_EXTRACT_CHARS: usize = 20_000;

/// Web content extraction tool. Backend directo por defecto, Exa opcional.
pub struct WebExtractTool {
    client: reqwest::Client,
    exa_api_key: Option<String>,
}

impl WebExtractTool {
    /// Crea la tool leyendo `EXA_API_KEY` del entorno (opcional).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let exa_api_key = std::env::var("EXA_API_KEY").ok().filter(|k| !k.is_empty());
        Self {
            client,
            exa_api_key,
        }
    }

    /// Extracción directa: GET a cada URL y parse del HTML.
    async fn extract_direct(&self, urls: &[String]) -> ToolResult {
        let mut documents = Vec::new();
        for url in urls {
            let (title, content) = match self.client.get(url).send().await {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        (
                            String::new(),
                            format!(
                                "HTTP {}: no se pudo obtener la página",
                                resp.status().as_u16()
                            ),
                        )
                    } else {
                        let html = resp.text().await.unwrap_or_default();
                        (extract_title(&html), extract_readable_text(&html))
                    }
                }
                Err(e) => (String::new(), format!("fetch error: {e}")),
            };

            documents.push(json!({
                "url": url,
                "title": title,
                "content": content,
            }));
        }

        let output = json!({ "success": true, "data": { "documents": documents } });
        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }

    /// Extracción vía Exa (`/get_contents`, requiere `EXA_API_KEY`).
    async fn extract_exa(&self, urls: &[String]) -> ToolResult {
        let key = self
            .exa_api_key
            .as_deref()
            .ok_or_else(|| "EXA_API_KEY missing".to_string())?;

        let response = self
            .client
            .post("https://api.exa.ai/get_contents")
            .header("x-api-key", key)
            .header("Content-Type", "application/json")
            .json(&json!({
                "urls": urls,
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

        let mut documents = Vec::new();
        if let Some(results) = data.get("results").and_then(Value::as_array) {
            for result in results {
                let content = result.get("text").and_then(Value::as_str).unwrap_or("");
                documents.push(json!({
                    "url": result.get("url").and_then(Value::as_str).unwrap_or(""),
                    "title": result.get("title").and_then(Value::as_str).unwrap_or(""),
                    "content": content,
                }));
            }
        }

        let output = json!({ "success": true, "data": { "documents": documents } });
        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }
}

impl Default for WebExtractTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebExtractTool {
    fn name(&self) -> &'static str {
        "web_extract"
    }

    fn description(&self) -> &'static str {
        "Extract content from web pages as clean text. Returns the main content of each URL. Works out of the box; uses Exa if EXA_API_KEY is set."
    }

    fn parameters(&self) -> Value {
        json!({
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

        if self.exa_api_key.is_some() {
            self.extract_exa(&url_strs).await
        } else {
            self.extract_direct(&url_strs).await
        }
    }
}

/// Extrae el título (`<title>`) de una página.
fn extract_title(html: &str) -> String {
    let document = Html::parse_document(html);
    if let Ok(sel) = Selector::parse("title") {
        if let Some(node) = document.select(&sel).next() {
            return node.text().collect::<String>().trim().to_string();
        }
    }
    String::new()
}

/// Extrae texto legible: párrafos, encabezados, listas, código.
fn extract_readable_text(html: &str) -> String {
    let document = Html::parse_document(html);
    let selectors = ["p", "h1", "h2", "h3", "h4", "li", "pre", "blockquote"];
    let mut text = String::new();
    let mut seen = std::collections::HashSet::new();

    for sel_str in selectors {
        let Ok(sel) = Selector::parse(sel_str) else {
            continue;
        };
        for node in document.select(&sel) {
            let t = node.text().collect::<String>().trim().to_string();
            if t.is_empty() || seen.contains(&t) {
                continue;
            }
            seen.insert(t.clone());
            text.push_str(&t);
            text.push('\n');
            if text.len() >= MAX_EXTRACT_CHARS {
                return text;
            }
        }
    }
    text.chars().take(MAX_EXTRACT_CHARS).collect()
}

fn format_extract_error(status: u16, body: &str) -> String {
    match status {
        0 => format!(
            "Exa content extraction failed: network/timeout error. Check internet and EXA_API_KEY. Details: {body}"
        ),
        401 | 403 => format!(
            "Exa content extraction failed: invalid or expired API key. Set a valid EXA_API_KEY. (HTTP {status})"
        ),
        429 => "Exa content extraction failed: rate limited. Wait a moment and retry. (HTTP 429)"
            .to_string(),
        _ => format!("Exa content extraction error (HTTP {status}): {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_missing_urls() {
        let tool = WebExtractTool::new();
        let result = tool.call(&json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument"));
    }

    #[tokio::test]
    async fn test_empty_urls() {
        let tool = WebExtractTool::new();
        let result = tool.call(&json!({"urls": []})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[tokio::test]
    async fn test_too_many_urls() {
        let tool = WebExtractTool::new();
        let urls: Vec<String> = (0..11)
            .map(|i| format!("https://example.com/{i}"))
            .collect();
        let result = tool.call(&json!({"urls": urls})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum 10"));
    }

    #[test]
    fn test_extract_readable_text() {
        let html = r#"
        <html><head><title>Mi Página</title></head>
        <body>
          <h1>Bienvenido</h1>
          <p>Este es el primer párrafo.</p>
          <p>Este es el segundo.</p>
          <ul><li>ítem A</li><li>ítem B</li></ul>
        </body></html>
        "#;
        let title = extract_title(html);
        assert_eq!(title, "Mi Página");
        let text = extract_readable_text(html);
        assert!(text.contains("Bienvenido"));
        assert!(text.contains("primer párrafo"));
        assert!(text.contains("ítem B"));
    }
}
