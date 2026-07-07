# dogma-agent

> The agent harness of the [Dogma](https://github.com/dogmalab/.github) platform.
> An AI runtime in one binary. Three tools, a loop, a state backend, and a
> Cost Gate that asks before it spends. Air-gapped by design.

**Read first:** the [Dogma Manifesto](https://github.com/dogmalab/.github/blob/main/MANIFESTO.md)
explains why this exists and what it is for. This document is the
agent-harness-specific README.

---

## What dogma-agent is

`dogma-agent` is the **agent harness** of the Dogma platform. It is a
Rust workspace that contains:

- the agent runtime (`dogma-v2-core`),
- the CLI / TUI facade (`dogma-v2-cli`),
- the shared types and NDJSON protocol (`dogma-v2-common`),
- and (historically) the network gateway (now in its own repo,
  [`dogmalab/dogma-gateway`](https://github.com/dogmalab/dogma-gateway)).

The agent harness is **air-gapped by design**: it has no HTTP
listener, no inbound sockets. All state lives in `.vdb` files
through the [state harness](https://github.com/dogmalab/dogma-vdb)
via a path dependency. The network harness is the only component
that touches the network.

The flagship pattern inside the agent harness is
[Enriched Inference (IE)](https://github.com/dogmalab/.github/blob/main/GLOSSARY.md#enriched-inference-ie):
N LLMs run in parallel, their responses are synthesized, iterated,
and gated by a [Cost Gate](https://github.com/dogmalab/.github/blob/main/GLOSSARY.md#cost-gate)
that asks the user to confirm the cost before the run.

## Workspace Crates

| Crate | Description | LOC |
|-------|-------------|-----|
| `dogma-v2-common` | Shared error types, NDJSON event protocol, and foundational traits | ~270 |
| `dogma-v2-core` | Async agent runtime — tool loop (RSI), LLM provider abstraction, state management on dogma-vdb, context compressor | ~1,800 |
| `dogma-v2-cli` | Terminal entrypoint — Clap-based command dispatch, NDJSON output mode | ~265 |
| `dogma-gateway` | Axum HTTP reverse proxy — edge validation, SSE streaming IPC to agent, RAG orchestration | ~270 |

## Workspace Crates

| Crate | Description |
|---|---|
| `dogma-v2-common` | Shared error types, NDJSON event protocol, and foundational traits |
| `dogma-v2-core` | Async agent runtime — tool loop (RSI), LLM provider abstraction, state management on dogma-vdb, context compressor, **Enriched Inference (IE)**, **Cost Gate** |
| `dogma-v2-cli` | Terminal entrypoint — Clap-based command dispatch, NDJSON output mode, `/ei` slash command |

> **Note:** the `dogma-gateway` crate is no longer part of this
> workspace. It has been moved to
> [`dogmalab/dogma-gateway`](https://github.com/dogmalab/dogma-gateway)
> so the network boundary is in its own repository.

## Architecture

```
External Client ──HTTP──► dogma-gateway ──IPC pipes──► dogma-v2-core ──mmap──► dogma-vdb
                              │                              │
                              │                         dogma-v2-common
                              │                              │
                              └──────► dogma-v2-cli ◄────────┘
                                       (terminal entry)
```

- `dogma-gateway` is the only component with network access. It proxies to the agent via anonymous OS pipes (stdin/stdout).
- `dogma-v2-core` is completely network-isolated. All state lives in `dogma-vdb` via memory-mapped I/O.
- `dogma-v2-common` provides typed errors and NDJSON event types shared across all crates.
- `dogma-v2-cli` is a thin CLI wrapper around the core runtime.

**Important note for contributors:** the `dogma-gateway` crate that
appears in the architecture diagram and the legacy Cargo.toml is
**stub code only**. The real backends (IPC pipes to the agent,
mmap to the state harness) are F2 in the platform
[ROADMAP](https://github.com/dogmalab/.github/blob/main/ROADMAP.md).
Until then, `dogma-gateway` lives in its own repository and
returns hardcoded responses.

## Quick Start

```sh
# Check the entire workspace compiles
cargo check --workspace

# Run tests
cargo test --workspace

# Build the gateway
cargo build -p dogma-gateway

# Run the gateway (stub endpoints on :8080)
RUST_LOG=dogma_gateway=info cargo run -p dogma-gateway
```

## Dependencies

The workspace keeps dependencies minimal and shared through `[workspace.dependencies]`:

- `tokio` — async runtime
- `axum` — HTTP framework (gateway only)
- `serde` / `serde_json` — serialisation
- `tracing` / `tracing-subscriber` — structured logging (stderr)
- `thiserror` — typed error derives
- `parking_lot` — safe synchronisation
- `chrono` / `uuid` — timestamps and identifiers
- `dogma-vdb` — native vector database backend (external crate)

## Quality Standards

- **Version**: In development (v2)
- **Zero `unsafe`** — enforced via `#![deny(unsafe_code)]` in every crate
- **Zero `unwrap()` in handlers** — all errors use `?` with typed error enums
- **Strict JSON validation** — `#[serde(deny_unknown_fields)]` on all ingress types
- **Minimal allocations** — stack-local types, bounded channels, no premature abstractions
- **Release profile** — `opt-level = "z"`, `lto = true`, `strip = true` for small binaries

## License

MIT — see [LICENSE-MIT](LICENSE-MIT).

## Author

**Argimiro Gil** — [github.com/arggil](https://github.com/arggil) — Creator and maintainer of the Dogma ecosystem.
