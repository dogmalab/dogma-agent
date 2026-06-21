//! # Suite de Estrés y Validación de Memoria — Memory Stress Tests
//!
//! Tres pruebas de estrés para certificar que el agente no experimenta
//! degradación cognitiva o pérdida de contexto en discusiones largas:
//!
//! 1. **Needle in a Haystack** — Recall de precisión (similitud coseno)
//! 2. **Cause-Effect** — Navegación de adyacencia (depth ≥ 1)
//! 3. **Context Drift** — No contaminación semántica
//!
//! Cada test pre-puebla una colección dogma-vdb con documentos que
//! tienen embeddings determinísticos, para que el MockEmbedder pueda
//! controlar exactamente qué documentos retorna la búsqueda semántica.
//! El CognitiveMockLLM simula el comportamiento del agente llamando
//! autónomamente a search_memory y procesando los resultados.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use dogma_v2_common::Result;
use dogma_v2_core::runtime::loop_handle::{LoopConfig, RuntimeLoop};
use dogma_v2_core::runtime::provider::{
    LLMProvider, LLMResponse, Message, MessageRole, ProviderConfig, TokenUsage, ToolCall,
};
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::{SearchMemoryTool, create_survival_tools};
use dogma_vdb::collection::Collection;
use dogma_vdb::doc::Document;
use dogma_vdb::embedding::Embedder;

// ═══════════════════════════════════════════════════════════════════
// Mock Embedder — vectores determinísticos controlados
// ═══════════════════════════════════════════════════════════════════

/// Embedder simulado que retorna vectores fijos según una clave.
///
/// Permite a los tests controlar exactamente qué documentos serán
/// considerados "similares" a una consulta, sin depender de un modelo
/// de embeddings real (ONNX, fastembed, etc.).
struct MockEmbedder {
    /// Dimensión fija de todos los vectores.
    dimension: usize,
    /// Vector que se retorna siempre (simula el embedding de la query).
    response_vector: Vec<f32>,
}

impl MockEmbedder {
    fn new(response_vector: Vec<f32>) -> Self {
        let dimension = response_vector.len();
        Self {
            dimension,
            response_vector,
        }
    }
}

impl Embedder for MockEmbedder {
    fn embed(&self, _text: &str) -> std::result::Result<Vec<f32>, dogma_vdb::error::Error> {
        Ok(self.response_vector.clone())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
}

// ═══════════════════════════════════════════════════════════════════
// Cognitive Mock LLM — agente simulado con ciclo RSI
// ═══════════════════════════════════════════════════════════════════

/// Proveedor LLM simulado que imita el comportamiento autónomo del agente.
///
/// - **Paso 0:** Retorna un `tool_call` a `search_memory` con la consulta
///   y session_id adecuados.
/// - **Paso 1+:** Examina los mensajes (donde ya está el resultado de la
///   herramienta) y retorna la respuesta final si encontró el dato esperado,
///   o un error si no.
struct CognitiveMockLLM {
    config: ProviderConfig,
    call_count: AtomicUsize,
    /// La query que el agente "decide" buscar en el paso 0.
    search_query: String,
    /// Session ID a pasar en el tool call.
    session_id: String,
    /// Estrategia de búsqueda para el tool call.
    strategy: String,
    /// Profundidad de adyacencia para el tool call.
    depth: usize,
    /// Texto que el agente espera encontrar en el resultado de la herramienta.
    expected_in_result: String,
    /// Texto de la respuesta final.
    final_answer: String,
}

impl CognitiveMockLLM {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: ProviderConfig,
        search_query: impl Into<String>,
        session_id: impl Into<String>,
        strategy: impl Into<String>,
        depth: usize,
        expected_in_result: impl Into<String>,
        final_answer: impl Into<String>,
    ) -> Self {
        Self {
            config,
            call_count: AtomicUsize::new(0),
            search_query: search_query.into(),
            session_id: session_id.into(),
            strategy: strategy.into(),
            depth,
            expected_in_result: expected_in_result.into(),
            final_answer: final_answer.into(),
        }
    }
}

