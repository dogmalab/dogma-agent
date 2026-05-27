# dogma-agent

> **Active workspace for the Dogma ecosystem — Agent runtime, IPC gateway, and shared types.**
> Low-dependency, CLI-first, state persisted in dogma-vdb.

dogma-agent is a Rust workspace that powers the next generation of the Dogma AI agent framework. It replaces the original Dogma 1.x monolith with a minimal, decoupled design: a **common type library**, a **core agent runtime**, a **CLI facade**, and a **network gateway**.

## Workspace Crates

| Crate | Description | LOC |
|-------|-------------|-----|
| `dogma-v2-common` | Shared error types, NDJSON event protocol, and foundational traits | ~270 |
| `dogma-v2-core` | Async agent runtime — tool loop (RSI), LLM provider abstraction, state management on dogma-vdb, context compressor | ~1,800 |
| `dogma-v2-cli` | Terminal entrypoint — Clap-based command dispatch, NDJSON output mode | ~265 |
| `dogma-gateway` | Axum HTTP reverse proxy — edge validation, SSE streaming IPC to agent, RAG orchestration | ~270 |

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
