//! # search_memory — Herramienta activa de búsqueda en memoria del agente
//!
//! Reemplaza el RAG pasivo FIFO por una herramienta que el LLM invoca
//! autónomamente cuando necesita contexto histórico. Implementa el
//! algoritmo de scoring híbrido:
//!
//! `Score = α × Similitud Coseno + β × Recencia Temporal + γ × Adyacencia en Grafo`
//!
//! El LLM controla dinámicamente el "zoom" semántico y cronológico a
//! través de parámetros: query, threshold, depth, max_tokens, strategy.

use crate::state::compressor::SemanticMatch;
use crate::state::session::SessionManager;
use crate::tools::{Tool, ToolResult};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use std::sync::Arc;

// ── Coeficientes por defecto ────────────────────────────────────────
const DEFAULT_ALPHA: f32 = 0.5; // Peso de similitud coseno
const DEFAULT_BETA: f32 = 0.3; // Peso de recencia temporal
const DEFAULT_GAMMA: f32 = 0.2; // Peso de adyacencia en grafo

/// Herramienta `search_memory`.
pub struct SearchMemoryTool {
    session: Arc<RwLock<SessionManager>>,
}

impl SearchMemoryTool {
    /// Crea una nueva instancia con una referencia compartida al
    /// SessionManager.
    #[must_use]
    pub fn new(session: Arc<RwLock<SessionManager>>) -> Self {
        Self { session }
    }

    /// Calcula el score híbrido para un resultado de búsqueda.
    ///
    /// # Hybrid Scoring Algorithm
    ///
    /// ```text
    /// score = α × cos_sim + β × recency + γ × adjacency
    /// ```
    ///
    /// donde:
    /// - `cos_sim`: similitud coseno devuelta por dogma-vdb (0.0 - 1.0)
    /// - `recency`: normalización temporal del campo `created_at`
    /// - `adjacency`: ratio de conexiones con otros resultados vía `parent_id`
    fn hybrid_score(
        match_: &SemanticMatch,
        neighbors: &[SemanticMatch],
        alpha: f32,
        beta: f32,
        gamma: f32,
    ) -> f32 {
        let cos_sim = match_.score; // Ya normalizado por dogma-vdb

        let recency = Self::recency_score(match_.created_at.as_deref());

        let adjacency = Self::adjacency_score(match_, neighbors);

        alpha * cos_sim + beta * recency + gamma * adjacency
    }

    /// Calcula el score de recencia basado en el timestamp ISO-8601.
    ///
    /// Returns 1.0 para el momento más reciente, decayendo hacia 0.0
    /// para tiempos más antiguos. Usa una escala logarítmica para
    /// evitar que mensajes de hace segundos tengan score muy diferente
    /// a los de hace minutos, mientras diferencia claramente entre
    /// horas recientes y días antiguos.
    fn recency_score(created_at: Option<&str>) -> f32 {
        let Some(ts) = created_at else {
            return 0.5; // Neutral si no hay timestamp
        };

        let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
            // Fallback: intentar parsear sin timezone offset (UTC implícito)
            let naive = match chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f"))
            {
                Ok(n) => n,
                Err(_) => return 0.5,
            };
            let utc: chrono::DateTime<chrono::Utc> =
                chrono::DateTime::from_naive_utc_and_offset(naive, chrono::Utc);
            return Self::recency_from_duration(utc);
        };

