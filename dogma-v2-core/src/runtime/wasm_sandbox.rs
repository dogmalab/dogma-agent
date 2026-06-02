//! # WasmSandbox — Ejecución aislada de módulos WebAssembly (WASI)
//!
//! Sustituye la ejecución nativa de comandos del SO por una micro-VM
//! basada en `wasmtime` con límites estrictos de CPU (fuel), sistema
//! de archivos virtual (`preopened_dir`) y punto de entrada WASI
//! (`_start`).
//!
//! ## Arquitectura
//!
//! ```text
//! Tool (execute_script)
//!   │
//!   └── WasmSandbox::run_captured()
//!         ├── Engine  (config + fuel + async)
//!         ├── Module  (wasm binary compilado)
//!         ├── Linker  (WASI preview1 bindings)
//!         ├── Store   (WasiP1Ctx + fuel state)
//!         └── Exec    (_start → stdout capture)
//! ```
//!
//! ## Seguridad
//!
//! * **Fuel**: Límite de instrucciones Wasm ejecutadas. Si se agota,
//!   la ejecución se aborta inmediatamente (previene bucles infinitos).
//! * **Filesystem**: Solo el directorio `allowed_workspace` se pre-abre
//!   como raíz virtual `/`. El módulo Wasm no puede escapar.
//! * **No raw syscalls**: El módulo solo puede usar las llamadas WASI
//!   (fd_write, fd_read, path_open, etc.) dentro de las pre-aperturas.

use std::io::Read;
use std::path::{Path, PathBuf};

use dogma_v2_common::error::Error;
use dogma_v2_common::Result;
use tracing::{debug, error, warn};
use wasmtime::*;
use wasmtime_wasi::preview1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::OutputFile;
use wasmtime_wasi::p2::WasiCtxBuilder;
use wasmtime_wasi::{DirPerms, FilePerms};

// ── Límites ────────────────────────────────────────────────────────────

/// Configuración de cuotas de recursos para el entorno virtualizado.
#[derive(Debug, Clone)]
pub struct SandboxLimits {
    /// Cantidad máxima de instrucciones Wasm ("combustible") permitidas
    /// antes del timeout. Una instrucción ≈ 1 operación de fuel.
    /// Valores típicos: 100_000 para scripts triviales, 10_000_000 para
    /// ejecución prolongada.
    pub max_fuel: u64,

    /// Ruta del sistema anfitrión que se pre-abre como raíz `/` dentro
    /// del sandbox. El módulo Wasm solo puede acceder archivos dentro
    /// de este directorio y sus subdirectorios.
    pub allowed_workspace: PathBuf,

    /// Directorio temporal para capturar stdout/stderr del sandbox.
    /// Si es `None`, se usa `std::env::temp_dir()`.
    pub temp_dir: Option<PathBuf>,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            max_fuel: 1_000_000,
            allowed_workspace: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            temp_dir: None,
        }
    }
}

impl SandboxLimits {
    /// Límites para scripts extremadamente cortos (echo, ls simple).
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            max_fuel: 100_000,
            ..Self::default()
        }
    }

    /// Límites para ejecución prolongada (compilación, procesamiento).
    #[must_use]
    pub fn generous() -> Self {
        Self {
            max_fuel: 100_000_000,
            ..Self::default()
        }
    }
}

// ── Resultado del sandbox ──────────────────────────────────────────────

/// Resultado de una ejecución en el sandbox.
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    /// Salida estándar capturada (stdout).
    pub stdout: String,
    /// Salida de error capturada (stderr).
    pub stderr: String,
    /// Combustible restante después de la ejecución.
    /// Si es `0`, la ejecución se abortó por límite de CPU.
    pub fuel_remaining: u64,
}

impl std::fmt::Display for SandboxOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if !self.stdout.is_empty() {
            f.write_str(&self.stdout)?;
        }
        if !self.stderr.is_empty() {
            if !self.stdout.is_empty() {
                f.write_str("\n")?;
            }
            f.write_str(&format!("[stderr]\n{}", self.stderr))?;
        }
        Ok(())
    }
}