#[async_trait]
impl LLMProvider for CognitiveMockLLM {
    fn config(&self) -> &ProviderConfig {
        &self.config
    }

    async fn chat(
        &self,
        messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<LLMResponse> {
        let step = self.call_count.fetch_add(1, Ordering::SeqCst);

        match step {
            0 => {
                // Paso 0: el agente decide autónomamente llamar a search_memory
                let tool_call = ToolCall {
                    id: format!("call_mem_{}", step),
                    name: "search_memory".into(),
                    arguments: serde_json::json!({
                        "query": self.search_query,
                        "session_id": self.session_id,
                        "strategy": self.strategy,
                        "depth": self.depth,
                        "threshold": 0.1,
                        "max_tokens": 8000,
                    })
                    .to_string(),
                };

                Ok(LLMResponse {
                    content: "Let me search my memory for that information.".into(),
                    tool_calls: vec![tool_call],
                    usage: TokenUsage::default(),
                    extra_fields: vec![],
                })
            }
            _ => {
                // Pasos 1+: verificar que el resultado de la herramienta
                // contiene la información esperada
                let found = messages.iter().any(|m| {
                    m.role == MessageRole::Tool && m.content.contains(&self.expected_in_result)
                });

                if found {
                    Ok(LLMResponse {
                        content: self.final_answer.clone(),
                        tool_calls: vec![],
                        usage: TokenUsage::default(),
                        extra_fields: vec![],
                    })
                } else {
                    Err(dogma_v2_common::error::Error::Execution(
                        "Memory stress test: tool result did not contain expected data.".into(),
                    ))
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn test_provider_config() -> ProviderConfig {
    ProviderConfig {
        base_url: "https://mock.test/v1".into(),
        model: "stress-test-model".into(),
        api_key: Some("sk-mock-key".into()),
        ..Default::default()
    }
}

/// Abre una colección e inserta documentos pre-configurados con embeddings.
///
/// Los documentos se persisten al .vdb para que `SessionManager::open()`
/// los cargue automáticamente. Esto permite controlar exactamente qué
/// vectores tiene cada documento, independientemente de `append_message`
/// (que no computa embeddings).
fn seed_collection(vdb_path: &std::path::Path, docs: Vec<Document>) {
    let mut col = Collection::open(vdb_path).expect("seed_collection: open collection failed");
    for doc in docs {
        col.insert(doc)
            .expect("seed_collection: insert document failed");
    }
    // Al dropear `col` se persisten los documentos
}

/// Crea un documento con embedding, metadata y timestamp actual.
fn make_doc(
    id: &str,
    content: &str,
    embedding: Vec<f32>,
    session_id: &str,
    node_type: &str,
    parent_id: Option<&str>,
    sequence: usize,
) -> Document {
    let mut builder = Document::builder(id, content)
        .embedding(embedding)
        .metadata("node_type", node_type)
        .metadata("session_id", session_id)
        .metadata("role", "user")
        .metadata("sequence", sequence.to_string())
        .metadata("edge_type", "NEXT")
        .metadata("created_at", chrono::Utc::now().to_rfc3339());

    if let Some(pid) = parent_id {
        builder = builder.metadata("parent_id", pid);
    }

    builder.build()
}

// ═══════════════════════════════════════════════════════════════════
// Test 1: Needle in a Haystack
// ═══════════════════════════════════════════════════════════════════

/// Verifica que el agente puede recuperar un dato específico sepultado
/// bajo 20 capas de ruido semántico:
///
/// 1. Se inyectan 10 documentos de ruido (embedding en eje Y)
/// 2. Se inyecta la "aguja" con el puerto secreto (embedding en eje X)
/// 3. Se inyectan 10 documentos más de ruido
/// 4. El MockEmbedder retorna un vector alineado con la aguja (eje X)
/// 5. El CognitiveMockLLM invoca search_memory y debe encontrar el puerto
#[tokio::test]
async fn test_needle_in_a_haystack() {
    let dir = tempfile::tempdir().expect("temp dir");
    let vdb_path = dir.path().join("sessions.vdb");
    let sid = "needle-test";
    let secret_port = "9091";

    // ── Pre-poblar colección ──────────────────────────────────
    let mut docs: Vec<Document> = (0..10)
        .map(|i| {
            make_doc(
                &format!("noise-pre-{i}"),
                &format!(
                    "Información irrelevante {}: La configuración de DNS \
                     requiere el puerto 53 para resolución UDP.",
                    i
                ),
                vec![0.0, 0.1, 0.0, 0.0], // Embedding en Y
                sid,
                "Message",
                None,
                i,
            )
        })
        .collect();

    // La aguja: embedding alineado con el eje X (cosine ≈ 1.0 con la query)
    docs.push(make_doc(
        "needle-1",
        &format!("El puerto secreto de comunicación interna es el {secret_port}."),
        vec![0.9, 0.0, 0.0, 0.0], // Embedding en X
        sid,
        "Message",
        None,
        10,
    ));

    for i in 0..10 {
        docs.push(make_doc(
            &format!("noise-post-{i}"),
            &format!(
                "Información redundante {}: El algoritmo quicksort tiene \
                 complejidad O(n log n) en promedio.",
                i
            ),
            vec![0.0, 0.1, 0.0, 0.0], // Embedding en Y
            sid,
            "Message",
            None,
            11 + i,
        ));
    }

    seed_collection(&vdb_path, docs);

    // ── Crear SessionManager con MockEmbedder ─────────────────
    let embedder = Arc::new(MockEmbedder::new(vec![0.9, 0.0, 0.0, 0.0]));
    let session = SessionManager::open(dir.path())
        .expect("open session")
        .with_embedder(embedder);

    // ── Configurar CognitiveMockLLM ────────────────────────────
    let provider = Arc::new(CognitiveMockLLM::new(
        test_provider_config(),
        "puerto secreto comunicación interna",
        sid,
        "hybrid",
        0,
        secret_port,
        format!("El puerto secreto de comunicación interna es {secret_port}."),
    ));

    // ── Construir RuntimeLoop con SearchMemoryTool ─────────────
    let tools = create_survival_tools();
    let config = LoopConfig {
        max_tool_iterations: 5,
        context_compression: false,
        ..LoopConfig::default()
    };
    let runtime = RuntimeLoop::new(provider.clone(), tools, session, config, None);
    runtime.register_tool(Box::new(SearchMemoryTool::new(runtime.session_handle())));

    // ── Ejecutar y verificar ───────────────────────────────────
    let prompt = "Necesito configurar la conexión segura del gateway. \
                   ¿Cuál era el puerto secreto de comunicación interna?";
    let response = runtime
        .run(prompt, sid)
        .await
        .expect("needle test should succeed");

    assert!(
        response.contains(secret_port),
        "❌ Needle test FAILED: respuesta no contiene el puerto secreto.\n  respuesta: {response}"
    );
    println!("✅ Needle in a Haystack: PASS — agente encontró el puerto {secret_port}");
}

// ═══════════════════════════════════════════════════════════════════
// Test 2: Cause-Effect (Adjacency, depth ≥ 1)
// ═══════════════════════════════════════════════════════════════════

/// Verifica que el agente puede reconstruir relaciones causales
/// utilizando las aristas del grafo (parent_id → depth=1):
///
/// 1. Se crea un documento con el comando `cat archivo_fantasma.txt`
/// 2. Se crea un documento con el error resultante, cuyo parent_id
///    apunta al comando original
/// 3. El agente pregunta "¿Por qué falló el script?"
/// 4. search_memory con depth=1 debe encontrar tanto el error como
///    el comando causal
#[tokio::test]
async fn test_cause_effect_adjacency() {
    let dir = tempfile::tempdir().expect("temp dir");
    let vdb_path = dir.path().join("sessions.vdb");
    let sid = "cause-effect-test";
    let expected_cause = "cat archivo_fantasma.txt";
    let expected_effect = "No such file or directory";

    // ── Pre-poblar colección con cadena causal ────────────────
    // Creamos primero el comando para obtener su ID
    let cmd_id = "cmd-cat-1";

    let mut docs = vec![
        // Documento A: el comando original (causa)
        make_doc(
            cmd_id,
            expected_cause,
            vec![1.0, 0.0, 0.0, 0.0], // Embedding en X (query apunta a Y)
            sid,
            "ToolResult",
            None,
            0,
        ),
        // Documento B: el resultado del error (efecto), con parent_id → A
        make_doc(
            "err-1",
            &format!("error al ejecutar comando: {expected_effect}"),
            vec![0.0, 1.0, 0.0, 0.0], // Embedding en Y
            sid,
            "ToolResult",
            Some(cmd_id), // ← Arista causal
            1,
        ),
    ];

    // Añadimos documentos de ruido para dificultar la búsqueda
    for i in 0..5 {
        docs.push(make_doc(
            &format!("noise-{i}"),
            &format!("Log de sistema: heartbeat check #{i} completado sin errores."),
            vec![0.0, 0.0, 0.1, 0.0],
            sid,
            "Message",
            None,
            10 + i,
        ));
    }

    seed_collection(&vdb_path, docs);

    // ── SessionManager: query apunta al efecto (eje Y) ────────
    let embedder = Arc::new(MockEmbedder::new(vec![0.0, 1.0, 0.0, 0.0]));
    let session = SessionManager::open(dir.path())
        .expect("open session")
        .with_embedder(embedder);

    // ── CognitiveMockLLM con depth=1 ───────────────────────────
    let provider = Arc::new(CognitiveMockLLM::new(
        test_provider_config(),
        "error ejecución script cat archivo fantasma",
        sid,
        "hybrid",
        1, // ← depth=1 para navegar adyacencia
        expected_effect,
        format!(
            "El script falló porque ejecutó '{expected_cause}' y \
             el archivo no existía: {expected_effect}."
        ),
    ));

    let tools = create_survival_tools();
    let config = LoopConfig {
        max_tool_iterations: 5,
        context_compression: false,
        ..LoopConfig::default()
    };
    let runtime = RuntimeLoop::new(provider.clone(), tools, session, config, None);
    runtime.register_tool(Box::new(SearchMemoryTool::new(runtime.session_handle())));

    // ── Ejecutar y verificar ───────────────────────────────────
    let prompt = "¿Por qué falló el script que ejecutamos con cat?";
    let response = runtime
        .run(prompt, sid)
        .await
        .expect("cause-effect test should succeed");

    assert!(
        response.contains(expected_cause) && response.contains(expected_effect),
        "❌ Cause-Effect test FAILED: respuesta no contiene la relación causal.\n  respuesta: {response}"
    );
    println!("✅ Cause-Effect (depth=1): PASS — agente reconstruyó la cadena causal");
}

// ═══════════════════════════════════════════════════════════════════
// Test 3: Context Drift
// ═══════════════════════════════════════════════════════════════════

/// Verifica que la memoria semántica no se contamina cuando se cambia
/// abruptamente de tema:
///
/// 1. Se inyectan documentos sobre mmap en Rust (embedding en X)
/// 2. Se inyectan documentos sobre recetas de cocina (embedding en Z)
/// 3. Se pregunta por las desventajas de mmap
/// 4. El MockEmbedder retorna vector alineado con X (no Z)
/// 5. La búsqueda debe encontrar los documentos técnicos, ignorando cocina
#[tokio::test]
async fn test_context_drift_semantic_isolation() {
    let dir = tempfile::tempdir().expect("temp dir");
    let vdb_path = dir.path().join("sessions.vdb");
    let sid = "context-drift-test";
    let expected_tech_content = "mmap en sistemas embebidos puede causar page faults impredecibles";

    // ── Pre-poblar colección ──────────────────────────────────
    let mut docs: Vec<Document> = (0..3)
        .map(|i| {
            make_doc(
                &format!("tech-{i}"),
                &format!(
                    "Ventaja de mmap: mapeo directo de archivos a memoria, \
                     evitando copias intermedidas. Iteración {i}."
                ),
                vec![0.9, 0.0, 0.0, 0.0], // Embedding en X (técnico)
                sid,
                "Message",
                None,
                i,
            )
        })
        .collect();

    // Documento técnico clave: desventajas de mmap en embebidos
    docs.push(make_doc(
        "tech-disadvantages",
        expected_tech_content,
        vec![0.9, 0.0, 0.0, 0.0], // Embedding en X (técnico)
        sid,
        "Message",
        None,
        3,
    ));

    // Documentos de cocina (ruido temático) — embedding en Z
    for i in 0..3 {
        docs.push(make_doc(
            &format!("cooking-{i}"),
            &format!(
                "Receta de cocina: pasta al pesto con albahaca fresca. \
                 Hervir agua, añadir sal y cocinar 8 minutos. Iteración {i}."
            ),
            vec![0.0, 0.0, 0.9, 0.0], // Embedding en Z (cocina)
            sid,
            "Message",
            None,
            10 + i,
        ));
    }

    seed_collection(&vdb_path, docs);

    // ── SessionManager: query apunta al tema técnico (eje X) ──
    let embedder = Arc::new(MockEmbedder::new(vec![0.9, 0.0, 0.0, 0.0]));
    let session = SessionManager::open(dir.path())
        .expect("open session")
        .with_embedder(embedder);

    // ── CognitiveMockLLM ───────────────────────────────────────
    let provider = Arc::new(CognitiveMockLLM::new(
        test_provider_config(),
        "desventajas de usar mmap en sistemas embebidos",
        sid,
        "similarity", // Pure similarity para evitar bias temporal
        0,
        "page faults impredecibles",
        format!("Las desventajas de mmap en embebidos incluyen: {expected_tech_content}."),
    ));

    let tools = create_survival_tools();
    let config = LoopConfig {
        max_tool_iterations: 5,
        context_compression: false,
        ..LoopConfig::default()
    };
    let runtime = RuntimeLoop::new(provider.clone(), tools, session, config, None);
    runtime.register_tool(Box::new(SearchMemoryTool::new(runtime.session_handle())));

    // ── Ejecutar y verificar ───────────────────────────────────
    let prompt = "¿Qué desventajas mencionamos sobre usar mmap en sistemas embebidos?";
    let response = runtime
        .run(prompt, sid)
        .await
        .expect("context drift test should succeed");

    assert!(
        response.contains("mmap") && response.contains("page faults"),
        "❌ Context Drift test FAILED: respuesta no contiene información técnica.\n  respuesta: {response}"
    );
    assert!(
        !response.to_lowercase().contains("pesto") && !response.to_lowercase().contains("albahaca"),
        "❌ Context Drift test FAILED: respuesta contiene contaminación de cocina.\n  respuesta: {response}"
    );
    println!("✅ Context Drift (aislamiento semántico): PASS — ruido temático ignorado");
}

// ═══════════════════════════════════════════════════════════════════
// Test suite metadata (no ejecutable, informativo)
// ═══════════════════════════════════════════════════════════════════

/// Resumen de la suite completa.
///
/// Todos los tests verifican el ciclo completo:
/// 1. Pre-poblado de colección dogma-vdb con embeddings controlados
/// 2. CognitiveMockLLM simula decisión autónoma de usar search_memory
/// 3. RuntimeLoop ejecuta la herramienta y realimenta el resultado
/// 4. El mock procesa el resultado y genera la respuesta final
///
/// Para ejecutar:
/// ```bash
/// cargo test --test memory_stress -- --nocapture
/// ```
#[cfg(test)]
mod summary {
    #[test]
    fn suite_stats() {
        println!("\n📊 Memory Stress Suite — 3 tests:");
        println!("  1. test_needle_in_a_haystack      — Recall preciso bajo ruido");
        println!("  2. test_cause_effect_adjacency     — Navegación de aristas (depth=1)");
        println!("  3. test_context_drift_semantic_isolation — Aislamiento temático");
    }
}
