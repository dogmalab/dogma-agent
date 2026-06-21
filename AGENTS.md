# AGENTS.md — Reglas para Implementar dogma-agent (Dogma 2.0)

## Filosofia del Proyecto

```
dogma-agent = Rust + tokio + dogma-vdb
              (minimo deps, CLI-first, sin servidor, testabilidad automatica)
```

Cada linea de codigo debe justificar su existencia. Preferimos **50 lineas claras** a 200 lineas "arquitectonicamente flexibles".

El agente Dogma 2.0 es una reescritura total desde cero. Abandonamos todas las dependencias heredadas de OpenCrabs (SQLite, deadpool, runtimes poliglotas). La filosofia core es minimalismo absoluto de tokens (estilo Pi-Mono), frontend ligero en kbytes y **unificacion total de estado en dogma-vdb**.

---

## ✅ ESTADO ACTUAL (2026-06-20)

### Estructura del Workspace — COMPILA (0 warnings, 0 errors)

| Crate | Archivos | LOC | Tests | Estado |
|-------|----------|-----|-------|--------|
| `dogma-v2-common` | 3 .rs | ~320 | 3 | Completo (error enum, NDJSON events) |
| `dogma-v2-core` | 12 .rs | ~2,800 | 15 | Completo (runtime, tools, state, compressor, context_manager, web tools) |
| `dogma-v2-cli` | 4 .rs | ~2,100 | 6 | Completo (clap CLI, ratatui TUI, config) |
| **Total** | **19 .rs** | **~5,220** | **24** | **Compila 0/0** |

### Diagrama de Capas

```
dogma-v2-common ──────► dogma-v2-core ──────► dogma-v2-cli
     (tipos)                 (runtime)             (CLI)
        │                        │
        │                   dogma-vdb
        │                  (estado nativo)
        └── serde + thiserror + parking_lot + tracing + chrono
```

---

## ✅ Lo Que SI Hacemos

### 1. Rust idiomatico — sin rodeos

- **`Into<T>` en constructores** para flexibilidad sin costo.
- **`impl Trait` en parametros** (monomorfizacion) en lugar de `Box<dyn Trait>` a menos que necesitemos dynamic dispatch real.
- **`async_trait`** para traits con metodos async (LLMProvider, Tool).
- **`parking_lot::RwLock`** sobre `std::sync::RwLock` — inmunidad contra envenenamiento de locks.
- **`#[must_use]`** en funciones cuyo resultado no deberia ignorarse.
- **`sort_unstable`** sobre `sort`. No necesitamos estabilidad.

### 2. Codigo pequeno — cada archivo < 300 lineas

Maximo 300 lineas por archivo (con excepciones comprobadas: `loop_handle.rs` y `main.rs` rozan el limite). Si un modulo crece mas, se divide.

### 3. Dependencias minimas — preguntar antes de anadir

**Deps obligatorias del core actual:**
- `tokio` — runtime asincrono
- `serde` + `serde_json` + `thiserror` — serializacion y errores
- `parking_lot` — locks seguros
- `tracing` + `tracing-subscriber` — logs estructurados
- `async-trait` — traits async
- `chrono` + `uuid` — timestamps e IDs
- `dogma-vdb` — unico backend de estado (path dep)

**Deps CLI:**
- `clap` — parser de comandos

### 4. Pruebas desde el principio

- Cada modulo tiene `#[cfg(test)] mod tests` al final.
- Tests deben pasar **sin red** ni servicios externos (sin llamadas reales a LLMs).
- Todos los tests nuevos deben compilar y pasar en CI.

### 5. NDJSON — el protocolo universal

```
stdout (modo --json)
├── {"type":"message","severity":"info","timestamp":"2026-05-25T20:00:00Z","content":"...","session_id":"..."}
├── {"type":"tool_call","severity":"info","content":"read_file","metadata":{"tool":"read_file","args":"..."}}
└── {"type":"done","severity":"success","content":"Task completed"}
```

- **Cada linea es independiente** — se puede hacer `grep`, `sed`, `head`.
- **Doble salida**: modo human-readable via `tracing` a stderr, modo JSON a stdout.
- **Facilita automatizacion**: tests E2E y consumo por UI en Lit Element.

### 6. Traits pequenos y enfocados

```rust
#[async_trait]
pub trait LLMProvider: Send + Sync {
    async fn chat(&self, messages: &[Message]) -> Result<LLMResponse>;
    async fn chat_stream(&self, messages: &[Message])
        -> Result<mpsc::Receiver<Result<String>>>;
    fn config(&self) -> &ProviderConfig;
}
```

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn parameters(&self) -> serde_json::Value;
    async fn call(&self, args: &Value) -> ToolResult;
}
```

### 7. Estado unificado en dogma-vdb

Todo el estado del agente se modela como nodos de un grafo vectorial:

```
Session (raiz)
  │
  ├── Message (role: user) ──NEXT──► Message (role: assistant)
  │                                          │
  │                                     TRIGGERED
  │                                          │
  │                                     ToolResult (read_file)
  │                                          │
  │                                     TRIGGERED
  │                                          │
  │                                     ToolResult (write_file)
  │
  └── Message (role: user) ──NEXT──► ...
