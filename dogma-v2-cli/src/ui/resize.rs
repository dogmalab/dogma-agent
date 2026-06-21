//! # Resize — Manejo de redimensionamiento de terminal

/// Maneja el evento SIGWINCH (resize de terminal).
///
/// En crossterm, el resize se detecta automáticamente en `terminal.draw()`.
/// Esta función simplemente retorna `true` para indicar que se debe
/// re-dibujar después de un resize.
#[allow(dead_code)]
pub fn should_redraw() -> bool {
    // crossterm maneja SIGWINCH internamente.
    // La próxima llamada a `terminal.draw()` detectará el cambio
    // de tamaño y re-dibujará todo.
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_should_redraw() {
        assert!(super::should_redraw());
    }
}
