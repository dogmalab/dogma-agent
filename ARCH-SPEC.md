# Architecture — dogma-agent (Dogma 2.0)

## 1. Principios Arquitectonicos

1. **1 crate = 1 capa**. Cada crate tiene responsabilidad unica y depende solo de las capas inferiores.
2. **LLMProvider trait como frontera** entre el runtime y los modelos de lenguaje. Cualquier proveedor OpenAI-compatible puede implementarlo.
3. **Tool trait como frontera** entre el runtime y las herramientas. Cada tool es un struct con nombre, descripcion, esquema JSON y metodo `call()`.
4. **Todo el estado es un grafo vectorial en dogma-vdb**. Sesiones, mensajes y tool results son nodos conectados por aristas (NEXT, TRIGGERED).
5. **Sin dependencias externas para el core del runtime**. El LLMProvider se inyecta, no se hardcodea.
6. **NDJSON como protocolo universal**. La CLI emite eventos NDJSON cuando se activa `--json`, facilitando tests E2E y consumo por UI.
7. **3 herramientas de supervivencia**. Reemplazan las 72 herramientas estaticas del Dogma 1.0. read_file, write_file, execute_script.

---

## 2. Diagrama de Arquitectura

```
                        dogma-v2-cli
                            |
                     (clap, --json)
                            |
                    dogma-v2-core
                    /      |      \
                   /       |       \
           runtime    tools     state
              |          |         |
         LLMProvider  3 survival   SessionManager
         (trait)     tools (impl)  (dogma-vdb)
              |                     |
         [inyectado]        Collection .vdb
              |              (sessions.vdb)
         OpenAI / Anthropic / Ollama ...
                            |
                    dogma-v2-common
                    /      |      \
              error.rs   event.rs  tipos
           (thiserror)  (NDJSON)  (serde)


    dogma-vdb (path dep externa)
       └── Collection, Document, Metric
```

### Flujo de datos en el RuntimeLoop

```
Usuario ──prompt──► RuntimeLoop ──messages──► LLMProvider
                     │                             │
                     │                         LLMResponse
                     │                             │
                     ▼                             ▼
               ¿Tool calls? ──si──► ToolRegistry.execute()
                     │                    │
                     no              ToolResult
                     │                    │
                     ▼                    ▼
               Respuesta final ◄── seguimos iterando ──┐
                     │                                  │
                     ▼                                  │
             SessionManager persist ────────────────────┘
```

---

## 3. Estructura de Archivos

```text
dogma-agent/
├── Cargo.toml                  # Workspace root (edition 2024)
├── AGENTS.md                   # Reglas para implementar
├── ARCH-SPEC.md                # Este archivo
│
├── dogma-v2-common/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Re-exports: error, event
│       ├── error.rs            # Error enum: Infrastructure, Execution, Fatal
│       └── event.rs            # NDJSON: Event, EventType, EventSeverity
│
├── dogma-v2-core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Re-exports: LLMProvider, RuntimeLoop, SessionManager, Compressor, Tool
│       ├── runtime/
│       │   ├── mod.rs          # Mod declarations
│       │   ├── provider.rs     # LLMProvider trait + Message, ToolCall, TokenUsage, ProviderConfig
│       │   └── loop_handle.rs  # RuntimeLoop: ciclo RSI completo
│       ├── tools/
│       │   ├── mod.rs          # Tool trait + ToolRegistry + create_survival_tools()
│       │   ├── read_file.rs    # Lectura de archivos (1 MB max)
│       │   ├── write_file.rs   # Escritura de archivos (1 MB max, crea dirs)
│       │   └── execute_script.rs # Ejecucion bash/python/node (30s timeout)
│       └── state/
│           ├── mod.rs          # Mod declarations
│           ├── session.rs      # SessionManager: grafo vectorial en dogma-vdb
│           └── compressor.rs   # Compresor deterministico + semantico (stub)
│
└── dogma-v2-cli/
    ├── Cargo.toml
    └── src/main.rs             # Clap CLI: init, chat, plan + flag --json
```

---

## 4. Protocolo de Eventos NDJSON

### 4.1. Formato

Cada evento es una linea JSON independiente:

```json
{"type":"message","timestamp":"2026-05-25T20:00:00Z","severity":"info","content":"Hello","session_id":"session-abc123","metadata":{}}
```

### 4.2. Tipos de Evento

| `type` | Descripcion |
|--------|-------------|
| `message` | Mensaje del asistente o del usuario |
| `tool_call` | Invocacion de herramienta por el LLM |
| `tool_result` | Resultado de la ejecucion de una herramienta |
| `plan_progress` | Progreso del planificador |
| `error` | Error ocurrido durante la ejecucion |
| `done` | Senal de finalizacion |
| `system` | Estado interno del runtime |

