//! # dogma-v2-cli — Interfaz de control por terminal
//!
//! CLI principal del agente Dogma 2.0.
//!
//! ## Comandos
//!
//! * `dogma init` — Inicializa el entorno y levanta los mapas de
//!   memoria de dogma-vdb.
//! * `dogma chat "<prompt>"` — Ejecución rápida de una interacción.
//! * `dogma interactive [prompt]` — Modo interactivo con UI en terminal.
//!   Soporta historial de input (Up/Down), multi-línea (Ctrl+J),
//!   scroll del chat (PageUp/PageDown), y slash commands.
//! * `dogma plan "<task>"` — Planificación estructurada de tareas.
//!
//! ## Herramientas del agente
//!
//! El agente tiene acceso a las siguientes herramientas:
//! * `read_file`, `write_file`, `execute_script` — operaciones básicas
//! * `search_memory` — búsqueda semántica en sesiones pasadas
//! * `update_user_memory` — guardar/recuperar preferencias del usuario
//! * `plan` — crear planes estructurados para tareas complejas
//! * `delegate_task` — spawn sub-agentes para ejecución aislada
//! * `install_skill` — instalar skills dinámicas desde skills.sh
//! * `web_search`, `web_extract` — búsqueda/extracción web (DuckDuckGo sin key; Exa si EXA_API_KEY)
//!
//! ## Memoria
//!
//! El agente mantiene 4 capas de memoria:
//! 1. **Session Context** — historial de conversación (dogma-vdb)
//! 2. **User Memory** — preferencias y datos del usuario (persistente)
//! 3. **System Context** — OS, project, git (auto-detectado)
//! 4. **Context Manager** — selección semántica de contexto relevante
//!
//! ## Flag `--json`
//!
//! Si está presente, silencia el output humano de `stdout` y escupe
//! exclusivamente el stream de eventos NDJSON línea por línea.

use std::collections::VecDeque;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use dogma_v2_common::Result;
use dogma_v2_common::event::{Event, EventSeverity, EventType};
use dogma_v2_core::RuntimeLoop;
use dogma_v2_core::models::delegation::{AgentRole, SubAgentConfig};
use dogma_v2_core::models::events::AgentEvent;
use dogma_v2_core::models::plan::Plan;
use dogma_v2_core::runtime::cost_gate::{
    AutoCostGate, CostGateImpl, InteractiveCostGate, TrustedCostGate,
};
use dogma_v2_core::runtime::enriched::{MoaConfig, MoaLoop};
use dogma_v2_core::runtime::loop_handle::LoopConfig;
use dogma_v2_core::runtime::provider::embedder::HttpEmbedder;
use dogma_v2_core::runtime::provider::openai::OpenAiProvider;
use dogma_v2_core::runtime::provider::{LLMProvider, Message, MessageRole};
use dogma_v2_core::runtime::sub_agent::SubAgentManager;
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::state::user_memory::UserMemory;
use dogma_v2_core::state::workspace::{WorkspaceIndexer, open_workspace_collection};
use dogma_v2_core::tools::DelegateTaskTool;
use dogma_v2_core::tools::InstallSkillTool;
use dogma_v2_core::tools::PlanTool;
use dogma_v2_core::tools::SearchMemoryTool;
use dogma_v2_core::tools::UpdateUserMemoryTool;
use dogma_v2_core::tools::create_survival_tools;
use dogma_v2_core::tools::{SandboxMode, SecurityConfig, SecurityMode, ToolGuardrail};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod config;
mod ui;

/// Dogma 2.0 — Agente IA minimalista con estado en dogma-vdb.
#[derive(Parser, Debug)]
#[command(name = "dogma", version, about, long_about = None)]
struct Cli {
    /// Activa el modo JSON: solo emite eventos NDJSON por stdout.
    #[arg(long, global = true)]
    json: bool,

    /// Directorio de datos (por defecto ~/.dogma).
    #[arg(long, default_value = "~/.dogma")]
    data_dir: String,

    /// Modo del sandbox WASI para virtualizar ejecución de scripts.
    /// Valores: disabled (default), enabled, wasm-only.
    #[arg(long, default_value = "disabled")]
    sandbox_mode: String,

    /// Reanuda una sesión existente por su ID en vez de crear una nueva.
    #[arg(long, global = true)]
    session: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Inicializa el entorno y levanta los mapas de memoria de dogma-vdb.
    Init,

    /// Ejecución rápida de una interacción con el agente.
    Chat {
        /// Prompt de entrada para el agente.
        prompt: String,
    },

