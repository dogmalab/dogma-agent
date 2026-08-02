//! # Chat — Renderizado del área de chat
//!
//! Muestra el historial de conversación con thinking blocks
//! y markdown renderizado.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

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
    /// Si el chat sigue automáticamente el contenido nuevo (fondo).
    /// Se desactiva cuando el usuario hace scroll hacia arriba.
    auto_scroll: bool,
}

impl ChatRenderer {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            thinking: String::new(),
            is_thinking: false,
            scroll_offset: 0,
            auto_scroll: true,
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
        self.auto_scroll = true;
    }

    /// Añade un error al chat.
    pub fn show_error(&mut self, msg: &str) {
        self.clear_thinking();
        self.content.push_str(&format!("[error] {msg}\n\n"));
    }

    /// Añade un mensaje informativo neutro (no es error).
    pub fn show_info(&mut self, msg: &str) {
        self.clear_thinking();
        self.content.push_str(&format!("[info] {msg}\n\n"));
    }

    /// Limpia todo el chat.
    pub fn clear(&mut self) {
        self.content.clear();
        self.thinking.clear();
        self.is_thinking = false;
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    /// Scroll al final del chat. Si `enable_follow` es `true`, reactiva
    /// el modo follow; si es `false`, solo posiciona sin cambiar el modo.
    pub fn scroll_to_bottom(&mut self, chat_height: usize) {
        let line_count = self.rendered_line_count();
        if line_count > chat_height {
            self.scroll_offset = (line_count - chat_height) as u16;
        } else {
            self.scroll_offset = 0;
        }
        self.auto_scroll = true;
    }

    /// Número de líneas estimado del contenido renderizado (markdown,
    /// sin contar el wrap por ancho).
    fn rendered_line_count(&self) -> usize {
        render_markdown(&self.content).len()
    }

    /// Scroll hacia arriba: desactiva el modo follow.
    pub fn scroll_up(&mut self) {
        self.auto_scroll = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    /// Scroll hacia abajo. Al llegar al fondo, reactiva el modo follow.
    pub fn scroll_down(&mut self, chat_height: usize) {
        let line_count = self.rendered_line_count();
        let max_scroll = line_count.saturating_sub(chat_height);
        let next = self.scroll_offset.saturating_add(3).min(max_scroll as u16);
        self.scroll_offset = next;
        if next as usize >= max_scroll {
            self.auto_scroll = true;
        }
    }

    pub fn scroll_top(&mut self) {
        self.auto_scroll = false;
        self.scroll_offset = 0;
    }

    /// `true` si el chat debe seguir el contenido nuevo (fondo).
    pub fn is_auto_scroll(&self) -> bool {
        self.auto_scroll
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

    #[test]
    fn test_auto_scroll_follow_mode() {
        let mut chat = ChatRenderer::new();
        for i in 0..20 {
            chat.push_content(&format!("line {i}\n"));
        }
        // Por defecto sigue el fondo.
        assert!(chat.is_auto_scroll());
        // Scroll up desactiva el follow.
        chat.scroll_up();
        assert!(!chat.is_auto_scroll());
        // Scroll al fondo reactiva.
        chat.scroll_to_bottom(5);
        assert!(chat.is_auto_scroll());
        // Un nuevo prompt reactiva follow.
        chat.scroll_up();
        assert!(!chat.is_auto_scroll());
        chat.show_sent("nuevo");
        assert!(chat.is_auto_scroll());
    }

    #[test]
    fn test_scroll_down_reaches_bottom() {
        let mut chat = ChatRenderer::new();
        for i in 0..20 {
            chat.push_content(&format!("line {i}\n"));
        }
        chat.scroll_top();
        assert!(!chat.is_auto_scroll());
        // Bajar muchas veces hasta el fondo reactiva follow.
        for _ in 0..20 {
            chat.scroll_down(5);
        }
        assert!(chat.is_auto_scroll());
    }
}
