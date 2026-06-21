//! # e2e_sandbox_cli — Integration tests for SandboxMode CLI integration
//!
//! Verifies that:
//! * `SandboxMode::WasmOnly` blocks native script execution via guardrails
//! * `SandboxMode` round-trips through `FromStr` and `Display`
//! * `SecurityConfig` correctly propagates sandbox_mode to inspect_command
//! * The full chain (parse → config → guardrail → verdict) works

use dogma_v2_core::tools::{
    CommandVerdict, SandboxMode, SecurityConfig, SecurityMode, ToolGuardrail,
};

fn set_sandbox_mode(mode: SandboxMode) {
    let allowed = vec![
        std::path::PathBuf::from("/tmp"),
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    ];
    ToolGuardrail::set_config(SecurityConfig {
        mode: SecurityMode::SemiAutonomous,
        allowed_dirs: allowed,
        sandbox_mode: mode,
        sandbox_limits: None,
    });
}

// ── SandboxMode::FromStr ───────────────────────────────────────────────

#[test]
fn test_sandbox_mode_parse_disabled() {
    assert_eq!(
        "disabled".parse::<SandboxMode>().unwrap(),
        SandboxMode::Disabled
    );
    assert_eq!("off".parse::<SandboxMode>().unwrap(), SandboxMode::Disabled);
    assert_eq!(
        "none".parse::<SandboxMode>().unwrap(),
        SandboxMode::Disabled
    );
}

#[test]
fn test_sandbox_mode_parse_enabled() {
    assert_eq!(
        "enabled".parse::<SandboxMode>().unwrap(),
        SandboxMode::Enabled
    );
    assert_eq!("on".parse::<SandboxMode>().unwrap(), SandboxMode::Enabled);
    assert_eq!("yes".parse::<SandboxMode>().unwrap(), SandboxMode::Enabled);
    assert_eq!(
        "sandbox".parse::<SandboxMode>().unwrap(),
        SandboxMode::Enabled
    );
}

#[test]
fn test_sandbox_mode_parse_wasm_only() {
    assert_eq!(
        "wasm-only".parse::<SandboxMode>().unwrap(),
        SandboxMode::WasmOnly
    );
    assert_eq!(
        "wasm_only".parse::<SandboxMode>().unwrap(),
        SandboxMode::WasmOnly
    );
    assert_eq!(
        "wasmonly".parse::<SandboxMode>().unwrap(),
        SandboxMode::WasmOnly
    );
    assert_eq!(
        "strict".parse::<SandboxMode>().unwrap(),
        SandboxMode::WasmOnly
    );
    assert_eq!(
        "WASM-ONLY".parse::<SandboxMode>().unwrap(),
        SandboxMode::WasmOnly
    );
}

#[test]
fn test_sandbox_mode_parse_invalid() {
    assert!("banana".parse::<SandboxMode>().is_err());
    assert!("".parse::<SandboxMode>().is_err());
}

#[test]
fn test_sandbox_mode_display_roundtrip() {
    for mode in &[
        SandboxMode::Disabled,
        SandboxMode::Enabled,
        SandboxMode::WasmOnly,
    ] {
        let s = mode.to_string();
        let parsed: SandboxMode = s
            .parse()
            .unwrap_or_else(|e| panic!("round-trip failed for {mode:?} ('{s}'): {e}"));
        assert_eq!(*mode, parsed);
    }
}

// ── CLI flag simulation: modo secuencial (comparten estado global) ─────

#[test]
fn test_cli_sandbox_modes_sequential() {
    // Verificar que WasmOnly bloquea bash
    set_sandbox_mode(SandboxMode::WasmOnly);
    let verdict = ToolGuardrail::inspect_command("bash", "echo blocked");
    assert!(
        matches!(&verdict, CommandVerdict::Blocked { .. }),
        "WasmOnly should block bash, got: {verdict:?}"
    );

    // Verificar que WasmOnly bloquea python
    let verdict = ToolGuardrail::inspect_command("python", "print('blocked')");
    assert!(
        matches!(&verdict, CommandVerdict::Blocked { .. }),
        "WasmOnly should block python, got: {verdict:?}"
    );

    // Verificar que WasmOnly bloquea node
    let verdict = ToolGuardrail::inspect_command("node", "console.log('blocked')");
    assert!(
        matches!(&verdict, CommandVerdict::Blocked { .. }),
        "WasmOnly should block node, got: {verdict:?}"
    );

    // Verificar que WasmOnly permite wasm
    let verdict = ToolGuardrail::inspect_command("wasm", "(module)");
    assert!(
        matches!(&verdict, CommandVerdict::Allowed),
        "WasmOnly should allow wasm, got: {verdict:?}"
    );

    // ── Disabled permite todo ──────────────────────────────────────
    set_sandbox_mode(SandboxMode::Disabled);
    let verdict = ToolGuardrail::inspect_command("bash", "echo allowed");
    assert!(
        matches!(&verdict, CommandVerdict::Allowed),
        "Disabled should allow bash, got: {verdict:?}"
    );
    let verdict = ToolGuardrail::inspect_command("python", "print('allowed')");
    assert!(
        matches!(&verdict, CommandVerdict::Allowed),
        "Disabled should allow python, got: {verdict:?}"
    );

    // ── Enabled permite todo ───────────────────────────────────────
    set_sandbox_mode(SandboxMode::Enabled);
    let verdict = ToolGuardrail::inspect_command("bash", "echo hello");
    assert!(
        matches!(&verdict, CommandVerdict::Allowed),
        "Enabled should allow bash, got: {verdict:?}"
    );
    let verdict = ToolGuardrail::inspect_command("wasm", "(module)");
    assert!(
        matches!(&verdict, CommandVerdict::Allowed),
        "Enabled should allow wasm, got: {verdict:?}"
    );
}

#[test]
fn test_security_config_default_sandbox_mode() {
    let default = SecurityConfig::default();
    assert_eq!(default.sandbox_mode, SandboxMode::Disabled);
    assert!(default.sandbox_limits.is_none());
}