```

Cada nodo incluye metadatos: `node_type`, `session_id`, `role`, `sequence`, `edge_type`, `created_at`.

### 8. Compresor de contexto de doble via

- **Determinista**: Podar payloads de herramientas masivas (>500 chars → resumen).
- **Semantico** (stub pendiente): Busqueda de similitud de coseno via mmap de dogma-vdb (<5ms).

---

## ❌ Lo Que NO Hacemos

### 1. NO anadir dependencias al core sin discutirlo

Si alguien quiere usar el runtime sin HTTP ni async pesado, que pueda hacerlo con deps minimas.

### 2. NO premature abstraction

```rust
// MAL — abstraer por abstraer
trait MessageProcessor { fn process(&self, m: Message) -> Result<()>; }

// BIEN — concreto, directo
pub async fn run_tool_loop(provider: &dyn LLMProvider, tools: &ToolRegistry, ...) -> Result<String>;
```

Empezamos con 3 herramientas de supervivencia. Si hace falta una cuarta, se anade como otro implementador del trait `Tool`.

### 3. NO clonar sin necesidad

```rust
// MAL
let response = self.provider.chat(&state.messages.clone()).await?;

// BIEN
let response = self.provider.chat(&state.messages).await?;
```

### 4. NO unwrap()/expect() en produccion

```rust
// MAL
let path = args["path"].as_str().unwrap();

// BIEN
let path = args.get("path").and_then(Value::as_str)
    .ok_or_else(|| "missing required argument: path".to_string())?;
```

`unwrap()` solo en tests y ejemplos.

### 5. NO std::sync::RwLock

Prohibido. Toda sincronizacion debe usar `parking_lot::RwLock` para garantizar inmunidad contra envenenamiento de locks.

### 6. NO 0 unsafe en todo el workspace

Cero unsafe en todo el espacio de trabajo. Si se necesita unsafe, documentar y justificar explicitamente.

### 7. NO estructuras sobreingenieria

- Sin macros procedurales.
- Sin genericos innecesarios.
- Sin mas de 3 herramientas en el core (las 3 de supervivencia).
- Sin estado global mutable.

### 8. NO ignorar los warnings de clippy

El CI falla con `-D warnings`. Silenciar warnings con `#[allow(...)]` solo si hay una razon justificada y documentada.

---

## Las 3 Herramientas de Supervivencia

Son las unicas herramientas registradas por defecto. Reemplazan las 72 herramientas estaticas del Dogma 1.0.

| Herramienta | Proposito | Limites |
|------------|-----------|---------|
| `read_file(path)` | Leer archivos del sistema local | 1 MB max, rechaza directorios |
| `write_file(path, content)` | Crear/sobrescribir archivos | 1 MB max, crea directorios padre |
| `execute_script(lang, code)` | Ejecutar scripts bash/python/node | 100 KB max, 30s timeout |

El LLM puede invocar estas herramientas para leer el contexto, escribir soluciones y ejecutar scripts. Si necesita algo mas complejo, escribe un script que lo haga.

---

## Estructura Tipica de un Modulo

```rust
//! 1. Docstring de una linea con el proposito.

// 2. Imports agrupados: stdlib, externos, crate
use std::path::PathBuf;
use crate::error::Result;

// 3. Tipos publicos (struct, enum, trait)
pub struct Foo { ... }
pub trait Bar { ... }

// 4. Implementaciones
impl Foo { ... }
impl Bar for Foo { ... }

// 5. Funciones publicas helpers (si aplica)
pub fn helper() -> Result<()> { ... }

// 6. Tests (al final del archivo)
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_foo() { ... }
}
```

---

## Como Evaluamos Codigo Nuevo

1. **Compila con `cargo check --workspace`** ✅
2. **Sin errores de clippy** (`cargo clippy --workspace -- -D warnings`) ✅
3. **Tests pasan** (`cargo test --workspace`) ✅
4. **Sin dependencias nuevas** en el core (o justificadas) ✅
5. **Formato correcto** (`cargo fmt --all -- --check`) ✅
6. **Warnings 0** — no se permite codigo con warnings ✅

Si cumple todo, el codigo puede mergearse.

---

## Herramientas Que Tenemos

### Del runtime (siempre disponibles)

| Herramienta | Para que |
|-------------|----------|
| `tokio::fs` | I/O asincrona de archivos |
| `tokio::process::Command` | Ejecutar scripts |
| `serde_json` | Serializar/deserializar |
| `thiserror` | Errores tipados |
| `parking_lot::RwLock` | Estado compartido seguro |
| `tracing` | Logs estructurados a stderr |
| `dogma_vdb::Collection` | Persistencia en grafo vectorial |

### De la stdlib de Rust (sin dependencias extra)

```rust
std::fs::read_to_string()   // → leer archivos (read_file)
std::fs::write()            // → escribir archivos (write_file)
std::fs::create_dir_all()   // → crear directorios
std::fs::metadata()         // → tamano, tipo de archivo
std::path::Path             // → rutas y extensiones
std::collections::HashMap   // → metadatos de eventos
```

---

## Pendiente (Roadmap)

- [x] Workspace multi-crate (root Cargo.toml)
- [x] dogma-v2-common (error enum, NDJSON events)
- [x] dogma-v2-core (runtime, tools, state, compressor)
- [x] dogma-v2-cli (clap CLI, --json flag)
- [ ] Implementar proveedor LLM concreto (OpenAI-compatible HTTP)
- [ ] Conectar RuntimeLoop real en `cmd_chat`
- [ ] Conectar embedder para busqueda semantica en Compressor
- [ ] Sesiones persistentes con recuperacion de historial
- [ ] Implementar `cmd_plan` con planificador real
- [ ] Tests E2E con mock LLM provider
- [ ] CI pipeline (cargo test, clippy, fmt)
- [ ] Frontend Lit Element (consumiendo NDJSON via SSE)

---

*Ultima actualizacion: 2026-05-25*