    /// Inicia el modo interactivo con UI dinámica en línea.
    Interactive {
        /// Prompt inicial opcional para comenzar la sesión.
        initial_prompt: Option<String>,
    },

    /// Genera un plan estructurado de tareas con el LLM y lo persiste.
    Plan {
        /// Descripción de la tarea a planificar.
        task: String,
    },

    /// Ejecuta Inferencia Enriquecida: N LLMs en paralelo con un
    /// compiler que sintetiza. Gated por Cost Gate.
    EnrichedInference {
        /// Query para los proposers.
        query: String,
        /// Modelo del compiler (opcional; por defecto el más fuerte disponible).
        #[arg(long)]
        compiler: Option<String>,
        /// Número de proposers en paralelo (default 3).
        #[arg(long, default_value_t = 3)]
        n_proposers: usize,
        /// Número de iteraciones del compiler (default 2).
        #[arg(long, default_value_t = 2)]
        iterations: usize,
        /// Modo del Cost Gate: interactive, auto, trusted.
        #[arg(long, default_value = "interactive")]
        gate: String,
    },

    /// Lista las sesiones existentes.
    Sessions,

    /// Indexa el workspace actual (o la ruta dada) con SML en workspace.vdb.
    Index {
        /// Ruta raíz a indexar (por defecto el directorio actual).
        #[arg(long)]
        path: Option<String>,
    },

    /// Exporta datos de sesiones a JSONL para inspección/debug.
    Export {
        /// Directorio de datos del agente.
        #[arg(short, long)]
        output: String,
    },
}

fn main() {
    let cli = Cli::parse();

    // En modo interactivo, silenciar tracing (conflictúa con la UI)
    let is_interactive = matches!(cli.command, Commands::Interactive { .. });
    let default_filter = if is_interactive { "error" } else { "info" };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Ejecutar el comando
    let runtime = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    let result = runtime.block_on(async { run(cli).await });

    if let Err(e) = result {
        emit_event(
            false,
            &Event::new(EventType::Error, EventSeverity::Fatal, e.to_string()),
        );
        std::process::exit(1);
    }
}

/// Ejecuta el comando solicitado.
async fn run(cli: Cli) -> Result<()> {
    let data_dir = resolve_data_dir(&cli.data_dir)?;

    // Parsear sandbox mode del flag CLI
    let sandbox_mode: SandboxMode = cli.sandbox_mode.parse().map_err(|e| {
        dogma_v2_common::error::Error::Validation(format!("invalid --sandbox-mode: {e}"))
    })?;

    match cli.command {
        Commands::Init => cmd_init(&data_dir, cli.json, sandbox_mode).await,
        Commands::Chat { prompt } => {
            cmd_chat(
                &data_dir,
                &prompt,
                cli.json,
                sandbox_mode,
                cli.session.as_deref(),
            )
            .await
        }
        Commands::Interactive { initial_prompt } => {
            cmd_interactive(
                &data_dir,
                initial_prompt.as_deref(),
                cli.json,
                sandbox_mode,
                cli.session.as_deref(),
            )
            .await
        }
        Commands::Plan { task } => cmd_plan(&data_dir, &task, cli.json, sandbox_mode).await,
        Commands::EnrichedInference {
            query,
            compiler,
            n_proposers,
            iterations,
            gate,
        } => {
            cmd_enriched_inference(
                &data_dir,
                &query,
                compiler.as_deref(),
                n_proposers,
                iterations,
                &gate,
                cli.json,
            )
            .await
        }
        Commands::Sessions => cmd_sessions(&data_dir).await,
        Commands::Index { path } => cmd_index(&data_dir, path.as_deref()).await,
        Commands::Export { output } => cmd_export(&data_dir, &output).await,
    }
}

/// Resuelve el directorio de datos, expandiendo `~` al home del usuario.
fn resolve_data_dir(raw: &str) -> Result<PathBuf> {
    if raw.starts_with('~') {
        let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
        let home = home.map_err(|_| {
            dogma_v2_common::error::Error::Internal("cannot determine home directory".into())
        })?;
        let stripped = raw.strip_prefix('~').unwrap_or("");
        Ok(PathBuf::from(home).join(stripped.strip_prefix('/').unwrap_or("")))
    } else {
        Ok(PathBuf::from(raw))
    }
}