### 4.3. Severidad

| `severity` | Uso |
|------------|-----|
| `info` | Progreso general, diagnostico |
| `warning` | Recuperable, no critico |
| `fatal` | Error que aborta el runtime |
| `debug` | Traza (solo con `RUST_LOG=debug`) |
| `success` | Operacion completada exitosamente |

### 4.4. Modo JSON vs modo humano

```
dogma chat "hello"          → output humano via tracing a stderr
dogma chat "hello" --json   → solo NDJSON a stdout, silenciar tracing
```

Esto permite:
- Uso interactivo: humano lee el output estilizado.
- Uso automatico: script o UI parsea NDJSON linea por linea.

---

## 5. RuntimeLoop — Ciclo RSI

### 5.1. Algoritmo

```
fn run(prompt, session_id):
    1. Resetear estado: iteration=0, messages=[User(prompt)]
    2. Persistir mensaje de usuario en SessionManager
    3. Entrar en tool_loop: loop
       a. Si iteration >= max_iterations (default 10), forzar respuesta
       b. Si compression enabled, maybe_compress_context()
       c. Enviar messages al LLMProvider::chat()
       d. Persistir respuesta del asistente
       e. Si no hay tool_calls → devolver respuesta
       f. Para cada tool_call:
          - ToolRegistry::execute(nombre, args_json)
          - Persistir tool call + resultado
          - Anadir ToolResult a messages locales
       g. Incrementar iteration, repetir
    4. Persistir respuesta final en SessionManager
    5. Devolver resultado
```

### 5.2. Estado local

```rust
struct LoopState {
    iteration: u32,           // contador de iteraciones
    messages: Vec<Message>,   // historial local de la sesion actual
}
```

### 5.3. Configuracion

```rust
struct LoopConfig {
    max_tool_iterations: u32,    // default: 10
    context_compression: bool,   // default: true
}
```

---

## 6. LLMProvider Trait — Proveedores Dinamicos

### 6.1. Definicion

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn chat(&self, messages: &[Message]) -> Result<LLMResponse>;
    async fn chat_stream(&self, messages: &[Message])
        -> Result<tokio::sync::mpsc::Receiver<Result<String>>>;  // default: wrappea chat()
    fn config(&self) -> &ProviderConfig;
}
```

### 6.2. Tipos Asociados

```rust
pub struct Message {
    pub role: MessageRole,     // System | User | Assistant | Tool
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
}

pub struct LLMResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,  // solicitudes de tool del LLM
    pub usage: TokenUsage,
}

pub struct ToolCall {
    pub id: String,            // id unico de la invocacion
    pub name: String,          // nombre de la herramienta
    pub arguments: String,     // JSON con los argumentos
}

pub struct ProviderConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub temperature: f32,      // default: 0.7
    pub max_tokens: u32,       // default: 4096
}
```

### 6.3. Proveedores Planeados

| Proveedor | URL Base | Implementacion |
|-----------|----------|---------------|
| OpenAI | `https://api.openai.com/v1` | Pendiente |
| Anthropic | `https://api.anthropic.com/v1` | Pendiente (adaptador) |
| Ollama | `http://localhost:11434/v1` | Pendiente |
| OpenRouter | `https://openrouter.ai/api/v1` | Pendiente |

---

## 7. Tool Trait — Las 3 de Supervivencia

### 7.1. Definicion

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> serde_json::Value;  // JSON Schema
    async fn call(&self, args: &Value) -> ToolResult;  // Err(String)
}
```

### 7.2. Registro y Ejecucion

```rust
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn register(&mut self, tool: Box<dyn Tool>);
    pub async fn execute(&self, name: &str, args_json: &str) -> ToolResult;
    pub fn tool_specs(&self) -> Vec<Value>;  // para inyectar en el prompt del sistema
}
```

### 7.3. Herramientas Implementadas

**read_file**: Lee contenido de archivos locales.
- Parametro: `path` (string, requerido)
- Limite: 1 MB
- Rechaza directorios
- Retorna el contenido textual completo

**write_file**: Crea o sobrescribe archivos.
- Parametros: `path` (string), `content` (string)
- Limite: 1 MB
- Crea directorios padre automaticamente
- Retorna confirmacion con bytes escritos

**execute_script**: Ejecuta scripts en lenguajes interpretados.
- Parametros: `lang` (enum: bash, sh, python, py, node, js), `code` (string)
- Limite: 100 KB de codigo, 30s timeout
- Retorna stdout + stderr (truncado a 50 KB)
- Usa `tokio::process::Command` con timeout

---

## 8. Session Manager — Estado en dogma-vdb

### 8.1. Modelo de Grafos

Cada sesion se modela como un grafo aciclico dirigido dentro de una coleccion `sessions.vdb`:

```
Session (raiz)
  │ node_type="Session", model="...", created_at="..."
  │
  ├── Message(user) ──NEXT──► Message(assistant) ──NEXT──► Message(user)
  │     edge_type="NEXT"          │                          │
  │                          TRIGGERED                    TRIGGERED
  │                               │                          │
  │                          ToolResult                   ToolResult
  │                          (tool_name="read_file")     (tool_name="execute_script")
