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
//!   Soporta historial de input (Up/Down), multi-línea (Shift+Enter),
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
//! * `web_search`, `web_extract` — búsqueda web (requiere EXA_API_KEY)
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
use dogma_v2_core::runtime::loop_handle::LoopConfig;
use dogma_v2_core::runtime::provider::openai::OpenAiProvider;
use dogma_v2_core::runtime::sub_agent::SubAgentManager;
use dogma_v2_core::state::session::SessionManager;
use dogma_v2_core::tools::DelegateTaskTool;
use dogma_v2_core::tools::InstallSkillTool;
use dogma_v2_core::tools::PlanTool;
use dogma_v2_core::tools::SearchMemoryTool;
use dogma_v2_core::tools::create_survival_tools;
use dogma_v2_core::tools::{SandboxMode, SecurityConfig, SecurityMode, ToolGuardrail};
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

    /// Inicia el modo estructurado de planificación.
    Plan {
        /// Descripción de la tarea a planificar.
        task: String,
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
        Commands::Chat { prompt } => cmd_chat(&data_dir, &prompt, cli.json, sandbox_mode).await,
        Commands::Interactive { initial_prompt } => {
            cmd_interactive(&data_dir, initial_prompt.as_deref(), cli.json, sandbox_mode).await
        }
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

/// Ejecuta una interacción rápida de chat.
async fn cmd_chat(
    data_dir: &PathBuf,
    prompt: &str,
    json_mode: bool,
    sandbox_mode: SandboxMode,
) -> Result<()> {
    emit_event(
        json_mode,
        &Event::new(
            EventType::System,
            EventSeverity::Info,
            "Starting chat session",
        ),
    );

    // ── 1. Cargar configuración del proveedor ──────────────────────
    let dogma_config =
        config::load_config(None).map_err(dogma_v2_common::error::Error::Validation)?;
    // ── 2. Crear proveedor LLM ─────────────────────────────────────
    let provider = Arc::new(OpenAiProvider::new(dogma_config.provider)?);

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
        allowed_dirs: vec![
            data_dir.clone(),
            std::env::current_dir().unwrap_or_default(),
        ],
        sandbox_mode,
        sandbox_limits: None,
    });

    // ── 5. Crear herramientas de supervivencia ─────────────────────
    let tools = create_survival_tools();

    // ── 6. Crear y ejecutar el RuntimeLoop ─────────────────────────
    let loop_config = LoopConfig::default();
    let runtime = Arc::new(RuntimeLoop::new(
        provider.clone(),
        tools,
        session,
        loop_config,
        None,
    ));

    // Registrar herramienta de búsqueda semántica activa
    let memory_search = SearchMemoryTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(memory_search));

    // Registrar herramienta de planificación
    let plan_tool = PlanTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(plan_tool));

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

    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2-planner")?;

    // Inicializar seguridad
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
) -> Result<()> {
    use dogma_v2_core::models::events::AgentEvent;
    use tokio::sync::mpsc;
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
    let provider = Arc::new(OpenAiProvider::new(dogma_config.provider)?);

    // ── 2. Inicializar sesión ───────────────────────────────────────
    let mut session = SessionManager::open(data_dir)?;
    let session_id = session.create_session("dogma-v2-interactive")?;

    // ── 3. Inicializar seguridad ────────────────────────────────────
    ToolGuardrail::init(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: vec![
            data_dir.clone(),
            std::env::current_dir().unwrap_or_default(),
        ],
        sandbox_mode,
        sandbox_limits: None,
    });

    // ── 4. Crear runtime con canal de eventos ───────────────────────
    let tools = create_survival_tools();
    let loop_config = LoopConfig {
        max_tool_iterations: dogma_config.max_tool_iterations,
        ..LoopConfig::default()
    };
    let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);

    let runtime = Arc::new(RuntimeLoop::new(
        provider.clone(),
        tools,
        session,
        loop_config,
        Some(event_tx),
    ));

    let memory_search = SearchMemoryTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(memory_search));

    let plan_tool = PlanTool::new(runtime.session_handle());
    runtime.register_tool(Box::new(plan_tool));

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

    // ── 5. UI setup ─────────────────────────────────────────────────
    let is_tty = std::io::stdin().is_terminal();
    if is_tty {
        crossterm::terminal::enable_raw_mode().map_err(|e| {
            dogma_v2_common::error::Error::Validation(format!("failed to enable raw mode: {e}"))
        })?;
    }

    let mut input_rx = ui::spawn_input_reader();
    let mut renderer = ui::Renderer::new();
    renderer.set_model(&model_name);
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
                        use crossterm::event::KeyCode;
                        match key.code {
                            KeyCode::Enter => {
                                // Shift+Enter detection: si el buffer ya tiene
                                // líneas nuevas, agregar otra en vez de submit.
                                if input_buffer.contains('\n') {
                                    input_buffer.push('\n');
                                    renderer.show_input(&input_buffer);
                                    continue;
                                }

                                let line = input_buffer.trim().to_string();
                                if line.is_empty() {
                                    continue;
                                }

                                match line.as_str() {
                                    "/exit" | "/quit" => break,
                                    "/help" => {
                                        renderer.show_sent(&line);
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
                                             │    <text> Enter — Send prompt to agent                    │\n\
                                             │    Shift+Enter  — New line in multi-line input            │\n\
                                             │    Up / Down    — Navigate input history                  │\n\
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
                                             │    web_search/web_extract — Web search (needs EXA key)    │\n\
                                             │                                                           │\n\
                                             └───────────────────────────────────────────────────────────┘"
                                        );
                                        renderer.show_input("");
                                    }
                                    "/status" => {
                                        renderer.show_sent(&line);
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