/// Inicializa el entorno: crea directorios y prepara dogma-vdb.
async fn cmd_init(data_dir: &PathBuf, json_mode: bool, sandbox_mode: SandboxMode) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            "Initializing Dogma 2.0 environment",
        ),
    );

    // Crear directorio de datos
    std::fs::create_dir_all(data_dir).map_err(|e| dogma_v2_common::error::Error::Io {
        path: data_dir.clone(),
        source: e,
    })?;

    // Inicializar seguridad con el sandbox mode del CLI
    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![
            data_dir.clone(),
            std::env::current_dir().unwrap_or_default(),
        ],
        sandbox_mode,
        sandbox_limits: None,
    });

    // Inicializar session manager (crea sessions.vdb)
    let _session = SessionManager::open(data_dir)?;

    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Success,
            format!("Dogma 2.0 initialized at {}", data_dir.display()),
        ),
    );

    Ok(())
}

/// Exporta sesiones a JSONL para debug/inspección.
async fn cmd_export(data_dir: &PathBuf, output: &str) -> Result<()> {
    let session = SessionManager::open(data_dir)?;
    let col = session.collection();
    col.export_jsonl(output)
        .map_err(|e| dogma_v2_common::error::Error::Internal(format!("export failed: {e}")))?;
    println!("Exported session data to {output}");
    Ok(())
}

/// Ejecuta una interacción rápida de chat.
async fn cmd_chat(
    data_dir: &PathBuf,
    prompt: &str,
    json_mode: bool,
    sandbox_mode: SandboxMode,
    resume_session: Option<&str>,
) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            "Starting chat session",
        ),
    );

    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;

    let bundle = setup_runtime(data_dir, &dogma_config, sandbox_mode, resume_session, None).await?;

    let session_id = bundle.session_id;

    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            format!("Session: {session_id}"),
        )
        .with_session_id(&session_id),
    );

    let response = bundle.runtime.run(prompt, &session_id).await?;

    emit_event(
        json_mode,
        &Event::new(EventType::Message, EventSeverity::Success, &response)
            .with_session_id(&session_id)
            .with_metadata("role", "assistant"),
    );

    emit_event(
        json_mode,
        &Event::new(
            EventType::Done,
            EventSeverity::Success,
            "Chat session completed",
        )
        .with_session_id(&session_id),
    );

    if !json_mode {
        println!();
        println!("{response}");
    }

    Ok(())
}

/// Bundle de runtime completo: loop + sesión activa.
struct RuntimeBundle {
    runtime: Arc<RuntimeLoop>,
    session_id: String,
}

