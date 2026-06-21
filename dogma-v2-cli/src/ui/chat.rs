//! # Chat — Renderizado del área de chat
//!
//! Muestra el historial de conversación con thinking blocks
//! y markdown renderizado.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::markdown::render_markdown;

/// Renderer del área de chat.
pub struct ChatRenderer {
    /// Texto completo del chat (todas las respuestas concatenadas).
    content: String,
    /// Texto de thinking/reasoning actual.
    thinking: String,
    /// Si hay thinking activo.
    is_thinking: bool,
    /// Scroll offset del chat (en líneas).
    scroll_offset: u16,
}

impl ChatRenderer {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            thinking: String::new(),
            is_thinking: false,
            scroll_offset: 0,
        }
    }

    /// Añade contenido del asistente al chat.
    pub fn push_content(&mut self, text: &str) {
        self.content.push_str(text);
    }

    /// Añade texto de thinking/reasoning.
    pub fn push_thinking(&mut self, text: &str) {
        self.thinking.push_str(text);
        self.is_thinking = true;
    }

    /// Limpia el thinking y marca como inactivo.
    pub fn clear_thinking(&mut self) {
        self.thinking.clear();
        self.is_thinking = false;
    }

    /// Añade un prompt enviado por el usuario.
    pub fn show_sent(&mut self, prompt: &str) {
        self.content.push_str(&format!(">>> {prompt}\n\n"));
    }

    /// Añade un error al chat.
    pub fn show_error(&mut self, msg: &str) {
        self.clear_thinking();
        self.content.push_str(&format!("[error] {msg}\n\n"));
    }

    /// Limpia todo el chat.
    pub fn clear(&mut self) {
        self.content.clear();
        self.thinking.clear();
        self.is_thinking = false;
        self.scroll_offset = 0;
    }

    /// Scroll al final del chat.
    pub fn scroll_to_bottom(&mut self, chat_height: usize) {
        let line_count = self.content.lines().count();
        if line_count > chat_height {
            self.scroll_offset = (line_count - chat_height) as u16;
        } else {
            self.scroll_offset = 0;
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    pub fn scroll_down(&mut self, chat_height: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
        self.clamp_scroll(chat_height);
    }

    pub fn scroll_top(&mut self) {
        self.scroll_offset = 0;
    }

    #[allow(dead_code)]
    pub fn scroll_bottom(&mut self, chat_height: usize) {
        self.scroll_to_bottom(chat_height);
    }

    fn clamp_scroll(&mut self, chat_height: usize) {
        let line_count = self.content.lines().count();
        let max_scroll = line_count.saturating_sub(chat_height) as u16;
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    /// Renderiza el área de chat completa.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        // Thinking text (dimmed)
        if self.is_thinking && !self.thinking.is_empty() {
            for line in self.thinking.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.push(Line::from(""));
        }

        // Chat content con markdown
        let md_lines = render_markdown(&self.content);
        lines.extend(md_lines);

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    }
}

impl Default for ChatRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_content() {
        let mut chat = ChatRenderer::new();
        chat.push_content("Hello");
        assert_eq!(chat.content, "Hello");
    }

    #[test]
    fn test_thinking() {
        let mut chat = ChatRenderer::new();
        assert!(!chat.is_thinking);
        chat.push_thinking("reasoning...");
        assert!(chat.is_thinking);
        chat.clear_thinking();
        assert!(!chat.is_thinking);
    }

    #[test]
    fn test_scroll() {
        let mut chat = ChatRenderer::new();
        for i in 0..20 {
            chat.push_content(&format!("line {i}\n"));
        }
        chat.scroll_to_bottom(5);
        assert!(chat.scroll_offset > 0);
        chat.scroll_top();
        assert_eq!(chat.scroll_offset, 0);
    }
}