// ── Sandbox principal ──────────────────────────────────────────────────

/// Entorno virtualizado que ejecuta un módulo WebAssembly con WASI.
///
/// El módulo Wasm se compila una vez en el constructor y se reutiliza
/// en múltiples invocaciones de `run_captured` o `run_inherited`.
pub struct WasmSandbox {
    engine: Engine,
    module: Module,
}

impl std::fmt::Debug for WasmSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmSandbox")
            .field("engine", &self.engine)
            .field("module", &self.module)
            .finish_non_exhaustive()
    }
}

impl WasmSandbox {
    /// Compila un binario Wasm en el motor de wasmtime.
    ///
    /// # Errors
    ///
    /// Devuelve `Error::Sandbox` si:
    /// * El motor no se puede configurar (fuel, async).
    /// * El binario Wasm no es válido o no se puede compilar.
    pub fn new(wasm_binary_bytes: &[u8]) -> Result<Self> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.async_support(true);

        let engine = Engine::new(&config).map_err(|e| Error::Sandbox {
            detail: format!("engine creation failed: {e}"),
        })?;

        let module = Module::new(&engine, wasm_binary_bytes).map_err(|e| Error::Sandbox {
            detail: format!("module compilation failed: {e}"),
        })?;

        debug!(
            "WasmSandbox initialized ({} bytes wasm)",
            wasm_binary_bytes.len()
        );