/// Construye el runtime con todas las herramientas, memoria de usuario,
/// workspace indexado (SML) y sesión (nueva o reanudada).
///
/// Herramientas registradas:
/// - Siempre: read_file, write_file, execute_script, search_memory,
///   update_user_memory, plan, delegate_task, web_search, web_extract
/// - Con skills.sh: install_skill
async fn setup_runtime(
    data_dir: &PathBuf,
    config: &config::DogmaConfig,
    sandbox_mode: SandboxMode,
    resume_session: Option<&str>,
    event_tx: Option<mpsc::Sender<AgentEvent>>,
) -> Result<RuntimeBundle> {
    // ── 1. Proveedor LLM ────────────────────────────────────────────
    let provider = Arc::new(OpenAiProvider::new(config.provider.clone())?);

    // ── 2. Embedder opcional (búsqueda semántica) ───────────────────
    let embedder: Option<Arc<dyn dogma_vdb::embedding::Embedder>> =
        HttpEmbedder::build_optional(&config.provider, config.embedding_model.as_deref())
            .map(|e| Arc::new(e) as Arc<dyn dogma_vdb::embedding::Embedder>);

    // ── 3. Session manager + sesión (nueva o reanudada) ─────────────
    let mut session = SessionManager::open(data_dir)?;
    if let Some(e) = embedder.as_ref() {
        session = session.with_embedder(Arc::clone(e));
    }

    let session_id = match resume_session {
        Some(id) => {
            if !session_exists(&session, id) {
                return Err(dogma_v2_common::error::Error::Validation(format!(
                    "session not found: {id}. List with `dogma sessions`."
                )));
            }
            id.to_string()
        }
        None => session.create_session(&config.provider.model)?,
    };

    // Plan activo si reanudamos una sesión con plan persistido
    let active_plan: Option<Plan> = if resume_session.is_some() {
        session
            .get_plans(&session_id)
            .unwrap_or_default()
            .into_iter()
            .next()
    } else {
        None
    };

    // ── 4. Seguridad ────────────────────────────────────────────────
    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![
            data_dir.clone(),
            std::env::current_dir().unwrap_or_default(),
        ],
        sandbox_mode,
        sandbox_limits: None,
    });

    // ── 5. Workspace collection (SML) ───────────────────────────────
    let workspace = match &embedder {
        Some(e) => {
            let mut col = open_workspace_collection(data_dir)?;
            if col.is_empty() {
                let cwd = std::env::current_dir().unwrap_or_default();
                info!(
                    "Indexing workspace at {} (first run) — run `dogma index` to refresh",
                    cwd.display()
                );
                let indexer = WorkspaceIndexer::new(Arc::clone(e));
                indexer.index_dir(&cwd, &mut col);
            }
            Some(Arc::new(RwLock::new(col)))
        }
        None => {
            warn!("No embedding model configured — semantic search and workspace context disabled");
            None
        }
    };

    // ── 6. User memory ──────────────────────────────────────────────
    let user_memory = Arc::new(RwLock::new(UserMemory::open(
        &data_dir.join("user_memory.vdb"),
    )?));
    let user_memory_handle = Arc::clone(&user_memory);

    // ── 7. RuntimeLoop ──────────────────────────────────────────────
    let loop_config = LoopConfig {
        max_tool_iterations: config.max_tool_iterations,
        context_window: config.context_window,
        ..LoopConfig::default()
    };

    let mut runtime = RuntimeLoop::new(
        provider.clone(),
        create_survival_tools(),
        session,
        loop_config,
        event_tx,
    );

    if let Some(plan) = active_plan {
        runtime.set_active_plan(plan);
    }

    runtime = runtime.with_user_memory(user_memory);
    if let Some(ws) = workspace {
        runtime = runtime.with_workspace_collection(ws);
    }

    let runtime = Arc::new(runtime);

    // ── 8. Registrar tools condicionales ────────────────────────────
    let memory_search = SearchMemoryTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(memory_search));

    let plan_tool = PlanTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(plan_tool));

    let um_tool = UpdateUserMemoryTool::new(user_memory_handle);
    runtime.register_tool(Box::new(um_tool));
    info!("UpdateUserMemoryTool registered");

    match InstallSkillTool::new(provider.clone(), data_dir) {
        Ok(skill_tool) => {
            runtime.register_tool(Box::new(skill_tool));
            info!("InstallSkillTool registered");
        }
        Err(e) => warn!("Failed to register InstallSkillTool: {e}"),
    }

    let subagent_config = SubAgentConfig {
        role: AgentRole::Orchestrator,
        max_spawn_depth: 2,
        max_iterations: 5,
        ..SubAgentConfig::default()
    };
    let subagent_mgr = SubAgentManager::new(Arc::clone(&runtime), subagent_config);
    let delegate_tool = DelegateTaskTool::new(Arc::new(subagent_mgr));
    runtime.register_tool(Box::new(delegate_tool));
    info!("DelegateTaskTool registered");

    runtime.register_tool(Box::new(dogma_v2_core::tools::WebSearchTool::new()));
    runtime.register_tool(Box::new(dogma_v2_core::tools::WebExtractTool::new()));
    info!("Web tools registered (DuckDuckGo por defecto; Exa si EXA_API_KEY está definida)");

    Ok(RuntimeBundle {
        runtime,
        session_id,
    })
}

/// Comprueba si una sesión existe (tiene nodo raíz `Session`).
fn session_exists(session: &SessionManager, id: &str) -> bool {
    session
        .get_session_nodes(id)
        .map(|nodes| {
            nodes
                .iter()
                .any(|d| d.metadata_val("node_type") == Some("Session"))
        })
        .unwrap_or(false)
}

/// Prompt del planificador: fuerza salida JSON con los pasos.
const PLAN_SYSTEM_PROMPT: &str = "You are a meticulous planning assistant. \
Given a task, break it into 3-8 clear, actionable, sequential steps. \
Return ONLY a JSON object with a \"steps\" array of strings, \
e.g. {\"steps\": [\"step one\", \"step two\"]}. No prose, no markdown.";

