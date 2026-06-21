//! # Tools — Display de ejecución de herramientas
//!
//! Muestra el estado de cada tool call: nombre, args, resultado.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Display de una ejecución de tool.
pub struct ToolDisplay {
    entries: Vec<ToolEntry>,
}

struct ToolEntry {
    name: String,
    status: ToolStatus,
    result: Option<String>,
}

enum ToolStatus {
    Running,
    Done,
    Error(String),
}

impl ToolDisplay {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registra que una tool empezó a ejecutarse.
    pub fn start(&mut self, name: &str) {
        self.entries.push(ToolEntry {
            name: name.to_string(),
            status: ToolStatus::Running,
            result: None,
        });
    }

    /// Marca una tool como completada exitosamente.
    pub fn finish(&mut self, name: &str, result: &str) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.name == name) {
            entry.status = ToolStatus::Done;
            entry.result = Some(truncate(result, 120));
        }
    }

    /// Marca una tool como fallida.
    pub fn fail(&mut self, name: &str, error: &str) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|e| e.name == name) {
            entry.status = ToolStatus::Error(truncate(error, 120));
        }
    }

    /// Limpia todas las entradas (después de completar una respuesta).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Renderiza las tool calls en el chat (como una fila separada).
    pub fn render_in_chat(&self, frame: &mut Frame, area: Rect) {
        if self.entries.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        self.render(&mut lines);
        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, area);
    }

    /// Renderiza las tool calls como líneas.
    pub fn render(&self, lines: &mut Vec<ratatui::text::Line<'static>>) {
        for entry in &self.entries {
            let (icon, style) = match &entry.status {
                ToolStatus::Running => (
                    "⟳".to_string(),
                    Style::default().fg(Color::Yellow),
                ),
                ToolStatus::Done => (
                    "✓".to_string(),
                    Style::default().fg(Color::Green),
                ),
                ToolStatus::Error(_) => (
                    "✗".to_string(),
                    Style::default().fg(Color::Red),
                ),
            };

            let name_style = Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD);

            let mut spans = vec![
                Span::styled(icon, style),
                Span::raw(" ".to_string()),
                Span::styled(entry.name.clone(), name_style),
            ];

            if let Some(ref result) = entry.result {
                spans.push(Span::raw(format!(" → {result}")));
            } else if let ToolStatus::Error(ref err) = entry.status {
                spans.push(Span::styled(
                    format!(" ✗ {err}"),
                    Style::default().fg(Color::Red),
                ));
            }

            lines.push(Line::from(spans));
        }
    }
}

impl Default for ToolDisplay {
    fn default() -> Self {
        Self::new()
    }
}

/// Trunca un string a `max` chars con "..." si es más largo.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_lifecycle() {
        let mut td = ToolDisplay::new();
        td.start("read_file");
        assert_eq!(td.entries.len(), 1);

        td.finish("read_file", "file content here");
        assert!(matches!(td.entries[0].status, ToolStatus::Done));
    }

    #[test]
    fn test_tool_error() {
        let mut td = ToolDisplay::new();
        td.start("write_file");
        td.fail("write_file", "permission denied");
        assert!(matches!(td.entries[0].status, ToolStatus::Error(_)));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world this is long", 10), "hello worl...");
    }
}