        Ok(Self { engine, module })
    }

    /// Devuelve una referencia al motor interno.
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Devuelve una referencia al módulo compilado.
    #[must_use]
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Ejecuta el módulo Wasm capturando stdout y stderr a Strings.
    ///
    /// Es el método principal para integración con herramientas: el
    /// output se devuelve como `SandboxOutput` en lugar de ir al terminal.
    ///
    /// # Errors
    ///
    /// * `Error::Sandbox` — Error de infraestructura del sandbox.
    /// * `Error::Security` — Aborto por límite de CPU excedido.
    pub async fn run_captured(
        &self,
        limits: &SandboxLimits,
        args: &[String],
    ) -> Result<SandboxOutput> {
        // ── Directorio temporal para capturar output ──────────────
        let tmp_root = limits
            .temp_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let session_dir = tmp_root.join(format!("wasm_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&session_dir).map_err(|e| Error::Sandbox {
            detail: format!("cannot create temp dir: {e}"),
        })?;

        let out_path = session_dir.join("stdout");
        let err_path = session_dir.join("stderr");

        let out_file = std::fs::File::create(&out_path).map_err(|e| Error::Sandbox {
            detail: format!("cannot create stdout file: {e}"),
        })?;
        let err_file = std::fs::File::create(&err_path).map_err(|e| Error::Sandbox {
            detail: format!("cannot create stderr file: {e}"),
        })?;

        // ── Construir contexto WASI ───────────────────────────────
        let wasi_ctx = self.build_wasi_ctx(limits, args, out_file, err_file)?;

        // ── Ejecutar ──────────────────────────────────────────────
        let fuel_remaining = self.execute_wasi(wasi_ctx, limits.max_fuel).await?;

        // ── Leer output capturado ─────────────────────────────────
        let mut stdout = String::new();
        std::fs::File::open(&out_path)
            .map_err(|e| Error::Sandbox {
                detail: format!("cannot read stdout: {e}"),
            })?
            .read_to_string(&mut stdout)
            .map_err(|e| Error::Sandbox {
                detail: format!("stdout read error: {e}"),
            })?;

        let mut stderr = String::new();
        std::fs::File::open(&err_path)
            .map_err(|e| Error::Sandbox {
                detail: format!("cannot read stderr: {e}"),
            })?
            .read_to_string(&mut stderr)
            .map_err(|e| Error::Sandbox {
                detail: format!("stderr read error: {e}"),
            })?;

        // ── Limpiar ───────────────────────────────────────────────
        let _ = std::fs::remove_dir_all(&session_dir);

        Ok(SandboxOutput {
            stdout,
            stderr,
            fuel_remaining,
        })
    }

    /// Ejecuta el módulo Wasm heredando stdout/stderr del proceso padre.
    ///
    /// Útil para depuración o cuando la salida debe ir al terminal
    /// directamente (modo interactivo). Devuelve el fuel restante.
    ///
    /// # Errors
    ///
    /// * `Error::Sandbox` — Error de infraestructura.
    /// * `Error::Security` — Aborto por límite de CPU.
    pub async fn run_inherited(&self, limits: &SandboxLimits, args: &[String]) -> Result<u64> {
        let mut builder = WasiCtxBuilder::new();
        builder.args(args).inherit_stdout().inherit_stderr();
        Self::add_preopens(&mut builder, limits)?;
        let wasi_ctx = builder.build_p1();
        self.execute_wasi(wasi_ctx, limits.max_fuel).await
    }

    // ── Helpers internos ──────────────────────────────────────────

    /// Construye un `WasiP1Ctx` con captura de output y preopened dir.
    fn build_wasi_ctx(
        &self,
        limits: &SandboxLimits,
        args: &[String],
        stdout_file: std::fs::File,
        stderr_file: std::fs::File,
    ) -> Result<WasiP1Ctx> {
        let mut builder = WasiCtxBuilder::new();
        builder.args(args);
        builder.stdout(OutputFile::new(stdout_file));
        builder.stderr(OutputFile::new(stderr_file));
        Self::add_preopens(&mut builder, limits)?;
        Ok(builder.build_p1())
    }

    /// Añade directorios pre-abiertos al builder.
    fn add_preopens(builder: &mut WasiCtxBuilder, limits: &SandboxLimits) -> Result<()> {
        let ws_path = Path::new(&limits.allowed_workspace);
        if ws_path.is_dir() {
            builder
                .preopened_dir(ws_path, "/", DirPerms::all(), FilePerms::all())
                .map_err(|e| Error::Sandbox {
                    detail: format!(
                        "cannot preopen dir '{}': {e}",
                        ws_path.display()
                    ),
                })?;
        } else {
            warn!(
                "Workspace '{}' is not a directory — no preopened dirs",
                ws_path.display()
            );
        }
        Ok(())
    }

    /// Ejecuta el módulo Wasm con un contexto WASI dado.
    async fn execute_wasi(&self, wasi_ctx: WasiP1Ctx, max_fuel: u64) -> Result<u64> {
        let mut linker = Linker::<WasiP1Ctx>::new(&self.engine);
        preview1::add_to_linker_async(&mut linker, |ctx: &mut WasiP1Ctx| ctx).map_err(|e| {
            Error::Sandbox {
                detail: format!("linker setup failed: {e}"),
            }
        })?;

        let mut store = Store::new(&self.engine, wasi_ctx);

        // Inyectar combustible máximo (límite de CPU)
        store.set_fuel(max_fuel).map_err(|e| Error::Sandbox {
            detail: format!("fuel injection failed: {e}"),
        })?;

        let instance = linker
            .instantiate_async(&mut store, &self.module)
            .await
            .map_err(|e| Error::Sandbox {
                detail: format!("instance creation failed: {e}"),
            })?;

        let entry_point = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| Error::Sandbox {
                detail: format!("_start not found: {e}"),
            })?;

        // Ejecutar dentro de la micro-VM
        match entry_point.call_async(&mut store, ()).await {
            Ok(_) => {
                let remaining = store.get_fuel().unwrap_or(0);
                debug!("WASM execution completed, fuel remaining: {remaining}");
                Ok(remaining)
            }
            Err(e) => {
                // Si get_fuel falla, asumimos 0 (seguro: abortar)
                let remaining = store.get_fuel().unwrap_or(0);
                if remaining == 0 {
                    warn!("WASM execution aborted: CPU limit exceeded");
                    Err(Error::Security(
                        "CPU limit exceeded — possible infinite loop".to_string(),
                    ))
                } else {
                    error!("WASM execution failed: {e}");
                    Err(Error::Sandbox {
                        detail: format!("execution failed: {e}"),
                    })
                }
            }
        }
    }
}