/// Inicia el modo estructurado de planificación con el LLM real.
async fn cmd_plan(
    data_dir: &PathBuf,
    task: &str,
    json_mode: bool,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Info, "Starting plan mode"),
    );

    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;
    let provider = Arc::new(OpenAiProvider::new(dogma_config.provider.clone())?);

    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2-planner")?;

    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![
            data_dir.clone(),
            std::env::current_dir().unwrap_or_default(),
        ],
        sandbox_mode,
        sandbox_limits: None,
    });

    emit_event(
        json_mode,
        &Event::new(
            EventType::PlanProgress,
            EventSeverity::Info,
            format!("Planning task: {task}"),
        )
        .with_session_id(&session_id),
    );

    // ── Generar plan con el LLM ────────────────────────────────────
    let messages = vec![
        Message::new(MessageRole::System, PLAN_SYSTEM_PROMPT),
        Message::new(MessageRole::User, task),
    ];

    let response = provider.chat(&messages, &[]).await?;
    let steps = parse_plan_steps(&response.content);

    if steps.is_empty() {
        return Err(dogma_v2_common::error::Error::Internal(format!(
            "LLM returned no plan steps for task: {task}"
        )));
    }

    let plan = Plan::new(task, &steps);
    session.save_plan(&session_id, &plan)?;

    let plan_text = plan.format_display();

    emit_event(
        json_mode,
        &Event::new(EventType::PlanProgress, EventSeverity::Success, &plan_text)
            .with_session_id(&session_id)
            .with_metadata("plan_id", &plan.id),
    );

    emit_event(
        json_mode,
        &Event::new(
            EventType::Done,
            EventSeverity::Success,
            format!("Plan saved to session {session_id}"),
        )
        .with_session_id(&session_id),
    );

    if !json_mode {
        println!("{plan_text}");
        println!();
        println!("Session: {session_id}");
    }

    Ok(())
}

/// Ejecuta Inferencia Enriquecida (MoA) con N proposers y un compiler.
///
/// Por MVP, todos los proposers usan el mismo provider/modelo
/// configurado en `~/.dogma/config.toml`. El compiler puede
/// especificarse via `--compiler <model>`; si no, usa el mismo
/// modelo que los proposers.
///
/// El Cost Gate es siempre obligatorio: el usuario ve el estimado
/// de costo en USD, tokens y wall-time antes de gastar.
#[allow(clippy::too_many_lines)]
async fn cmd_enriched_inference(
    data_dir: &PathBuf,
    query: &str,
    compiler_model: Option<&str>,
    n_proposers: usize,
    iterations: usize,
    gate_kind: &str,
    json_mode: bool,
) -> Result<()> {
    use dogma_v2_core::runtime::provider::LLMProvider;

    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            format!(
                "Starting Enriched Inference: {n_proposers} proposers, {iterations} iters, gate={gate_kind}"
            ),
        ),
    );

    // ── 1. Cargar configuración y construir proposers + compiler ────
    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;

    // Por MVP, todos los proposers usan el mismo provider/modelo
    // configurado en `keys.toml`. Cuando se agregue soporte para
    // múltiples modelos por proposer, esto se reemplaza por una
    // lista explícita de configs.
    let mut proposers: Vec<Arc<dyn LLMProvider>> = Vec::new();
    for _ in 0..n_proposers {
        let proposer = Arc::new(OpenAiProvider::new(dogma_config.provider.clone())?);
        proposers.push(proposer);
    }

    let mut compiler_cfg = dogma_config.provider.clone();
    if let Some(model) = compiler_model {
        compiler_cfg.model = model.to_string();
    }
    let compiler: Arc<dyn LLMProvider> = Arc::new(OpenAiProvider::new(compiler_cfg)?);

    // ── 2. Construir el Cost Gate ───────────────────────────────────
    let gate: Arc<dyn CostGateImpl> = match gate_kind {
        "auto" => Arc::new(AutoCostGate { max_cost_usd: 1.0 }),
        "trusted" => Arc::new(TrustedCostGate),
        _ => Arc::new(InteractiveCostGate), // default: interactive
    };

    // ── 3. Construir el MoaLoop ─────────────────────────────────────
    let moa_config = MoaConfig {
        n_proposers,
        max_iterations: iterations,
        compiler: None,
        enable_verification_skills: false,
        ..MoaConfig::default()
    };

    let session = Arc::new(parking_lot::RwLock::new(
        SessionManager::open(data_dir)
            .map_err(|e| dogma_v2_common::error::Error::StorageCorrupted(e.to_string()))?,
    ));

    let moa = MoaLoop::new(proposers, compiler, gate, moa_config).with_session(session);

    // ── 4. Ejecutar ─────────────────────────────────────────────────
    let result = moa.run(query).await?;

    // ── 5. Emitir resultado ─────────────────────────────────────────
    if json_mode {
        emit_event(
            json_mode,
            &Event::new(
                EventType::Done,
                EventSeverity::Success,
                serde_json::to_string(&result).unwrap_or_default(),
            ),
        );
    } else {
        println!();
        println!("─── Enriched Inference Result ───");
        println!("Iterations: {}", result.iterations.len());
        println!("Total wall-time: {}ms", result.total_wall_time_ms);
        println!(
            "Quality estimate: {:.2} (basis: {:?})",
            result.quality.expected_score, result.quality.basis
        );
        println!(
            "Cost: ${:.4} (expected) — see session.vdb for breakdown",
            result.cost.total_estimate.expected_cost_usd
        );
        println!();
        println!("Final answer:");
        println!("{}", result.final_text);
    }

    Ok(())
}

