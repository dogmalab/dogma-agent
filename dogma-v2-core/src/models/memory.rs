//! # Memory — Tipos para el framework de memoria jerárquica
//!
//! Define los tipos base que gobiernan el estado físico, los objetivos
//! y la telemetría del contexto operativo del agente.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Tipo de nodo de memoria.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryType {
    /// Memoria episódica: interacciones pasadas del usuario.
    Episodic,
    /// Memoria semántica: conocimiento general extraído.
    Semantic,
    /// Memoria procedimental: habilidades y patrones aprendidos.
    Procedural,
}

/// Normaliza una ruta resolviendo componentes `..`.
///
/// Ejemplo: `/workspace/../etc/passwd` → `/etc/passwd`
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();

    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::Normal(c) => {
                result.push(c);
            }
            std::path::Component::RootDir => {
                result.push(std::path::Component::RootDir);
            }
            std::path::Component::CurDir => {}
            std::path::Component::Prefix(_) => {
                result.push(component);
            }
        }
    }

    result
}

/// Memoria de entorno del agente.
///
/// Almacena el estado del workspace, restricciones de rutas,
/// variables de entorno y directivas de negocio que el agente
/// debe poder recuperar si es necesario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentMemory {
    /// Directorio raíz del espacio de trabajo físico asignado al agente.
    pub current_working_dir: PathBuf,
    /// Lista negra absoluta de rutas del Sistema Operativo Host que el agente jamás puede tocar.
    pub forbidden_paths: HashSet<PathBuf>,
    /// Variables de entorno virtuales aisladas.
    pub env_vars: HashMap<String, String>,
    /// Herramientas inhibidas o bloqueadas temporalmente en este turno.
    pub active_tool_locks: HashSet<String>,
    /// Directivas de negocio o reglas que el agente debe recordar.
    pub business_directives: Vec<String>,
}

impl EnvironmentMemory {
    /// Crea una nueva EnvironmentMemory para un directorio de trabajo dado.
    pub fn new(workspace: PathBuf) -> Self {
        let mut forbidden = HashSet::new();
        #[cfg(unix)]
        {
            forbidden.insert(PathBuf::from("/etc"));
            forbidden.insert(PathBuf::from("/root"));
            forbidden.insert(PathBuf::from("/proc"));
            forbidden.insert(PathBuf::from("/sys"));
        }
        Self {
            current_working_dir: workspace,
            forbidden_paths: forbidden,
            env_vars: HashMap::new(),
            active_tool_locks: HashSet::new(),
            business_directives: Vec::new(),
        }
    }

    /// Valida que una ruta sea segura para el agente.
    ///
    /// Retorna `true` si la ruta está dentro del directorio de trabajo
    /// y no está en la lista negra. Normaliza `..` para prevenir
    /// ataques de path traversal.
    pub fn is_path_safe(&self, target: &std::path::Path) -> bool {
        // Verificar forbidden paths
        if self.forbidden_paths.iter().any(|f| target.starts_with(f)) {
            return false;
        }

        // Normalizar la ruta resolviendo componentes `..`
        let normalized = normalize_path(target);

        // Verificar que la ruta normalizada esté dentro del workspace
        normalized.starts_with(&self.current_working_dir)
    }

    /// Bloquea una herramienta temporalmente.
    pub fn lock_tool(&mut self, tool_name: &str) {
        self.active_tool_locks.insert(tool_name.to_string());
    }

    /// Desbloquea una herramienta.
    pub fn unlock_tool(&mut self, tool_name: &str) {
        self.active_tool_locks.remove(tool_name);
    }

    /// Verifica si una herramienta está bloqueada.
    pub fn is_tool_locked(&self, tool_name: &str) -> bool {
        self.active_tool_locks.contains(tool_name)
    }

    /// Agrega una directiva de negocio.
    pub fn add_directive(&mut self, directive: &str) {
        self.business_directives.push(directive.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_environment_memory_new() {
        let mem = EnvironmentMemory::new(PathBuf::from("/workspace"));
        assert_eq!(mem.current_working_dir, PathBuf::from("/workspace"));
        assert!(mem.env_vars.is_empty());
        assert!(mem.active_tool_locks.is_empty());
        assert!(mem.business_directives.is_empty());
    }

    #[test]
    fn test_is_path_safe() {
        let mem = EnvironmentMemory::new(PathBuf::from("/workspace"));
        assert!(mem.is_path_safe(&PathBuf::from("/workspace/file.txt")));
        assert!(!mem.is_path_safe(&PathBuf::from("/etc/passwd")));
        assert!(!mem.is_path_safe(&PathBuf::from("/workspace/../etc/passwd")));
    }

    #[test]
    fn test_tool_locks() {
        let mut mem = EnvironmentMemory::new(PathBuf::from("/workspace"));
        assert!(!mem.is_tool_locked("execute_script"));
        mem.lock_tool("execute_script");
        assert!(mem.is_tool_locked("execute_script"));
        mem.unlock_tool("execute_script");
        assert!(!mem.is_tool_locked("execute_script"));
    }

    #[test]
    fn test_business_directives() {
        let mut mem = EnvironmentMemory::new(PathBuf::from("/workspace"));
        mem.add_directive("Use Rust edition 2024");
        mem.add_directive("Maximum 300 lines per file");
        assert_eq!(mem.business_directives.len(), 2);
    }
}