// ── Factory ────────────────────────────────────────────────────────────

/// Crea un `WasmSandbox` desde un archivo `.wasm` en disco.
///
/// # Errors
///
/// Devuelve `Error::Sandbox` si el archivo no existe o no es un
/// binario Wasm válido.
pub fn sandbox_from_file(path: impl AsRef<Path>) -> Result<WasmSandbox> {
    let bytes = std::fs::read(path.as_ref()).map_err(|e| Error::Sandbox {
        detail: format!(
            "cannot read wasm file '{}': {e}",
            path.as_ref().display()
        ),
    })?;
    WasmSandbox::new(&bytes)
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Un módulo WASI mínimo en WAT que escribe "hello from wasm\n" en
    /// stdout y "error msg\n" en stderr.
    ///
    /// Usa la ABI estándar de WASI preview1:
    ///   fd_write(fd: i32, iovs: i32, iovs_len: i32, nwritten: i32) -> i32
    /// donde cada iov es {buf: i32, buf_len: i32} (8 bytes).
    const HELLO_WAT: &str = r#"
(module
    (import "wasi_snapshot_preview1" "fd_write"
        (func $fd_write (param i32 i32 i32 i32) (result i32)))
    (memory (export "memory") 1)
    (export "_start" (func $_start))
    (func $_start
        ;; stdout: fd_write(1, &iov, 1, &nwritten)
        i32.const 1
        i32.const 64
        i32.const 1
        i32.const 80
        call $fd_write
        drop
        ;; stderr: fd_write(2, &iov2, 1, &nwritten)
        i32.const 2
        i32.const 72
        i32.const 1
        i32.const 80
        call $fd_write
        drop
    )
    ;; "hello from wasm\n" (17 bytes)
    (data (i32.const 8) "hello from wasm\n")
    ;; "error msg\n" (10 bytes)
    (data (i32.const 32) "error msg\n")
    ;; iov[0] stdout: {buf=8, buf_len=17}
    (data (i32.const 64) "\08\00\00\00\11\00\00\00")
    ;; iov2[0] stderr: {buf=32, buf_len=10}
    (data (i32.const 72) "\20\00\00\00\0a\00\00\00")
    ;; nwritten (4 bytes, queda a cero en offset 80)
)
"#;

    /// Módulo con bucle infinito — debe ser abortado por fuel.
    const INFINITE_LOOP_WAT: &str = r#"
(module
    (memory (export "memory") 1)
    (export "_start" (func $_start))
    (func $_start
        (loop
            br 0
        )
    )
)
"#;

    /// Módulo vacío — solo verificar que no crashea.
    const EMPTY_WAT: &str = r#"
(module
    (memory (export "memory") 1)
    (export "_start" (func $_start))
    (func $_start)
)
"#;

    fn compile_wat(wat: &str) -> Vec<u8> {
        wat::parse_str(wat).expect("WAT should be valid")
    }

    #[tokio::test]
    async fn test_sandbox_hello() {
        let wasm = compile_wat(HELLO_WAT);
        let sandbox = WasmSandbox::new(&wasm).expect("sandbox creation");
        let limits = SandboxLimits::minimal();
        let args: Vec<String> = vec![];

        let output = sandbox.run_captured(&limits, &args).await.expect("execution");
        assert!(output.stdout.contains("hello from wasm"), "stdout: {:?}", output.stdout);
        assert!(output.stderr.contains("error msg"), "stderr: {:?}", output.stderr);
        assert!(
            output.fuel_remaining < 100_000,
            "fuel_remaining: {}",
            output.fuel_remaining
        );
    }

    #[tokio::test]
    async fn test_sandbox_empty_module() {
        let wasm = compile_wat(EMPTY_WAT);
        let sandbox = WasmSandbox::new(&wasm).expect("sandbox creation");
        let limits = SandboxLimits::minimal();
        let args: Vec<String> = vec![];

        let output = sandbox.run_captured(&limits, &args).await.expect("execution");
        assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
        assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
    }

    #[tokio::test]
    async fn test_sandbox_infinite_loop_killed_by_fuel() {
        let wasm = compile_wat(INFINITE_LOOP_WAT);
        let sandbox = WasmSandbox::new(&wasm).expect("sandbox creation");

        // Fuel muy bajo para que el bucle se aborte rápido
        let limits = SandboxLimits {
            max_fuel: 1_000,
            ..SandboxLimits::minimal()
        };
        let args: Vec<String> = vec![];

        let result = sandbox.run_captured(&limits, &args).await;
        let err = result.expect_err("infinite loop should be killed by fuel");
        assert!(
            err.to_string().contains("CPU limit exceeded"),
            "expected CPU limit error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_sandbox_from_file() {
        // Crear un .wasm temporal desde WAT
        let wasm_bytes = compile_wat(EMPTY_WAT);
        let tmp = tempfile::tempdir().expect("temp dir");
        let wasm_path = tmp.path().join("test.wasm");
        std::fs::write(&wasm_path, &wasm_bytes).expect("write wasm");

        let sandbox = sandbox_from_file(&wasm_path).expect("sandbox from file");
        let limits = SandboxLimits::minimal();
        let args: Vec<String> = vec![];

        let output = sandbox.run_captured(&limits, &args).await.expect("execution");
        assert!(output.stdout.is_empty());
    }

    #[tokio::test]
    async fn test_sandbox_invalid_wasm() {
        let invalid = b"not valid wasm";
        let result = WasmSandbox::new(invalid);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("module compilation failed"));
    }

    #[tokio::test]
    async fn test_sandbox_max_fuel_enforced() {
        let wasm = compile_wat(HELLO_WAT);
        let sandbox = WasmSandbox::new(&wasm).expect("sandbox creation");

        // Fuel insuficiente para completar el programa
        let limits = SandboxLimits {
            max_fuel: 1,
            ..SandboxLimits::minimal()
        };
        let args: Vec<String> = vec![];
        let result = sandbox.run_captured(&limits, &args).await;
        let err = result.expect_err("should fail with insufficient fuel");
        assert!(
            err.to_string().contains("CPU limit exceeded"),
            "expected CPU limit error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_sandbox_generous_limits() {
        let wasm = compile_wat(HELLO_WAT);
        let sandbox = WasmSandbox::new(&wasm).expect("sandbox creation");
        let limits = SandboxLimits::generous();
        let args: Vec<String> = vec![];
        let output = sandbox.run_captured(&limits, &args).await.expect("execution");
        assert!(output.stdout.contains("hello from wasm"));
        assert!(
            output.fuel_remaining > 0,
            "fuel_remaining: {}",
            output.fuel_remaining
        );
    }

    #[test]
    fn test_sandbox_limits_defaults() {
        let limits = SandboxLimits::default();
        assert_eq!(limits.max_fuel, 1_000_000);
        assert!(limits.allowed_workspace.exists());
    }

    #[test]
    fn test_sandbox_output_display() {
        let output = SandboxOutput {
            stdout: "hello".to_string(),
            stderr: "error".to_string(),
            fuel_remaining: 500,
        };
        let displayed = format!("{output}");
        assert!(displayed.contains("hello"));
        assert!(displayed.contains("error"));
        assert!(displayed.contains("[stderr]"));
    }

    #[test]
    fn test_sandbox_output_display_stdout_only() {
        let output = SandboxOutput {
            stdout: "only stdout".to_string(),
            stderr: String::new(),
            fuel_remaining: 500,
        };
        let displayed = format!("{output}");
        assert_eq!(displayed, "only stdout");
    }
}
