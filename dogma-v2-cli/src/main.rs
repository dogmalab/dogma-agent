//! # dogma-v2-cli — Interfaz de control por terminal
//!
//! CLI principal del agente Dogma 2.0 con comandos:
//!
//! * `dogma init` — Inicializa el entorno y levanta los mapas de
//!   memoria de dogma-vdb.
//! * `dogma chat "<prompt>"` — Ejecución rápida de una interacción.
//! * `dogma plan "<task>"` — Inicia el modo estructurado de
//!   planificación.
//!
//! ## Flag `--json`
//!
//! Si está presente, silencia el output humano de `stdout` y escupe
//! exclusivamente el stream de eventos NDJSON línea por línea.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use dogma_v2_common::Result;
use dogma_v2_common::event::{Event, EventSeverity, EventType};
use dogma_v2_core::RuntimeLoop;
use dogma_v2_core::runtime::loop_handle::LoopConfig;
use dogma_v2_core::runtime::provider::openai::OpenAiProvider;
use dogma_v2_core::runtime::sub_agent::SubAgentManager;
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::create_survival_tools;
use dogma_v2_core::tools::DelegateTaskTool;
use dogma_v2_core::tools::InstallSkillTool;
use dogma_v2_core::tools::SearchMemoryTool;
use dogma_v2_core::tools::{SandboxMode, SecurityConfig, SecurityMode, ToolGuardrail};
use dogma_v2_core::models::delegation::{AgentRole, SubAgentConfig};
use tracing::{error, info, warn};

mod config;

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

    /// Inicia el modo estructurado de planificación.
    Plan {
        /// Descripción de la tarea a planificar.
        task: String,
    },
}

fn main() {
    let cli = Cli::parse();

    // Inicializar tracing (siempre a stderr)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
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
        Commands::Chat { prompt } => cmd_chat(&data_dir, &prompt, cli.json, sandbox_mode).await,
        Commands::Plan { task } => cmd_plan(&data_dir, &task, cli.json, sandbox_mode).await,
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
        allowed_dirs: vec![data_dir.clone(), std::env::current_dir().unwrap_or_default()],
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

/// Ejecuta una interacción rápida de chat.
async fn cmd_chat(data_dir: &PathBuf, prompt: &str, json_mode: bool, sandbox_mode: SandboxMode) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            "Starting chat session",
        ),
    );

    // ── 1. Cargar configuración del proveedor ──────────────────────
    let provider_config =
        config::load_provider_config(None).map_err(dogma_v2_common::error::Error::Validation)?;
    // ── 2. Crear proveedor LLM ─────────────────────────────────────
    let provider = Arc::new(OpenAiProvider::new(provider_config)?);

    // ── 3. Inicializar sesión ──────────────────────────────────────
    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2")?;

    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            format!("Session: {session_id}"),
        )
        .with_session_id(&session_id),
    );

    // ── 4. Inicializar seguridad ─────────────────────────────────────
    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![data_dir.clone(), std::env::current_dir().unwrap_or_default()],
        sandbox_mode,
        sandbox_limits: None,
    });

    // ── 5. Crear herramientas de supervivencia ─────────────────────
    let tools = create_survival_tools();

    // ── 6. Crear y ejecutar el RuntimeLoop ─────────────────────────
    let loop_config = LoopConfig::default();
    let runtime = Arc::new(RuntimeLoop::new(provider.clone(), tools, session, loop_config));

    // Registrar herramienta de búsqueda semántica activa
    let memory_search = SearchMemoryTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(memory_search));

    // Registrar herramienta de instalación de skills dinámicos
    match InstallSkillTool::new(provider.clone(), data_dir) {
        Ok(skill_tool) => {
            runtime.register_tool(Box::new(skill_tool));
            info!("InstallSkillTool registered");
        }
        Err(e) => warn!("Failed to register InstallSkillTool: {e}"),
    }

    // Registrar herramienta de delegación a subagentes efímeros
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

    let response = runtime.run(prompt, &session_id).await?;

    // ── 6. Emitir resultado ────────────────────────────────────────
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

/// Inicia el modo estructurado de planificación.
async fn cmd_plan(data_dir: &PathBuf, task: &str, json_mode: bool, sandbox_mode: SandboxMode) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Info, "Starting plan mode"),
    );

    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2-planner")?;

    // Inicializar seguridad
    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![data_dir.clone(), std::env::current_dir().unwrap_or_default()],
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

    // Placeholder: el planificador real vendrá en una fase posterior
    let plan = format!(
        "Plan for: {task}\n\
         ─────────────────────────\n\
         1. Analyze requirements\n\
         2. Design solution\n\
         3. Implement\n\
         4. Test & verify\n\
         ─────────────────────────\n\
         Status: Planning phase initialized."
    );

    emit_event(
        json_mode,
        &Event::new(EventType::PlanProgress, EventSeverity::Success, &plan)
            .with_session_id(&session_id),
    );

    if !json_mode {
        println!("{plan}");
    }

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
