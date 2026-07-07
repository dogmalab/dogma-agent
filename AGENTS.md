# AGENTS.md — Reglas para Implementar dogma-agent (Dogma 2.0)

> El **agent harness** de la plataforma [Dogma](https://github.com/dogmalab/.github).

Este documento contiene las reglas **específicas del harness** para
trabajar en `dogma-agent`. Las reglas generales de la plataforma —
las Cuatro Preguntas, el proceso de contribución, el flujo de
reporte de seguridad — viven en el
[CONTRIBUTING.md](https://github.com/dogmalab/.github/blob/main/CONTRIBUTING.md)
a nivel de organización. El "por qué" detrás de estas reglas está
en el [MANIFESTO.md](https://github.com/dogmalab/.github/blob/main/MANIFESTO.md).

> **Nota sobre el idioma:** este `AGENTS.md` está en español
> deliberadamente. Es el único documento del ecosistema Dogma en
> español, por diseño: refleja el idioma de trabajo del autor
> principal. Los comentarios de código y la documentación
> user-facing (READMEs, FAQ, ROADMAP, GLOSSARY) están en inglés.
> Las reglas de formato y estilo de código del
> [CONTRIBUTING.md](https://github.com/dogmalab/.github/blob/main/CONTRIBUTING.md#code-style-per-harness)
> aplican a este harness también.

Si una regla de este archivo entra en conflicto con una regla a
nivel de organización, gana la regla a nivel de organización. Si una
regla aquí falta a nivel organización, eso es un bug — abrir un
issue para resolverlo.

---

## Filosofia del Proyecto

```
dogma-agent = Rust + tokio + dogma-vdb
              (minimo deps, CLI-first, sin servidor, testabilidad automatica)
```

Cada linea de codigo debe justificar su existencia. Preferimos **50
lineas claras** a 200 lineas "arquitectonicamente flexibles".

El agente Dogma 2.0 es una reescritura total desde cero. Abandonamos
todas las dependencias heredadas de OpenCrabs (SQLite, deadpool,
runtimes poliglotas). La filosofia core es minimalismo absoluto de
tokens (estilo Pi-Mono), frontend ligero en kbytes y **unificacion
total de estado en dogma-vdb**.

---

## Estado Actual (2026-07-07)

### Estructura del Workspace

| Crate | Descripcion | Estado |
|-------|-------------|--------|
| `dogma-v2-common` | Tipos compartidos, protocolo NDJSON, traits fundamentales | Completo |
| `dogma-v2-core` | Runtime async, loop de herramientas (RSI), `LLMProvider`, gestión de estado, compresor, **IE (F2)**, **Cost Gate (F2)** | En desarrollo (F2 pendiente) |
| `dogma-v2-cli` | CLI con clap, TUI modular, modo `--json`, comando `/ei` | Completo (cmd_plan es placeholder) |

> **Nota:** el conteo exacto de líneas y tests se mide con
> `cargo test --workspace` antes de cada release. La cifra
> histórica "31 .rs / 6,720 LOC / 49 tests" del AGENTS.md previo
> está desactualizada. El número real se publica en el CHANGELOG
> de cada release.

### Diagrama de Capas

```
dogma-v2-common ──────► dogma-v2-core ──────► dogma-v2-cli
     (tipos)                 (runtime)             (CLI+TUI)
        │                        │
        │                   dogma-vdb
        │                  (estado nativo)
        └── serde + thiserror + parking_lot + tracing + chrono
```

### Capas de Memoria

```
1. Session Context  — historial de conversación (dogma-vdb)
2. User Memory      — preferencias y datos del usuario (persistente)
3. System Context   — OS, project, git (auto-detectado)
4. Context Manager  — selección semántica de contexto relevante
5. Cost Gate        — cada operacion cara pide confirmacion al usuario
```

---

## Reglas Específicas del Harness

Estas son las reglas que aplican a `dogma-agent` y no necesariamente
a los otros harnesses. Las reglas generales — formato, comentarios
en inglés, no abstracción prematura, estilo de código — viven en el
[CONTRIBUTING.md](https://github.com/dogmalab/.github/blob/main/CONTRIBUTING.md#code-style-per-harness).

### 1. Zero `unsafe` en todo el workspace

`#![deny(unsafe_code)]` está activo en cada crate. Si se necesita
`unsafe`, debe documentarse y justificarse explícitamente, y la
discusión debe incluir al maintainer. La política de Dogma es
**cero `unsafe` en runtime del agente**.

### 2. Zero `unwrap()` en producción

Los handlers, el loop, y el state manager deben propagar errores
con `?` y enums tipados. `unwrap()` solo en tests y ejemplos.
Excepción: la implementación de traits donde el contrato requiere
`Result` puede usar `unwrap_or_default` cuando el default es seguro.

### 3. `parking_lot::RwLock` sobre `std::sync::RwLock`

Prohibido usar `std::sync::RwLock`. Toda sincronización debe usar
`parking_lot::RwLock` para garantizar inmunidad contra
envenenamiento de locks. Un agente que corre durante horas no puede
morir por un `RwLock` envenenado.

### 4. Async permitido (con justificación)

El agent harness **sí es async**, a diferencia del state harness.
Razón: hablar con LLMs por HTTP no es zero-runtime, pero es
zero-blocking. Un agente síncrono congelaría en cada llamada. El
runtime es `tokio`. El boundary de async está en el lugar correcto:
dentro del harness, en el trait `LLMProvider`, en la red.

### 5. Sin macros procedurales

Las macros de derive de `serde` y `thiserror` están permitidas. Las
macros procedurales custom, los `macro_rules!` complejos, y la
generación de código en build.rs están prohibidos sin discusión en
issue. Razón: el debug es más caro cuando el código no es legible.

### 6. Sin genéricos innecesarios en el core loop

El loop de razonamiento es el hot path. Cada indirección se paga
en cada iteración. Usar `Box<dyn Trait>` cuando hay dispatch
dinámico real; usar `impl Trait` cuando hay monomorfización;
evitar abstracciones genéricas que no paguen su costo.

### 7. Tres herramientas core + skills instaladas en runtime

Las tres herramientas de supervivencia son las únicas registradas
por defecto. Reemplazan las 72 herramientas estáticas de Dogma 1.0:

| Herramienta | Proposito | Limites |
|-------------|-----------|---------|
| `read_file(path)` | Leer archivos del sistema local | 1 MB max, rechaza directorios |
| `write_file(path, content)` | Crear/sobrescribir archivos | 1 MB max, crea directorios padre |
| `execute_script(lang, code)` | Ejecutar scripts bash/python/node | 100 KB max, 30s timeout |

Cualquier herramienta adicional se instala en runtime via
`SkillManager`, no se registra en el código del core. Esto
preserva la promesa "tres herramientas y un loop".

> **Nota histórica:** la regla original decía "máximo 3
> herramientas". La realidad actual es que `cmd_chat` registra
> más herramientas (search_memory, plan, delegate_task, etc.).
> Estas son skills, no herramientas core, y se documentan
> como tal. La regla efectiva es: **3 herramientas core, N skills
> instaladas en runtime**.

### 8. NDJSON — el protocolo universal

```
stdout (modo --json)
├── {"type":"message","severity":"info","timestamp":"...","content":"...","session_id":"..."}
├── {"type":"tool_call","severity":"info","content":"read_file","metadata":{...}}
└── {"type":"done","severity":"success","content":"Task completed"}
```

- **Cada linea es independiente** — se puede hacer `grep`, `sed`, `head`.
- **Doble salida**: modo human-readable via `tracing` a stderr, modo JSON a stdout.
- **Facilita automatizacion**: tests E2E y consumo por UI / gateway.

### 9. Estado unificado en dogma-vdb

Todo el estado del agente se modela como nodos de un grafo
vectorial en `dogma-vdb`. Las aristas viven en `metadata`:

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

Cada nodo incluye metadatos: `node_type`, `session_id`, `role`,
`sequence`, `edge_type`, `created_at`. Para IE, se agregan
`CostProposal`, `CostDecision`, `CostActual`.

### 10. El Cost Gate es obligatorio

Cada patrón que use tokens de LLM, ejecución WASM, o instalación
de skills debe pasar por `CostGate::ask`. Sin ejecución silenciosa.
La decisión del usuario se loguea en el session graph. Esta regla
no es negociable — es la Cuarta Pregunta del
[CONTRIBUTING.md](https://github.com/dogmalab/.github/blob/main/CONTRIBUTING.md).

### 11. Compresor de contexto de doble via

- **Determinista**: Podar payloads de herramientas masivas (>500
  chars → resumen).
- **Semantico**: Busqueda de similitud de coseno via mmap de
  dogma-vdb. **Pendiente de integración** (F2 conecta el
  embedder real).

---

## Las 3 Herramientas de Supervivencia

Documentación detallada en el README del proyecto. Resumen:

- **`read_file(path)`** — Lee hasta 1 MB. Rechaza directorios.
  Usado por el LLM para inspeccionar el contexto.
- **`write_file(path, content)`** — Escribe hasta 1 MB. Crea
  directorios padre. Usado por el LLM para guardar resultados.
- **`execute_script(lang, code)`** — Ejecuta bash/python/node en
  el `WasmSandbox` (cuando el modo WASM está activo) o nativo
  (cuando no). Timeout: 30s. Output: stdout/stderr capturados.

El LLM puede invocar estas herramientas para leer el contexto,
escribir soluciones y ejecutar scripts. Si necesita algo más
complejo, escribe un script que lo haga.

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
    async fn test_foo() -> Result<()> { ... }
}
```

---

## Como Evaluamos Codigo Nuevo

Las cinco puertas, en orden:

1. **Compila con `cargo check --workspace`**.
2. **Sin errores de clippy** (`cargo clippy --workspace -- -D warnings`).
3. **Tests pasan** (`cargo test --workspace`).
4. **Sin dependencias nuevas** en el core (o justificadas en `[rfc]`).
5. **Formato correcto** (`cargo fmt --all -- --check`).
6. **Warnings 0** — no se permite código con warnings.

Si cumple todo, el código puede mergearse. CI corre los mismos
chequeos.

---

## Lo Que NO Hacemos (específico del agent harness)

Las reglas anti-platform están en el
[MANIFESTO.md](https://github.com/dogmalab/.github/blob/main/MANIFESTO.md#what-we-are-not).
Estas son específicas del agent harness:

- **NO 72 herramientas estáticas**. El agente Dogma 1.0 tenía
  72 tools registradas. Dogma 2.0 tiene 3 + skills. La promesa es
  esta.
- **NO abstracciones sobre el LLM**. `LLMProvider` es la
  abstracción. `AgentExecutor` no existe. `Retriever` no existe.
  `ToolNode` no existe. El loop es el loop.
- **NO estado global mutable**. La configuración va en TOML; el
  estado de sesión va en `.vdb`; el estado en proceso va en el
  `RuntimeLoop`. Sin singletons.
- **NO 0 unsafe en todo el workspace**. La regla es "cero
  unsafe"; el cero es literal.
- **NO std::sync::RwLock**. La regla es `parking_lot::RwLock`
  exclusivamente.

---

## Roadmap (específico del agent harness)

El roadmap a nivel plataforma está en el
[ROADMAP.md](https://github.com/dogmalab/.github/blob/main/ROADMAP.md).
Esta sección lista los items específicos del agent harness.

### Recientemente completado

- [x] Workspace multi-crate
- [x] `dogma-v2-common` (error enum, NDJSON events)
- [x] `dogma-v2-core` (runtime, tools, state, compressor)
- [x] `dogma-v2-cli` (clap CLI, `--json` flag)
- [x] `OpenAiProvider` real (HTTP reqwest, JSON parsing defensivo)
- [x] `SessionManager` integrado con `dogma-vdb`
- [x] `WasmSandbox` para `execute_script`
- [x] `SkillManager` con instalación de skills.sh
- [x] `ToolGuardrail` con modos de seguridad
- [x] Comando `/ei` (placeholder; F2 lo cablea)

### En curso (F2 + F3)

- [ ] `enriched.rs` — patrón de Inferencia Enriquecida
- [ ] `cost_gate.rs` — el patrón de Cost Gate con `Interactive`,
      `Auto`, `Trusted`, `Webhook`
- [ ] `cost_estimator.rs` — trait `CostCalculable` con impls por
      provider
- [ ] `quality_estimator.rs` — trait `QualityCalculable` con
      baseline heurística
- [ ] LLMProvider fan-out y fan-in
- [ ] Persistencia en `.vdb` de `CostProposal`, `CostDecision`,
      `CostActual`
- [ ] Conectar el embedder real en `Compressor` (FIXME existente)

### Pendiente (F4+)

- [ ] `cmd_plan` con planificador real (actualmente es placeholder)
- [ ] Sesiones persistentes con recuperación de historial
- [ ] Tests E2E con mock LLM provider
- [ ] CI pipeline (cargo test, clippy, fmt)
- [ ] Frontend (consumiendo NDJSON via SSE) — el gateway es el
      lugar para esto, no el agent

### Rechazado

- ❌ Servidor HTTP en el agent. El gateway es el boundary.
- ❌ Async sin `tokio`. La elección es `tokio`.
- ❌ Más de 3 herramientas core. El resto son skills.
- ❌ `std::sync::RwLock`. `parking_lot` exclusivamente.
- ❌ Macros procedurales custom. Solo derives de `serde` y
  `thiserror`.

---

## Ver Tambien

- [README.md](./README.md) — documentación user-facing.
- [ARCH-SPEC.md](./ARCH-SPEC.md) — decisiones de arquitectura.
- [SPEC.md del state harness](https://github.com/dogmalab/dogma-vdb/blob/main/SPEC.md)
  — la spec técnica del state harness que este agent consume.
- Documentos a nivel plataforma:
  [MANIFESTO](https://github.com/dogmalab/.github/blob/main/MANIFESTO.md),
  [STRATEGY](https://github.com/dogmalab/.github/blob/main/STRATEGY.md),
  [CONTRIBUTING](https://github.com/dogmalab/.github/blob/main/CONTRIBUTING.md),
  [FAQ](https://github.com/dogmalab/.github/blob/main/FAQ.md),
  [ROADMAP](https://github.com/dogmalab/.github/blob/main/ROADMAP.md),
  [GLOSSARY](https://github.com/dogmalab/.github/blob/main/GLOSSARY.md).

---

*Última actualización: 2026-07-07*
