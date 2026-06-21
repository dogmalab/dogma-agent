//! # Context Manager — Gestión semántica del contexto
//!
//! En lugar de comprimir el contexto (resumir con LLM), este módulo
//! usa búsqueda semántica sobre dogma-vdb para mantener solo los
//! mensajes más relevantes en el contexto activo.
//!
//! ## Flujo
//!
//! 1. Los últimos N turnos siempre se mantienen en contexto.
//! 2. El query del usuario se embedde y busca en dogma-vdb.
//! 3. Los mensajes más relevantes de sesiones pasadas se inyectan.
//! 4. Los mensajes antiguos NO se borran de dogma-vdb — solo se
//!    descartan del contexto activo (reversible).
//!
//! ## Diferencia con compresión tradicional
//!
//! - Compresión: resume → pierde detalle → irreversible
//! - Este enfoque: busca → selecciona → el detalle se mantiene en DB

use crate::runtime::provider::Message;
use crate::state::compressor::SemanticMatch;
use dogma_vdb::embedding::Embedder;
use tracing::{debug, info};

/// Configuración del context manager.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Número mínimo de turnos recientes que siempre se mantienen.
    pub recent_turns: usize,
    /// Número máximo de mensajes "relevantes" a inyectar del historial.
    pub max_relevant: usize,
    /// Umbral de similitud para considerar "relevante" (0.0–1.0).
    pub relevance_threshold: f32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            recent_turns: 5,
            max_relevant: 5,
            relevance_threshold: 0.3,
        }
    }
}

/// Context manager que usa búsqueda semántica para optimizar el contexto.
///
/// No comprime ni resume — simplemente selecciona qué mensajes del
/// historial son más relevantes para el query actual.
pub struct ContextManager {
    config: ContextConfig,
}

impl ContextManager {
    pub fn new(config: ContextConfig) -> Self {
        Self { config }
    }

    /// Construye el contexto óptimo para una consulta.
    ///
    /// Toma los mensajes recientes y el historial completo de dogma-vdb,
    /// y retorna una lista de mensajes que incluye:
    /// - Mensajes relevantes del historial (los más similares al query)
    /// - Los últimos N turnos (siempre presentes)
    ///
    /// # Arguments
    ///
    /// * `recent_messages` — Últimos turnos de la conversación actual
    /// * `session_id` — ID de la sesión actual
    /// * `current_query` — El query del usuario (para buscar similitud)
    /// * `embedder` — Para embeber el query
    /// * `search_fn` — Función de búsqueda (para desacoplar de SessionManager)
    ///
    /// # Returns
    ///
    /// Vec de `SemanticMatch` con los mensajes relevantes del historial.
    /// Estos se inyectan al contexto DESPUÉS del system prompt y ANTES
    /// de los mensajes recientes.
    pub fn build_context<F>(
        &self,
        _recent_messages: &[Message],
        _session_id: &str,
        current_query: &str,
        embedder: &dyn Embedder,
        search_fn: F,
    ) -> Result<Vec<SemanticMatch>, String>
    where
        F: Fn(&[f32], usize) -> Vec<SemanticMatch>,
    {
        // 1. No buscar si el query está vacío
        if current_query.trim().is_empty() {
            return Ok(Vec::new());
        }

        // 2. No buscar si no hay embedder
        let embedding = embedder.embed(current_query).map_err(|e| {
            format!("context manager: embedding failed: {e}")
        })?;

        if embedding.is_empty() {
            debug!("Context manager: embedder returned empty vector");
            return Ok(Vec::new());
        }

        // 3. Buscar mensajes relevantes en dogma-vdb
        let candidates = search_fn(&embedding, self.config.max_relevant * 2);

        // 4. Filtrar por umbral de relevancia
        let relevant: Vec<SemanticMatch> = candidates
            .into_iter()
            .filter(|m| m.score >= self.config.relevance_threshold)
            .take(self.config.max_relevant)
            .collect();

        info!(
            "Context manager: {} relevant messages found for query '{}'",
            relevant.len(),
            &current_query[..current_query.len().min(50)]
        );

        Ok(relevant)
    }

    /// Determina si el contexto necesita optimización.
    ///
    /// Returns `true` si hay más mensajes que `recent_turns * 2` (estimación
    /// de que el historial es lo suficientemente largo como para beneficiarse
    /// de la búsqueda semántica).
    pub fn should_optimize(&self, total_messages: usize) -> bool {
        total_messages > self.config.recent_turns * 2
    }

    /// Formatea los mensajes relevantes como contexto inyectable.
    ///
    /// Los mensajes se concatenan como un bloque de texto que se
    /// inyecta al system prompt o como mensajes separados.
    pub fn format_relevant_context(matches: &[SemanticMatch]) -> String {
        if matches.is_empty() {
            return String::new();
        }

        let mut context = String::from("Relevant context from past conversations:\n\n");
        for (i, m) in matches.iter().enumerate() {
            let preview: String = m.content.chars().take(200).collect();
            context.push_str(&format!(
                "[{}] (session: {}, score: {:.2})\n{}\n\n",
                i + 1,
                &m.session_id[..m.session_id.len().min(20)],
                m.score,
                preview,
            ));
        }
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::compressor::SemanticMatch;

    fn make_match(node_id: &str, content: &str, score: f32) -> SemanticMatch {
        SemanticMatch {
            node_id: node_id.to_string(),
            content: content.to_string(),
            score,
            session_id: "test-session".to_string(),
            created_at: None,
            parent_id: None,
        }
    }

    #[test]
    fn test_format_relevant_context_empty() {
        let ctx = ContextManager::format_relevant_context(&[]);
        assert!(ctx.is_empty());
    }

    #[test]
    fn test_format_relevant_context() {
        let matches = vec![
            make_match("msg-1", "Rust is fast and safe", 0.85),
            make_match("msg-2", "Python is ergonomic", 0.65),
        ];
        let ctx = ContextManager::format_relevant_context(&matches);
        assert!(ctx.contains("Rust is fast"));
        assert!(ctx.contains("Python is ergonomic"));
        assert!(ctx.contains("0.85"));
    }

    #[test]
    fn test_should_optimize() {
        let cm = ContextManager::new(ContextConfig {
            recent_turns: 5,
            ..Default::default()
        });
        assert!(!cm.should_optimize(5)); // 5 <= 5*2=10
        assert!(cm.should_optimize(15)); // 15 > 10
    }
}
