//! # System Context — Detección automática del entorno
//!
//! Detecta OS, arquitectura, shell, directorio de trabajo, tipo de
//! proyecto, estado de git, y reglas del proyecto (AGENTS.md).
//!
//! La información se inyecta en el system prompt al inicio de cada sesión.

use std::path::{Path, PathBuf};
use tracing::debug;

/// Contexto del sistema detectado automáticamente.
#[derive(Debug, Clone)]
pub struct SystemContext {
    /// SO: "linux", "macos", "windows".
    pub os: String,
    /// Arquitectura: "x86_64", "aarch64".
    pub arch: String,
    /// Shell: "bash", "zsh", "fish", "powershell".
    pub shell: String,
    /// Directorio de trabajo actual.
    pub cwd: PathBuf,
    /// Tipo de proyecto detectado: "rust", "python", "node", "go", None.
    pub project_type: Option<String>,
    /// Branch de git actual (si el cwd es un repo git).
    pub git_branch: Option<String>,
    /// Si hay cambios sin commitear.
    pub git_dirty: bool,
    /// Reglas del proyecto (contenido de AGENTS.md o .cursorrules).
    pub project_rules: Option<String>,
}

impl SystemContext {
    /// Detecta el entorno del sistema automáticamente.
    pub fn detect() -> Self {
        let os = detect_os();
        let arch = detect_arch();
        let shell = detect_shell();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let project_type = detect_project_type(&cwd);
        let (git_branch, git_dirty) = detect_git_status(&cwd);
        let project_rules = load_project_rules(&cwd);

        debug!(
            "SystemContext: os={os} arch={arch} shell={shell} project={:?} git={:?}",
            project_type, git_branch
        );

        Self {
            os,
            arch,
            shell,
            cwd,
            project_type,
            git_branch,
            git_dirty,
            project_rules,
        }
    }

    /// Genera una sección de texto para inyectar en el system prompt.
    pub fn to_prompt_section(&self) -> String {
        let mut section = String::from("SYSTEM CONTEXT:\n");

        section.push_str(&format!("  OS: {} {}\n", self.os, self.arch));
        section.push_str(&format!("  Shell: {}\n", self.shell));
        section.push_str(&format!("  Working directory: {}\n", self.cwd.display()));

        if let Some(ref pt) = self.project_type {
            section.push_str(&format!("  Project type: {pt}\n"));
        }

        if let Some(ref branch) = self.git_branch {
            let dirty = if self.git_dirty { " (dirty)" } else { "" };
            section.push_str(&format!("  Git branch: {branch}{dirty}\n"));
        }

        if let Some(ref rules) = self.project_rules {
            section.push_str(&format!("  Project rules:\n{rules}\n"));
        }

        section
    }
}

/// Detecta el SO.
fn detect_os() -> String {
    std::env::consts::OS.to_string()
}

/// Detecta la arquitectura.
fn detect_arch() -> String {
    std::env::consts::ARCH.to_string()
}

/// Detecta el shell.
fn detect_shell() -> String {
    #[cfg(unix)]
    {
        if let Ok(shell) = std::env::var("SHELL") {
            let shell_name = Path::new(&shell)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            return shell_name.to_string();
        }
    }
    #[cfg(windows)]
    {
        if std::env::var("COMSPEC").is_ok() {
            return "powershell".to_string();
        }
    }
    "unknown".to_string()
}

/// Detecta el tipo de proyecto buscando archivos característicos.
fn detect_project_type(cwd: &Path) -> Option<String> {
    if cwd.join("Cargo.toml").exists() {
        return Some("rust".to_string());
    }
    if cwd.join("pyproject.toml").exists() || cwd.join("setup.py").exists() {
        return Some("python".to_string());
    }
    if cwd.join("package.json").exists() {
        return Some("node".to_string());
    }
    if cwd.join("go.mod").exists() {
        return Some("go".to_string());
    }
    None
}

/// Detecta branch de git y estado dirty.
fn detect_git_status(cwd: &Path) -> (Option<String>, bool) {
    // Buscar .git hacia arriba
    let mut dir = cwd.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            break;
        }
        if !dir.pop() {
            return (None, false);
        }
    }

    // Leer HEAD
    let head_file = dir.join(".git/HEAD");
    let branch = std::fs::read_to_string(&head_file)
        .ok()
        .and_then(|s| {
            s.trim()
                .strip_prefix("ref: refs/heads/")
                .map(String::from)
        });

    // Detectar dirty: buscar archivos modificados
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(cwd)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);

    (branch, dirty)
}

/// Carga reglas del proyecto desde AGENTS.md o .cursorrules.
fn load_project_rules(cwd: &Path) -> Option<String> {
    // Prioridad: AGENTS.md > .cursorrules
    let candidates = ["AGENTS.md", ".cursorrules"];
    for name in &candidates {
        let path = cwd.join(name);
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                // Limitar a 2000 chars para no saturar el prompt
                let truncated = if content.len() > 2000 {
                    format!("{}...(truncated)", &content[..2000])
                } else {
                    content
                };
                return Some(truncated);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_os() {
        let os = detect_os();
        assert!(!os.is_empty());
    }

    #[test]
    fn test_detect_arch() {
        let arch = detect_arch();
        assert!(!arch.is_empty());
    }

    #[test]
    fn test_system_context_detect() {
        let ctx = SystemContext::detect();
        assert!(!ctx.os.is_empty());
        assert!(!ctx.arch.is_empty());
        assert!(!ctx.cwd.as_os_str().is_empty());
    }

    #[test]
    fn test_to_prompt_section() {
        let ctx = SystemContext {
            os: "linux".into(),
            arch: "x86_64".into(),
            shell: "zsh".into(),
            cwd: PathBuf::from("/workspace"),
            project_type: Some("rust".into()),
            git_branch: Some("main".into()),
            git_dirty: false,
            project_rules: None,
        };
        let section = ctx.to_prompt_section();
        assert!(section.contains("linux"));
        assert!(section.contains("rust"));
        assert!(section.contains("main"));
    }

    #[test]
    fn test_to_prompt_section_with_rules() {
        let ctx = SystemContext {
            os: "linux".into(),
            arch: "x86_64".into(),
            shell: "zsh".into(),
            cwd: PathBuf::from("/workspace"),
            project_type: None,
            git_branch: None,
            git_dirty: false,
            project_rules: Some("Rule 1: Be concise".into()),
        };
        let section = ctx.to_prompt_section();
        assert!(section.contains("Rule 1"));
    }
}
