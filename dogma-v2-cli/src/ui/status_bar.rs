//! # StatusBar — Barra de estado inferior
//!
//! Muestra: modelo, barra de contexto, tokens, branch de git.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

const BAR_WIDTH: usize = 10;

/// Barra de estado inferior del TUI.
pub struct StatusBar {
    model: String,
    tokens: u64,
    context_pct: f32,
    busy: bool,
    git_branch: Option<String>,
}

impl StatusBar {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            tokens: 0,
            context_pct: 0.0,
            busy: false,
            git_branch: None,
        }
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    pub fn update_tokens(&mut self, tokens: u64, context_pct: f32) {
        self.tokens = tokens;
        self.context_pct = context_pct;
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    pub fn is_busy(&self) -> bool {
        self.busy
    }

    #[allow(dead_code)]
    pub fn set_git_branch(&mut self, branch: Option<String>) {
        self.git_branch = branch;
    }

    /// Renderiza la barra de estado.
    pub fn render(&self, frame: &mut Frame, area: Rect, spinner_frame: &str) {
        let status_icon = if self.busy { spinner_frame } else { " " };

        let bar = build_context_bar(self.context_pct);
        let tokens = format_tokens(self.tokens);
        let git = self
            .git_branch
            .as_deref()
            .map(|b| format!(" ({b})"))
            .unwrap_or_default();

        let line = Line::from(vec![
            Span::raw(status_icon),
            Span::raw(" "),
            Span::styled(
                &self.model,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(&bar, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(&tokens, Style::default().fg(Color::DarkGray)),
            Span::styled(git, Style::default().fg(Color::DarkGray)),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }
}

/// Formatea tokens: 999 → "999", 1500 → "1.5K", 1500000 → "1.5M".
fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1000..=999_999 => format!("{:.1}K", count as f64 / 1000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// Construye una barra de contexto visual.
///
/// Ejemplo: `[████░░░░░░]` para 40%
fn build_context_bar(pct: f32) -> String {
    let filled = ((pct * BAR_WIDTH as f32) as usize).min(BAR_WIDTH);
    let empty = BAR_WIDTH - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5K");
        assert_eq!(format_tokens(15000), "15.0K");
        assert_eq!(format_tokens(1500000), "1.5M");
    }

    #[test]
    fn test_build_context_bar() {
        assert_eq!(build_context_bar(0.0), "[░░░░░░░░░░]");
        assert_eq!(build_context_bar(1.0), "[██████████]");
        assert_eq!(build_context_bar(0.5), "[█████░░░░░]");
    }
}
