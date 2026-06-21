//! # Spinner — Indicador animado de actividad
//!
//! Rune cycle: ᚛ ᚜ ᛟ ᛝ ᛜ ᛛ
//! Ciclo lento (150ms) que evoca sabiduría antigua,
//! consistente con la estética "Dogma".

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Runas para el ciclo de animación.
const RUNES: [&str; 6] = ["᚛", "᚜", "ᛟ", "ᛝ", "ᛜ", "ᛛ"];

/// Indicador animado que muestra una rune rotativa.
pub struct Spinner {
    frame: usize,
    /// Mensaje junto a la rune (ej: "thinking...", "searching...").
    #[allow(dead_code)]
    label: String,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            frame: 0,
            label: String::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_label(label: impl Into<String>) -> Self {
        Self {
            frame: 0,
            label: label.into(),
        }
    }

    /// Avanza al siguiente frame de animación.
    pub fn tick(&mut self) {
        self.frame = (self.frame + 1) % RUNES.len();
    }

    /// Rune actual.
    pub fn current(&self) -> &str {
        RUNES[self.frame]
    }

    /// Renderiza el spinner en el area dado.
    #[allow(dead_code)]
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let symbol = self.current();
        let mut spans = vec![
            Span::styled(symbol, Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
        ];
        if !self.label.is_empty() {
            spans.push(Span::styled(
                &self.label,
                Style::default().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_new() {
        let s = Spinner::new();
        assert_eq!(s.current(), "᚛");
    }

    #[test]
    fn test_spinner_tick() {
        let mut s = Spinner::new();
        s.tick();
        assert_eq!(s.current(), "᚜");
        s.tick();
        assert_eq!(s.current(), "ᛟ");
    }

    #[test]
    fn test_spinner_wraps() {
        let mut s = Spinner::new();
        for _ in 0..RUNES.len() {
            s.tick();
        }
        assert_eq!(s.current(), "᚛");
    }

    #[test]
    fn test_spinner_with_label() {
        let s = Spinner::with_label("thinking...");
        assert_eq!(s.label, "thinking...");
    }
}