/// Extrae los pasos del plan de la respuesta del LLM.
///
/// Intenta parsear JSON `{"steps": [...]}`; si falla, parsea líneas
/// numeradas o bullet points.
fn parse_plan_steps(content: &str) -> Vec<String> {
    // Intento 1: JSON
    if let Some(start) = content.find('{') {
        if let Some(end) = content.rfind('}') {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content[start..=end]) {
                if let Some(arr) = value.get("steps").and_then(serde_json::Value::as_array) {
                    let steps: Vec<String> = arr
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect();
                    if !steps.is_empty() {
                        return steps;
                    }
                }
            }
        }
    }

    // Intento 2: líneas numeradas / bullets
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| {
            line.starts_with(|c: char| c.is_ascii_digit())
                || line.starts_with('-')
                || line.starts_with('*')
        })
        .map(|line| {
            line.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(['.', ')', '-', '*'])
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Lista las sesiones existentes.
async fn cmd_sessions(data_dir: &PathBuf) -> Result<()> {
    let session = SessionManager::open(data_dir)?;
    let sessions = session.list_sessions();

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    for (id, model, created, count) in sessions {
        println!("{id}\t[{model}]\t{count} nodes\t{created}");
    }

    Ok(())
}

/// Indexa el workspace actual (o la ruta dada) con SML en workspace.vdb.
///
/// Siempre reconstruye el índice desde cero para evitar duplicados.
async fn cmd_index(data_dir: &std::path::Path, path: Option<&str>) -> Result<()> {
    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;

    let embedder = HttpEmbedder::build_optional(
        &dogma_config.provider,
        dogma_config.embedding_model.as_deref(),
    )
    .ok_or_else(|| {
        dogma_v2_common::error::Error::Validation(
            "No embedding model configured. Set DOGMA_EMBEDDING_MODEL or \
             [provider] embedding_model in keys.toml"
                .into(),
        )
    })?;

    let root = match path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_default(),
    };
    if !root.is_dir() {
        return Err(dogma_v2_common::error::Error::Validation(format!(
            "not a directory: {}",
            root.display()
        )));
    }

    // Reconstrucción limpia: borrar el índice anterior
    let vdb_path = data_dir.join("workspace.vdb");
    if vdb_path.exists() {
        std::fs::remove_file(&vdb_path).map_err(|e| dogma_v2_common::error::Error::Io {
            path: vdb_path.clone(),
            source: e,
        })?;
    }

    let mut collection = open_workspace_collection(data_dir)?;
    let indexer = WorkspaceIndexer::new(Arc::new(embedder));
    let count = indexer.index_dir(&root, &mut collection);

    if count == 0 {
        return Err(dogma_v2_common::error::Error::Internal(format!(
            "no files indexed under {} (check extensions/size)",
            root.display()
        )));
    }

    println!(
        "Indexed {count} chunks from {} into {}",
        root.display(),
        vdb_path.display()
    );

    Ok(())
}

/// Spawna una tarea de LLM y devuelve un receptor para la respuesta.
fn spawn_llm(
    runtime: &Arc<RuntimeLoop>,
    prompt: &str,
    session_id: &str,
) -> tokio::sync::oneshot::Receiver<Result<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let runtime = Arc::clone(runtime);
    let prompt = prompt.to_owned();
    let session_id = session_id.to_owned();

    tokio::spawn(async move {
        let result = runtime.run(&prompt, &session_id).await;
        let _ = tx.send(result);
    });

    rx
}

