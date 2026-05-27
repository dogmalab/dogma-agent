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

use clap::{Parser, Subcommand};
use dogma_v2_common::event::{Event, EventSeverity, EventType};
use dogma_v2_common::Result;
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::create_survival_tools;
use tracing::{error, info};

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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
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

    match cli.command {
        Commands::Init => cmd_init(&data_dir, cli.json).await,
        Commands::Chat { prompt } => cmd_chat(&data_dir, &prompt, cli.json).await,
        Commands::Plan { task } => cmd_plan(&data_dir, &task, cli.json).await,
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
async fn cmd_init(data_dir: &PathBuf, json_mode: bool) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Info, "Initializing Dogma 2.0 environment"),
    );

    // Crear directorio de datos
    std::fs::create_dir_all(data_dir).map_err(|e| {
        dogma_v2_common::error::Error::Io {
            path: data_dir.clone(),
            source: e,
        }
    })?;

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
async fn cmd_chat(data_dir: &PathBuf, prompt: &str, json_mode: bool) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Info, "Starting chat session"),
    );

    // Inicializar session manager
    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2")?;

    // Crear herramientas de supervivencia
    let _tools = create_survival_tools();

    // FIXME: Se necesita un proveedor LLM real aquí. Por ahora
    // registramos el inicio y salida con un mensaje informativo.
    //
    // let provider = Arc::new(MyLLMProvider::new(config));
    // let loop_config = LoopConfig::default();
    // let runtime = RuntimeLoop::new(provider, tools, session, loop_config);
    // let response = runtime.run(prompt, &session_id).await?;

    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Warning,
            "Chat mode requires an LLM provider. Use `dogma provider set` or configure via environment.",
        ),
    );

    // Emitir el prompt como evento
    emit_event(
        json_mode,
        &Event::new(EventType::Message, EventSeverity::Info, prompt)
            .with_session_id(&session_id)
            .with_metadata("role", "user"),
    );

    // Emitir placeholder de respuesta
    emit_event(
        json_mode,
        &Event::new(EventType::Message, EventSeverity::Info,
            "Chat session created. Configure an LLM provider to start interacting.",
        ).with_session_id(&session_id)
        .with_metadata("role", "assistant"),
    );

    // Si no es modo JSON, mostrar en humano
    if !json_mode {
        println!("Session ID: {session_id}");
        println!("Data dir: {}", data_dir.display());
        println!();
        println!("Chat mode ready. Provider configuration needed.");
        println!("  Set DOGMA_API_KEY and DOGMA_BASE_URL environment variables");
        println!("  or use: dogma provider set <provider> <api_key>");
    }

    Ok(())
}

/// Inicia el modo estructurado de planificación.
async fn cmd_plan(data_dir: &PathBuf, task: &str, json_mode: bool) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(EventType::System, EventSeverity::Info, "Starting plan mode"),
    );

    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2-planner")?;

    emit_event(
        json_mode,
        &Event::new(EventType::PlanProgress, EventSeverity::Info, &format!("Planning task: {task}"))
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