        Self::recency_from_duration(parsed.into())
    }

    /// Calcula recencia a partir de la diferencia con now.
    fn recency_from_duration(timestamp: chrono::DateTime<chrono::Utc>) -> f32 {
        let now = chrono::Utc::now();
        let duration = now.signed_duration_since(timestamp);

        let seconds = duration.num_seconds().max(0) as f32;

        // Escala logarítmica: 1.0 en t=0, ~0.5 a los 10 min, ~0.2 a las 2h
        // Fórmula: 1.0 / (1.0 + log2(1 + seconds / 60))
        if seconds < 1.0 {
            return 1.0;
        }
        let minutes = seconds / 60.0;
        1.0 / (1.0 + (1.0 + minutes).log2())
    }

    /// Calcula el score de adyacencia: qué tan conectado está un nodo
    /// con otros resultados por `parent_id`.
    ///
    /// Un nodo que comparte `parent_id` con muchos otros resultados es
    /// más "adyacente" (forma parte de un cluster denso).
    fn adjacency_score(node: &SemanticMatch, neighbors: &[SemanticMatch]) -> f32 {
        let Some(parent_id) = &node.parent_id else {
            return 0.0;
        };

        if parent_id.is_empty() || neighbors.is_empty() {
            return 0.0;
        }

        // Contar cuántos vecinos comparten el mismo parent_id
        let shared = neighbors
            .iter()
            .filter(|n| {
                n.node_id != node.node_id && n.parent_id.as_deref() == Some(parent_id)
            })
            .count();

        // Normalizar: ratio de vecinos que comparten parent
        shared as f32 / neighbors.len() as f32
    }

    /// Formatea los resultados como texto para el LLM, respetando
    /// el límite de `max_tokens` (aproximado como caracteres × 4).
    fn format_results(matches: &[&SemanticMatch], max_chars: usize) -> String {
        if matches.is_empty() {
            return "No relevant context found in memory.".to_string();
        }

        let mut output = String::new();
        output.push_str("── Relevant context from agent memory ──\n\n");

        for (i, m) in matches.iter().enumerate() {
            let label = match m.created_at.as_deref().unwrap_or("unknown") {
                ts if ts.len() >= 10 => {
                    // Intentar extraer fecha legible del ISO-8601
                    format!("[{}] #{}, score={:.2}", &ts[..10], i + 1, m.score)
                }
                ts => format!("[{}] #{}, score={:.2}", ts, i + 1, m.score),
            };

            let entry = format!("{label}\n{}\n\n", m.content);

            // Si agregar esta entrada excede el límite, parar
            if output.len() + entry.len() > max_chars {
                let remaining = max_chars.saturating_sub(output.len());
                if remaining > 20 {
                    let truncated = &m.content[..remaining.min(m.content.len())];
                    output.push_str(&format!("{label}\n{truncated}…\n\n"));
                }
                output.push_str(&format!(
                    "[… truncated at {} character limit]\n",
                    max_chars
                ));
                break;
            }

            output.push_str(&entry);
        }

        // Cerrar el bloque de contexto
        output.push_str("── End of relevant context ──");

        output
    }
}