/// Inicia el modo interactivo con UI reactiva y cola de input.
async fn cmd_interactive(
    data_dir: &PathBuf,
    initial_prompt: Option<&str>,
    json_mode: bool,
    sandbox_mode: SandboxMode,
    resume_session: Option<&str>,
) -> Result<()> {
    use ui::InputEvent;

    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            "Starting interactive mode",
        ),
    );

    // ── 1. Cargar configuración del proveedor ───────────────────────
    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;
    let model_name = dogma_config.provider.model.clone();

    // ── 2. Runtime completo con canal de eventos ────────────────────
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    let bundle = setup_runtime(
        data_dir,
        &dogma_config,
        sandbox_mode,
        resume_session,
        Some(event_tx),
    )
    .await?;

    let runtime = bundle.runtime;
    let session_id = bundle.session_id;

    // ── 3. UI setup ─────────────────────────────────────────────────
    let is_tty = std::io::stdin().is_terminal();
    if is_tty {
        crossterm::terminal::enable_raw_mode().map_err(|e| {
            dogma_v2_common::error::Error::Validation(format!("failed to enable raw mode: {e}"))
        })?;
    }

    let mut input_rx = ui::spawn_input_reader();
    let mut renderer = ui::Renderer::new();
    renderer.set_model(&model_name);
    renderer.set_context_window(dogma_config.context_window);
    renderer.init();

    let mut input_buffer = String::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut busy = false;
    let mut llm_rx: Option<tokio::sync::oneshot::Receiver<Result<String>>> = None;
    let mut input_history: Vec<String> = Vec::new();
    let mut history_idx: Option<usize> = None;

    // ── 6. Prompt inicial ───────────────────────────────────────────
    if let Some(prompt) = initial_prompt {
        renderer.reset_output();
        renderer.show_sent(prompt);
        busy = true;
        llm_rx = Some(spawn_llm(&runtime, prompt, &session_id));
    } else {
        renderer.show_input("");
    }

    // ── 7. Main loop ────────────────────────────────────────────────
    loop {
        tokio::select! {
            // Input del teclado
            Some(event) = input_rx.recv() => {
                match event {
                    InputEvent::Key(key) => {
                        use crossterm::event::{KeyCode, KeyModifiers};
                        match key.code {
                            KeyCode::Enter => {
                                // Enter: si la línea actual está llena → nueva línea
                                //        si no → enviar
                                let current_line = input_buffer.lines().last().unwrap_or("");
                                let term_width = crossterm::terminal::size()
                                    .map(|(w, _)| w as usize)
                                    .unwrap_or(80);
                                let line_full = current_line.len() >= term_width.saturating_sub(4);

                                if line_full {
                                    input_buffer.push('\n');
                                    renderer.show_input(&input_buffer);
                                    continue;
                                }

                                let prompt = input_buffer.trim().to_string();
                                if prompt.is_empty() {
                                    continue;
                                }

                                match prompt.as_str() {
                                    "/exit" | "/quit" => break,
                                    "/help" => {
                                        renderer.show_sent(&prompt);
                                        eprintln!(
                                            "┌─ Dogma 2.0 Interactive ─────────────────────────────────┐\n\
                                             │                                                           │\n\
                                             │  START                                                     │\n\
                                             │    dogma interactive            — Start interactive mode  │\n\
                                             │    dogma interactive \"hello\"    — Start with initial msg  │\n\
                                             │    dogma chat \"quick prompt\"    — Single-shot (no TUI)    │\n\
                                             │                                                           │\n\
                                             │  COMANDOS                                                 │\n\
                                             │    /help        — Show this help                          │\n\
                                             │    /exit /quit  — Exit interactive mode                   │\n\
                                             │    /status      — Show session stats                      │\n\
                                             │                                                           │\n\
                                             │  INPUT                                                     │\n\
                                             │    Enter      — Send prompt (or newline if line is full)  │\n\
                                             │    Ctrl+J     — Always add new line                       │\n\
                                             │    Up / Down  — Navigate input history                    │\n\
                                             │                                                           │\n\
                                             │  SCROLL                                                    │\n\
                                             │    PageUp/Down  — Scroll chat                             │\n\
                                             │    Home / End   — Jump to top/bottom                      │\n\
                                             │                                                           │\n\
                                             │  HERRAMIENTAS (el agente puede usarlas)                   │\n\
                                             │    search_memory      — Semantic search across sessions  │\n\
                                             │    update_user_memory — Store/retrieve user preferences  │\n\
                                             │    read_file/write_file — File operations                 │\n\
                                             │    execute_script     — Run code (bash/python/wasm)       │\n\
                                             │    plan               — Create structured task plans      │\n\
                                             │    delegate_task      — Spawn sub-agents                  │\n\
                                             │    web_search/web_extract — Web search (DuckDuckGo, no key) │\n\
                                             │                                                           │\n\
                                             └───────────────────────────────────────────────────────────┘"
                                        );
                                        renderer.show_input("");
                                    }
                                    "/status" => {
                                        renderer.show_sent(&prompt);
                                        eprintln!("Session: {session_id}");
                                        eprintln!("Model: {model_name}");
                                        eprintln!("Data dir: {}", data_dir.display());
                                        renderer.show_input("");
                                    }
                                    prompt => {
                                        // Guardar en history
                                        if input_history.last().map(|s| s.as_str()) != Some(prompt) {
                                            input_history.push(prompt.to_string());
                                        }
                                        history_idx = None;

                                        renderer.reset_output();
                                        renderer.show_sent(prompt);
                                        input_buffer.clear();
                                        renderer.show_input("");

                                        if busy {
                                            queue.push_back(prompt.to_string());
                                            renderer.show_queued(prompt);
                                        } else {
                                            busy = true;
                                            renderer.show_busy();
                                            llm_rx = Some(spawn_llm(&runtime, prompt, &session_id));
                                        }
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                input_buffer.pop();
                                renderer.show_input(&input_buffer);
                            }
                            // Ctrl+J = insert newline (multi-line input)
                            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                input_buffer.push('\n');
                                renderer.show_input(&input_buffer);
                            }
                            KeyCode::Char(c) => {
                                input_buffer.push(c);
                                history_idx = None; // reset history navigation
                                renderer.show_input(&input_buffer);
                            }
                            // Input history: Up/Down
                            KeyCode::Up => {
                                if input_history.is_empty() {
                                    continue;
                                }
                                if history_idx.is_none() {
                                    // Guardar buffer actual y empezar desde el final
                                    input_history.push(input_buffer.clone());
                                    history_idx = Some(input_history.len() - 1);
                                } else if let Some(idx) = history_idx {
                                    if idx > 0 {
                                        history_idx = Some(idx - 1);
                                    }
                                }
                                if let Some(idx) = history_idx {
                                    input_buffer = input_history[idx].clone();
                                    renderer.show_input(&input_buffer);
                                }
                            }
                            KeyCode::Down => {
                                if let Some(idx) = history_idx {
                                    if idx + 1 < input_history.len() {
                                        history_idx = Some(idx + 1);
                                        input_buffer = input_history[idx + 1].clone();
                                    } else {
                                        history_idx = None;
                                        input_buffer.clear();
                                    }
                                    renderer.show_input(&input_buffer);
                                }
                            }
                            // Scroll keys
                            KeyCode::PageUp => renderer.scroll_up(),
                            KeyCode::PageDown => renderer.scroll_down(),
                            KeyCode::Home => renderer.scroll_top(),
                            KeyCode::End => renderer.scroll_bottom(),
                            _ => {}
                        }
                    }
                    InputEvent::Quit => break,
                    InputEvent::Tick => {
                        renderer.tick();
                    }
                }
            }

            // Eventos del agente (sub-agentes, tools, status)
            Some(event) = event_rx.recv() => {
                renderer.handle_agent_event(event);
            }

            // Respuesta del LLM
            Some(result) = async {
                match llm_rx.as_mut() {
                    Some(rx) => rx.await.into(),
                    None => std::future::pending().await,
                }
            } => {
                busy = false;
                match result {
                    Ok(Ok(_response)) => {
                        // Streaming ya mostró el contenido en tiempo real.
                        // Solo actualizar status bar y mostrar input.
                        renderer.finish_response();
                    }
                    Ok(Err(e)) => {
                        renderer.show_error(&e.to_string());
                    }
                    Err(_) => {
                        renderer.show_error("LLM task panicked");
                    }
                }

                // Procesar cola
                if let Some(next) = queue.pop_front() {
                    busy = true;
                    renderer.show_busy();
                    llm_rx = Some(spawn_llm(&runtime, &next, &session_id));
                } else {
                    llm_rx = None;
                    renderer.show_input("");
                }
            }
        }
    }

    // ── 8. Cleanup ──────────────────────────────────────────────────
    renderer.cleanup();

    emit_event(
        json_mode,
        &Event::new(
            EventType::Done,
            EventSeverity::Success,
            "Interactive session completed",
        )
        .with_session_id(&session_id),
    );

    Ok(())
}

/// Emite un evento: en modo JSON lo escribe a stdout, si no a tracing.
fn emit_event(json_mode: bool, event: &Event) {
    if json_mode {
        // En modo JSON, escribir a stdout como NDJSON
        let line = event.to_ndjson_line();
        // Usar print! directamente; ignorar errores de pipe roto
        let _ = std::io::Write::write_all(&mut std::io::stdout(), line.as_bytes());
        let _ = std::io::Write::flush(&mut std::io::stdout());
    } else {
        // En modo humano, usar tracing
        match event.severity {
            EventSeverity::Fatal => {
                error!("{}", event.content);
            }
            EventSeverity::Warning => {
                tracing::warn!("{}", event.content);
            }
            EventSeverity::Success => {
                info!("{}", event.content);
            }
            _ => {
                info!("{}", event.content);
            }
        }
    }
}