```

### 8.2. Estructura de un Nodo

```json
{
  "id": "session-<uuid>" | "msg-<uuid>" | "tool-<uuid>",
  "text": "contenido del mensaje o resultado",
  "metadata": {
    "node_type": "Session | Message | ToolResult",
    "session_id": "session-abc123",
    "role": "user | assistant | tool | system",
    "sequence": "0",
    "edge_type": "NEXT | TRIGGERED",
    "tool_name": "read_file (solo ToolResult)",
    "tool_call_id": "call_xxx (solo ToolResult)",
    "model": "gpt-4o (solo Session)",
    "created_at": "2026-05-25T20:00:00Z"
  }
}
```

### 8.3. API

```rust
impl SessionManager {
    pub fn open(base_path: &Path) -> Result<Self>;
    pub fn create_session(&mut self, model: &str) -> Result<String>;
    pub async fn append_message(&mut self, session_id, role, content) -> Result<String>;
    pub async fn append_tool_result(&mut self, session_id, tool_name, tool_call_id, result) -> Result<String>;
    pub fn get_session_nodes(&self, session_id) -> Result<Vec<Document>>;
    pub fn session_node_count(&self, session_id) -> Result<usize>;
}
```

---

## 9. Compresor de Contexto

### 9.1. Compresion Determinista

Reemplaza payloads de herramientas masivas con resumenes estructurales:

| Umbral | Accion |
|--------|--------|
| Tool result > 500 chars | `[Tool output: 2,304 bytes, exit 0]` |
| Tool results consecutivos > 3 | `[Tool run: 4 tools (read_file, write_file), total 12 KB]` |

### 9.2. Compresion Semantica (Pendiente)

Buscara similitud de coseno via dogma-vdb para re-inyectar contexto relevante:

```
1. Generar embedding del prompt actual via Embedder
2. Collection::search(embedding, limit=5) en sessions.vdb
3. Devolver textos de nodos similares como contexto adicional
```

Requisito: `Embedder` trait conectado (fastembed, OpenAI embeddings, etc.).

---

## 10. CLI Interface

### 10.1. Comandos

```
dogma init                # Inicializa entorno y sessions.vdb
dogma chat "<prompt>"     # Ejecucion rapida
dogma plan "<task>"       # Modo planificacion estructurada
```

### 10.2. Flags

| Flag | Efecto |
|------|--------|
| `--json` | Output solo NDJSON a stdout |
| `--data-dir <path>` | Directorio de datos (default: `~/.dogma`) |

### 10.3. Ejemplos

```bash
# Inicializar
dogma init --data-dir ~/.dogma-dev

# Chat en modo humano
dogma chat "How do I implement quicksort in Rust?"

# Chat en modo JSON para pipeline automatico
dogma chat --json "List all .rs files in this project" | jq 'select(.severity == "success") | .content'

# Planificar
dogma plan "Build a CLI tool for file organization"
```

---

## 11. Compilacion y Verificacion

### 11.1. Requisitos

- Rust 1.85+ (edition 2024)
- dogma-vdb como path dependency (`../dogma-vdb`)

### 11.2. Comandos

```bash
cargo check --workspace          # 0 errors, 0 warnings
cargo test --workspace           # todos pasan
cargo clippy --workspace -- -D warnings  # lint limpio
cargo fmt --all -- --check       # formateo correcto
```

### 11.3. Perfiles

```toml
[profile.release]
lto = true
codegen-units = 1
opt-level = 3
strip = true
```

---

## 12. Metricas de Progreso

| Metrica | Objetivo | Actual |
|---------|----------|--------|
| LOC totales | < 5,000 | ~2,335 |
| Archivos .rs | < 20 | 13 |
| Dependencias core | < 10 | 8 |
| Tools en core | = 3 | 3 |
| Tests | > 20 | 13 |
| Warnings | 0 | 0 |
| unsafe | 0 | 0 |

---

*Ultima actualizacion: 2026-05-25*
