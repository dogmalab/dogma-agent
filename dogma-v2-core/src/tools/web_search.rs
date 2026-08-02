//! # web_search — Search the web for information
//!
//! Busca en la web sin requerir API key: usa DuckDuckGo (HTML scraping)
//! por defecto. Si `EXA_API_KEY` está definida, usa la API de Exa para
//! mejor calidad de resultados.

use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use scraper::{Html, Selector};
use serde_json::{Value, json};
use std::time::Duration;

/// Web search tool. Backend DuckDuckGo por defecto, Exa opcional.
pub struct WebSearchTool {
    client: reqwest::Client,
    exa_api_key: Option<String>,
}

impl WebSearchTool {
    /// Crea la tool leyendo `EXA_API_KEY` del entorno (opcional).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
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

    /// Búsqueda vía DuckDuckGo (sin auth).
    async fn search_duckduckgo(&self, query: &str, num_results: usize) -> ToolResult {
        let url = format!("https://html.duckduckgo.com/html/?q={}", url_encode(query));
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("DuckDuckGo search request failed: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!(
                "DuckDuckGo error (HTTP {}): {}",
                resp.status().as_u16(),
                resp.status().canonical_reason().unwrap_or("unknown")
            ));
        }

        let html = resp
            .text()
            .await
            .map_err(|e| format!("failed to read DuckDuckGo response: {e}"))?;

        let web_results = parse_ddg_html(&html, num_results);
        // "0 resultados" es un resultado válido, no un error: el LLM puede
        // reformular la query. Se incluye una nota para orientarlo.
        let note = if web_results.is_empty() {
            format!("No results found for: {query}. Try rephrasing the search query.")
        } else {
            String::new()
        };

        let output = json!({ "success": true, "data": { "web": web_results }, "note": note });
        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }

    /// Búsqueda vía Exa (requiere `EXA_API_KEY`).
    async fn search_exa(&self, query: &str, num_results: usize) -> ToolResult {
        let key = self
            .exa_api_key
            .as_deref()
            .ok_or_else(|| "EXA_API_KEY missing".to_string())?;

        let response = self
            .client
            .post("https://api.exa.ai/search")
            .header("x-api-key", key)
            .header("Content-Type", "application/json")
            .json(&json!({
                "query": query,
                "numResults": num_results,
                "contents": { "highlights": true }
            }))
            .send()
            .await
            .map_err(|e| format_exa_error(0, &e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect::<String>();
            return Err(format_exa_error(status.as_u16(), &body));
        }

        let data: Value = response
            .json()
            .await
            .map_err(|e| format!("failed to parse Exa response: {e}"))?;

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

                web_results.push(json!({
                    "title": result.get("title").and_then(Value::as_str).unwrap_or(""),
                    "url": result.get("url").and_then(Value::as_str).unwrap_or(""),
                    "description": highlights,
                    "position": i + 1,
                }));
            }
        }

        let note = if web_results.is_empty() {
            format!("No results found for: {query}. Try rephrasing the search query.")
        } else {
            String::new()
        };

        let output = json!({ "success": true, "data": { "web": web_results }, "note": note });
        serde_json::to_string_pretty(&output).map_err(|e| format!("serialization error: {e}"))
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &'static str {
        "web_search"
    }

    fn description(&self) -> &'static str {
        "Search the web for information. Returns relevant URLs, titles, and snippets. Works out of the box (DuckDuckGo); uses Exa if EXA_API_KEY is set."
    }

    fn parameters(&self) -> Value {
        json!({
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

        if self.exa_api_key.is_some() {
            self.search_exa(query, num_results).await
        } else {
            self.search_duckduckgo(query, num_results).await
        }
    }
}

/// Extrae los resultados del HTML de DuckDuckGo.
fn parse_ddg_html(html: &str, num_results: usize) -> Vec<Value> {
    let document = Html::parse_document(html);
    let result_sel = Selector::parse(".result").expect("result selector");
    let title_sel = Selector::parse("a.result__a").expect("title selector");
    let snippet_sel = Selector::parse(".result__snippet").expect("snippet selector");

    let mut results = Vec::new();
    for (i, node) in document.select(&result_sel).enumerate() {
        if i >= num_results {
            break;
        }
        let title = node
            .select(&title_sel)
            .next()
            .map(|t| t.text().collect::<String>())
            .unwrap_or_default();
        let href = node
            .select(&title_sel)
            .next()
            .and_then(|t| t.value().attr("href"))
            .unwrap_or("")
            .to_string();
        let url = decode_ddg_url(&href);
        let snippet = node
            .select(&snippet_sel)
            .next()
            .map(|s| s.text().collect::<String>())
            .unwrap_or_default();

        if !title.is_empty() || !url.is_empty() {
            results.push(json!({
                "title": title.trim(),
                "url": url,
                "description": snippet.trim(),
                "position": i + 1,
            }));
        }
    }
    results
}

/// Decodifica la URL de redirect de DuckDuckGo (`//duckduckgo.com/l/?uddg=<encoded>`).
fn decode_ddg_url(href: &str) -> String {
    let encoded = href
        .find("uddg=")
        .map(|idx| {
            let rest = &href[idx + 5..];
            rest.split('&').next().unwrap_or(rest)
        })
        .unwrap_or(href);
    percent_decode(encoded)
}

/// Decodificación de percent-encoding (suficiente para URLs de resultados).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Codifica un query para la URL (`%20` para espacios).
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Formatea errores de la API Exa con mensajes amigables según el código HTTP.
fn format_exa_error(status: u16, body: &str) -> String {
    match status {
        0 => format!(
            "Exa search failed: network/timeout error. Check internet and EXA_API_KEY. Details: {body}"
        ),
        401 | 403 => format!(
            "Exa search failed: invalid or expired API key. Set a valid EXA_API_KEY. (HTTP {status})"
        ),
        429 => "Exa search failed: rate limited. Wait a moment and retry. (HTTP 429)".to_string(),
        _ => format!("Exa search error (HTTP {status}): {body}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_missing_query() {
        let tool = WebSearchTool::new();
        let result = tool.call(&json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument"));
    }

    #[tokio::test]
    async fn test_tool_schema() {
        let tool = WebSearchTool::new();
        assert_eq!(tool.name(), "web_search");
        let params = tool.parameters();
        assert!(params.get("properties").unwrap().get("query").is_some());
    }

    #[test]
    fn test_parse_ddg_html() {
        let html = r##"
        <html><body>
        <div class="result">
          <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&amp;rut=abc">Example Title</a></h2>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=...">This is a snippet about the page.</a>
        </div>
        <div class="result">
          <h2 class="result__title"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fotro.example.org">Segundo Resultado</a></h2>
          <a class="result__snippet" href="#">Otro snippet.</a>
        </div>
        </body></html>
        "##;
        let results = parse_ddg_html(html, 5);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "Example Title");
        assert_eq!(results[0]["url"], "https://example.com/page");
        assert!(
            results[0]["description"]
                .as_str()
                .unwrap()
                .contains("snippet")
        );
        assert_eq!(results[1]["title"], "Segundo Resultado");
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com%2Fa%20b"),
            "https://example.com/a b"
        );
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hola mundo"), "hola+mundo");
        assert_eq!(url_encode("a/b"), "a%2Fb");
    }
}