#[async_trait]
impl Tool for SearchMemoryTool {
    fn name(&self) -> &'static str {
        "search_memory"
    }

    fn description(&self) -> &'static str {
        "Search the agent's memory for relevant context from the current \
         and past sessions. Uses hybrid scoring (cosine similarity, recency, \
         graph adjacency) to find the most relevant information. Call this \
         when you need to recall previous errors, user preferences, historical \
         decisions, or any information you may have forgotten."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural language query describing what you're looking for"
                },
                "session_id": {
                    "type": "string",
                    "description": "Optional session ID to scope the search. If omitted, searches across all sessions"
                },
                "threshold": {
                    "type": "number",
                    "description": "Minimum hybrid score (0.0–1.0) to include a result. Higher = stricter filtering",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 0.3
                },
                "depth": {
                    "type": "integer",
                    "description": "How many levels of graph adjacency to traverse. 0 = direct matches only",
                    "minimum": 0,
                    "maximum": 5,
                    "default": 0
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Maximum tokens of context to return (approximate, 1 token ≈ 4 chars)",
                    "minimum": 100,
                    "maximum": 32000,
                    "default": 2000
                },
                "strategy": {
                    "type": "string",
                    "enum": ["hybrid", "similarity", "recent", "connected"],
                    "description": "Scoring strategy: 'hybrid' (default, weighted combination), 'similarity' (pure cosine), 'recent' (recency-biased), 'connected' (adjacency-biased)",
                    "default": "hybrid"
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

        let session_id = args.get("session_id").and_then(Value::as_str);

        let threshold = args
            .get("threshold")
            .and_then(Value::as_f64)
            .map_or(0.3, |v| v.clamp(0.0, 1.0) as f32);

        let _depth = args
            .get("depth")
            .and_then(Value::as_i64)
            .map_or(0, |v| v.clamp(0, 5) as usize);

        let max_tokens = args
            .get("max_tokens")
            .and_then(Value::as_i64)
            .map_or(2000, |v| v.clamp(100, 32_000) as usize);

        let strategy = args
            .get("strategy")
            .and_then(Value::as_str)
            .unwrap_or("hybrid");

        // Calcular max_chars como 4× max_tokens (aproximación conservadora)
        let max_chars = max_tokens.saturating_mul(4);

        // Ejecutar búsqueda
        let session = self.session.read();
        let k = 50; // Buscar bastantes resultados para aplicar hybrid scoring después

        let raw_results = if let Some(sid) = session_id {
            session.search_similar(query, sid, k)
        } else {
            session.search_similar_global(query, k)
        }
        .unwrap_or_default();

        if raw_results.is_empty() {
            return Ok("No relevant context found in memory.".to_string());
        }

        // Aplicar scoring híbrido
        let alpha = match strategy {
            "similarity" => 1.0,
            "recent" => 0.0,
            "connected" => 0.0,
            _ => DEFAULT_ALPHA,
        };
        let beta = match strategy {
            "similarity" => 0.0,
            "recent" => 1.0,
            "connected" => 0.0,
            _ => DEFAULT_BETA,
        };
        let gamma = match strategy {
            "similarity" => 0.0,
            "recent" => 0.0,
            "connected" => 1.0,
            _ => DEFAULT_GAMMA,
        };

        let mut scored: Vec<(f32, &SemanticMatch)> = raw_results
            .iter()
            .map(|m| {
                let score = Self::hybrid_score(m, &raw_results, alpha, beta, gamma);
                (score, m)
            })
            .collect();

        // Ordenar por score descendente
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Filtrar por threshold
        scored.retain(|(score, _)| *score >= threshold);

        // Aplicar depth: adyacencia en grafo
        // Por ahora v1: depth > 0 agrega vecinos que compartan parent_id
        // (implementación completa de BFS vendrá en versión futura)
        if _depth > 0 {
            let parent_ids: Vec<Option<&str>> = raw_results
                .iter()
                .filter_map(|m| m.parent_id.as_deref())
                .map(Some)
                .collect();

            // Expandir: incluir nodos cuyos parent_id estén en el set
            // (implementación simplificada — expandir a profundidad 1)
            for m in &raw_results {
                let not_already_scored = !scored.iter().any(|(_, sm)| sm.node_id == m.node_id);
                if let Some(ref pid) = m.parent_id {
                    if parent_ids.contains(&Some(pid.as_str())) && not_already_scored {
                        let score = Self::hybrid_score(m, &raw_results, alpha, beta, gamma);
                        if score >= threshold {
                            scored.push((score, m));
                        }
                    }
                }
            }
            // Reordenar después de expandir
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        }

        // Extraer solo los SemanticMatch, ordenados
        let ordered: Vec<&SemanticMatch> = scored.into_iter().map(|(_, m)| m).collect();

        // Formatear y devolver
        let result = Self::format_results(&ordered, max_chars);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::session::SessionManager;

    fn make_match(
        id: &str,
        content: &str,
        score: f32,
        created_at: Option<&str>,
        parent_id: Option<&str>,
    ) -> SemanticMatch {
        SemanticMatch {
            node_id: id.to_string(),
            content: content.to_string(),
            score,
            session_id: "test-session".to_string(),
            created_at: created_at.map(String::from),
            parent_id: parent_id.map(String::from),
        }
    }

    #[test]
    fn test_recency_score_recent() {
        // Un timestamp muy reciente debería dar score cercano a 1.0
        let now = chrono::Utc::now();
        let ts = now.to_rfc3339();
        let score = SearchMemoryTool::recency_score(Some(&ts));
        assert!(
            score > 0.9,
            "Recent timestamp should score > 0.9, got {score}"
        );
    }

    #[test]
    fn test_recency_score_old() {
        // Un timestamp de hace varios días debería dar score bajo
        let old = chrono::Utc::now()
            - chrono::Duration::days(7);
        let ts = old.to_rfc3339();
        let score = SearchMemoryTool::recency_score(Some(&ts));
        assert!(
            score < 0.3,
            "Old timestamp should score < 0.3, got {score}"
        );
    }

    #[test]
    fn test_recency_score_none() {
        // Sin timestamp, debe devolver el valor neutral
        let score = SearchMemoryTool::recency_score(None);
        assert_eq!(score, 0.5);
    }

    #[test]
    fn test_adjacency_score_shared_parent() {
        let node = make_match("id-1", "hello", 0.5, None, Some("parent-1"));
        let neighbors = vec![
            make_match("id-2", "world", 0.5, None, Some("parent-1")),
            make_match("id-3", "foo", 0.5, None, Some("parent-2")),
        ];

        // id-1 comparte parent-1 con id-2 → 1 de 2 vecinos = 0.5
        let score = SearchMemoryTool::adjacency_score(&node, &neighbors);
        assert!((score - 0.5).abs() < f32::EPSILON, "Expected 0.5, got {score}");
    }

    #[test]
    fn test_adjacency_score_no_parent() {
        let node = make_match("id-1", "hello", 0.5, None, None);
        let neighbors = vec![make_match("id-2", "world", 0.5, None, None)];

        let score = SearchMemoryTool::adjacency_score(&node, &neighbors);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_adjacency_score_no_shared() {
        let node = make_match("id-1", "hello", 0.5, None, Some("parent-a"));
        let neighbors = vec![
            make_match("id-2", "world", 0.5, None, Some("parent-b")),
            make_match("id-3", "foo", 0.5, None, Some("parent-c")),
        ];

        let score = SearchMemoryTool::adjacency_score(&node, &neighbors);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_hybrid_score_default_weights() {
        let node = make_match("id-1", "content", 0.8, Some("2024-01-01T00:00:00Z"), Some("p1"));
        let neighbors = vec![
            make_match("id-2", "other", 0.5, Some("2024-01-01T00:00:00Z"), Some("p1")),
        ];

        // Default: α=0.5, β=0.3, γ=0.2
        // cos_sim=0.8, recency≈0.0 (very old), adjacency=1.0 (only neighbor shares parent)
        let score = SearchMemoryTool::hybrid_score(&node, &neighbors, 0.5, 0.3, 0.2);
        // ~ 0.5*0.8 + 0.3*0.0 + 0.2*1.0 = 0.4 + 0.0 + 0.2 = 0.6
        assert!((score - 0.6).abs() < 0.1, "Expected ~0.6, got {score}");
    }

    #[test]
    fn test_hybrid_score_pure_similarity() {
        let node = make_match("id-1", "content", 0.9, Some("2024-01-01T00:00:00Z"), None);
        let neighbors = vec![];

        // Pure similarity: α=1.0, β=0.0, γ=0.0
        let score = SearchMemoryTool::hybrid_score(&node, &neighbors, 1.0, 0.0, 0.0);
        assert!((score - 0.9).abs() < f32::EPSILON, "Expected 0.9, got {score}");
    }

    #[test]
    fn test_format_results_empty() {
        let result = SearchMemoryTool::format_results(&[], 1000);
        assert_eq!(result, "No relevant context found in memory.");
    }

    #[test]
    fn test_format_results_truncation() {
        let matches = vec![
            make_match("id-1", "A very long content that should be truncated by the formatter", 0.9, None, None),
        ];
        let refs: Vec<&SemanticMatch> = matches.iter().collect();
        let result = SearchMemoryTool::format_results(&refs, 20);
        // Should be truncated since content is longer than 20 chars
        assert!(result.contains("truncated"));
    }

    #[tokio::test]
    async fn test_missing_query() {
        let temp_dir = std::env::temp_dir().join(format!("dogma-test-{}", uuid::Uuid::new_v4()));
        let session_manager = SessionManager::open(&temp_dir).expect("open");
        let session = Arc::new(RwLock::new(session_manager));
        let tool = SearchMemoryTool::new(session);

        let result = tool.call(&serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing required argument: query"));
    }

    #[tokio::test]
    async fn test_search_empty_session() {
        let temp_dir = std::env::temp_dir().join(format!("dogma-test-{}", uuid::Uuid::new_v4()));
        let mut session_manager = SessionManager::open(&temp_dir).expect("open");
        let _sid = session_manager.create_session("test").expect("create");
        let session = Arc::new(RwLock::new(session_manager));
        let tool = SearchMemoryTool::new(session);

        let args = serde_json::json!({
            "query": "test query",
            "threshold": 0.0
        });
        let result = tool.call(&args).await;
        // Should succeed but return no results (no embedder configured)
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No relevant context found"));
    }

    #[test]
    fn test_parameters_schema() {
        let temp_dir = std::env::temp_dir().join(format!("dogma-test-{}", uuid::Uuid::new_v4()));
        let session_manager = SessionManager::open(&temp_dir).expect("open");
        let session = Arc::new(RwLock::new(session_manager));
        let tool = SearchMemoryTool::new(session);

        let params = tool.parameters();
        assert!(params.get("properties").is_some());
        assert!(params.get("required").is_some());
        assert!(params["required"].as_array().unwrap().contains(&serde_json::Value::String("query".to_string())));
    }
}
