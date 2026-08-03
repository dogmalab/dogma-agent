//! # Renderer — Orquestador del UI
//!
//! Coordina los módulos de rendering: chat, spinner, status bar, tools.
//! Layout:
//! ```text
//! Row 0-N-5: chat area (scrollable)
//! Row N-4:   tool calls
//! Row N-3:   ▎ input (dynamic height, grows with content)
//! Row N-2:   separator
//! Row N-1:   status bar (model, context, tokens)
//! ```

use crossterm::terminal;
use dogma_v2_core::models::events::AgentEvent;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use super::{ChatRenderer, Spinner, StatusBar, ToolDisplay};

/// Máximo de filas visibles del input antes de hacer scroll interno.
const MAX_INPUT_ROWS: u16 = 6;

pub struct Renderer {
    terminal: Option<Terminal<CrosstermBackend<std::io::Stderr>>>,
    chat: ChatRenderer,
    spinner: Spinner,
    status: StatusBar,
    tools: ToolDisplay,
    input_buffer: String,
    initialized: bool,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            terminal: None,
            chat: ChatRenderer::new(),
            spinner: Spinner::new(),
            status: StatusBar::new("unknown"),
            tools: ToolDisplay::new(),
            input_buffer: String::new(),
            initialized: false,
        }
    }

    /// Inicializa: alternate screen, ratatui terminal.
    pub fn init(&mut self) {
        if self.initialized {
            return;
        }

        let _ = terminal::enable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::cursor::Show,
        );

        let backend = CrosstermBackend::new(std::io::stderr());
        let terminal = Terminal::new(backend).expect("failed to create terminal");
        self.terminal = Some(terminal);

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = crossterm::execute!(
                std::io::stderr(),
                crossterm::event::DisableMouseCapture,
                crossterm::cursor::Show,
            );
            let _ = terminal::disable_raw_mode();
            let _ =
                crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen,);
            original_hook(panic_info);
        }));

        self.initialized = true;
        self.draw();
    }

    /// Renderiza todo el UI.
    fn draw(&mut self) {
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };

        let input_buffer = self.input_buffer.clone();
        let spinner_frame = self.spinner.current().to_string();

        let _ = terminal.draw(|frame| {
            let area = frame.area();
            let inner = inset(area, 2);

            let input_area_width = inner.width.max(1);
            let input_rows = Self::input_rows(&input_buffer, input_area_width);

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),             // chat
                    Constraint::Length(1),          // tools
                    Constraint::Length(1),          // separator
                    Constraint::Length(input_rows), // input (dynamic)
                    Constraint::Length(1),          // separator
                    Constraint::Length(1),          // status bar
                ])
                .split(inner);

            self.chat.render(frame, chunks[0]);
            self.tools.render_in_chat(frame, chunks[1]);
            render_separator(frame, chunks[2]);
            render_input(frame, chunks[3], &input_buffer);
            render_separator(frame, chunks[4]);
            self.status.render(frame, chunks[5], &spinner_frame);
        });
    }

    /// Calcula el alto dinámico del input a partir del contenido.
    ///
    /// Cada línea explícita (separada por `\n`) ocupa `ceil(len / width)`
    /// filas. El resultado se clampa a `[1, MAX_INPUT_ROWS]`.
    fn input_rows(buffer: &str, width: u16) -> u16 {
        let width = width.max(1) as usize;
        let mut rows: u16 = 0;
        for line in buffer.split('\n') {
            let line_width = line.width();
            let line_rows = if line.is_empty() {
                1
            } else {
                line_width.div_ceil(width).max(1) as u16
            };
            rows = rows.saturating_add(line_rows);
            if rows >= MAX_INPUT_ROWS {
                return MAX_INPUT_ROWS;
            }
        }
        // Si la última línea llena exactamente el ancho, el cursor
        // avanza a la siguiente fila.
        let lines: Vec<&str> = buffer.split('\n').collect();
        if let Some(last) = lines.last() {
            if !last.is_empty() && last.width() % width == 0 {
                rows += 1;
            }
        }
        rows.clamp(1, MAX_INPUT_ROWS)
    }

    /// Alto del área de chat (resto de la terminal tras tools, input, status).
    fn chat_area_height(&self) -> usize {
        let Some(terminal) = self.terminal.as_ref() else {
            return 0;
        };
        let size = terminal.size().unwrap_or(ratatui::layout::Size {
            width: 80,
            height: 24,
        });
        let inner_height = size.height as usize;
        let inner_width = size.width.saturating_sub(4).max(1);
        let input_rows = Self::input_rows(&self.input_buffer, inner_width) as usize;
        // tools(1) + sep(1) + input + sep(1) + status(1)
        inner_height.saturating_sub(4 + input_rows)
    }

    /// Scroll al final del chat.
    fn scroll_to_bottom(&mut self) {
        self.chat.scroll_to_bottom(self.chat_area_height());
    }

    // ── Public API (mantenida para compatibilidad con main.rs) ──────

    pub fn set_model(&mut self, model: &str) {
        self.status.set_model(model);
        self.draw();
    }

    /// Configura la ventana de contexto del modelo para el % real de uso.
    pub fn set_context_window(&mut self, window: u32) {
        self.status.set_context_window(window);
        self.draw();
    }

    pub fn tick(&mut self) {
        self.spinner.tick();
        if self.status.is_busy() {
            self.draw();
        }
    }

    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SubAgentSpawned { description, .. } => {
                self.tools.start(&description);
                self.draw();
            }
            AgentEvent::StageChanged { .. } => {}
            AgentEvent::ToolExecuted {
                tool_name,
                duration_ms,
                ..
            } => {
                self.tools
                    .finish(&tool_name, &format!("done in {duration_ms}ms"));
                self.draw();
            }
            AgentEvent::ToolError { tool_name, message } => {
                // Mostrar el error de tool en el área de chat, no sobre el input.
                self.chat.show_error(&format!("{tool_name}: {message}"));
                if self.chat.is_auto_scroll() {
                    self.scroll_to_bottom();
                }
                self.draw();
            }
            AgentEvent::GoalEvaluated { completed, .. } => {
                if !completed {
                    self.tools.fail("goal", "goal failed");
                }
                self.draw();
            }
            AgentEvent::StatusUpdate {
                context_used,
                total_tokens,
                model,
            } => {
                self.status.update_tokens(total_tokens, context_used);
                if !model.is_empty() {
                    self.status.set_model(&model);
                }
                self.draw();
            }
            AgentEvent::ThinkingChunk { content } => {
                self.chat.push_thinking(&content);
                self.draw();
            }
            AgentEvent::ContentChunk { content } => {
                self.chat.push_content(&content);
                if self.chat.is_auto_scroll() {
                    self.scroll_to_bottom();
                }
                self.draw();
            }
            AgentEvent::SubAgentTerminated { .. } => {}
        }
    }

    pub fn finish_response(&mut self) {
        self.chat.clear_thinking();
        self.status.set_busy(false);
        self.tools.clear();
        self.chat.push_content("\n\n");
        if self.chat.is_auto_scroll() {
            self.scroll_to_bottom();
        }
        self.draw();
    }

    pub fn show_error(&mut self, msg: &str) {
        self.chat.show_error(msg);
        self.status.set_busy(false);
        if self.chat.is_auto_scroll() {
            self.scroll_to_bottom();
        }
        self.draw();
    }

    /// Muestra un mensaje informativo neutro (ej. cancelación).
    pub fn show_info(&mut self, msg: &str) {
        self.chat.show_info(msg);
        self.status.set_busy(false);
        if self.chat.is_auto_scroll() {
            self.scroll_to_bottom();
        }
        self.draw();
    }

    pub fn show_input(&mut self, buffer: &str) {
        self.input_buffer = buffer.to_string();
        self.draw();
    }

    pub fn reset_output(&mut self) {
        self.chat.clear();
        self.draw();
    }

    pub fn show_sent(&mut self, prompt: &str) {
        self.chat.show_sent(prompt);
        self.status.set_busy(true);
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn show_queued(&self, _prompt: &str) {}

    pub fn show_busy(&mut self) {
        self.status.set_busy(true);
        self.draw();
    }

    pub fn scroll_up(&mut self) {
        self.chat.scroll_up();
        self.draw();
    }

    pub fn scroll_down(&mut self) {
        let chat_height = self.chat_area_height();
        self.chat.scroll_down(chat_height);
        self.draw();
    }

    pub fn scroll_top(&mut self) {
        self.chat.scroll_top();
        self.draw();
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn cleanup(&mut self) {
        self.terminal.take();
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stderr(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::cursor::Show,
        );
    }
}

// ── Funciones de renderizado auxiliares ─────────────────────────────

fn inset(area: Rect, margin: u16) -> Rect {
    Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin * 2),
        height: area.height,
    }
}

fn render_separator(frame: &mut ratatui::Frame, area: Rect) {
    let separator = "─".repeat(area.width as usize);
    let paragraph = Paragraph::new(Span::styled(
        separator,
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(paragraph, area);
}

fn render_input(frame: &mut ratatui::Frame, area: Rect, input_buffer: &str) {
    let width = area.width.max(1) as usize;

    let lines: Vec<Line> = input_buffer
        .split('\n')
        .map(|line| Line::from(Span::raw(line.to_string())))
        .collect();

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    // Posicionar el cursor en su posición visual real tras el wrap.
    let (col, row) = cursor_position(input_buffer, width);
    let cursor_x = area.x.saturating_add(col as u16);
    let cursor_y = area
        .y
        .saturating_add((row as u16).min(area.height.saturating_sub(1)));
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Calcula la posición visual (columna, fila 0-indexada) del cursor dentro
/// del input, teniendo en cuenta el wrap a `width` columnas y el display
/// width de cada carácter (emojis/CJK = 2 columnas).
fn cursor_position(buffer: &str, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let lines: Vec<&str> = buffer.split('\n').collect();

    let rows_before: usize = lines[..lines.len().saturating_sub(1)]
        .iter()
        .map(|l| {
            let w = l.width();
            if w == 0 { 1 } else { w.div_ceil(width).max(1) }
        })
        .sum();

    let last_width = lines.last().copied().unwrap_or("").width();
    let row = rows_before + (last_width / width);
    let col = if last_width > 0 && last_width % width == 0 {
        0
    } else {
        last_width % width
    };

    (col, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inset() {
        let area = Rect::new(0, 0, 80, 24);
        let inner = inset(area, 2);
        assert_eq!(inner.x, 2);
        assert_eq!(inner.width, 76);
    }

    #[test]
    fn test_input_rows_grows_with_content() {
        // Ancho de 10 columnas.
        assert_eq!(Renderer::input_rows("", 10), 1);
        assert_eq!(Renderer::input_rows("hello", 10), 1);
        assert_eq!(Renderer::input_rows("hello\nworld", 10), 2);
        // Una línea que excede el ancho ocupa 2 filas.
        assert_eq!(Renderer::input_rows("123456789012345", 10), 2);
        // Una línea exacta al ancho necesita una fila extra para el cursor.
        assert_eq!(Renderer::input_rows("1234567890", 10), 2);
        // Clamp al máximo.
        let many = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk";
        assert_eq!(Renderer::input_rows(many, 10), MAX_INPUT_ROWS);
    }

    #[test]
    fn test_cursor_position_wrap() {
        // "abc" en ancho 10 → fila 0, col 3.
        assert_eq!(cursor_position("abc", 10), (3, 0));
        // "abc\ndef" → fila 1, col 3.
        assert_eq!(cursor_position("abc\ndef", 10), (3, 1));
        // Línea que excede el ancho: "1234567890123" (13 chars) en ancho 10
        // → fila 1, col 3.
        assert_eq!(cursor_position("1234567890123", 10), (3, 1));
        // Línea exacta al ancho: "1234567890" → fila 1, col 0.
        assert_eq!(cursor_position("1234567890", 10), (0, 1));
        // Vacío → (0, 0).
        assert_eq!(cursor_position("", 10), (0, 0));
    }

    #[test]
    fn test_cursor_position_wide_chars() {
        // Emoji ocupa 2 columnas: "abc🦜" = 5 celdas visuales.
        // En ancho 10 no wrappea: fila 0, col 5.
        assert_eq!(cursor_position("abc🦜", 10), (5, 0));
        // "🦜🦜🦜🦜🦜" = 10 celdas → llena el ancho 10 exacto → fila 1, col 0.
        assert_eq!(cursor_position("🦜🦜🦜🦜🦜", 10), (0, 1));
        // Mezcla: "🦜abc" = 2+3 = 5 celdas → col 5.
        assert_eq!(cursor_position("🦜abc", 10), (5, 0));
    }

    #[test]
    fn test_input_rows_wide_chars() {
        // "🦜🦜🦜🦜🦜" = 10 celdas → 1 fila + cursor = 2.
        assert_eq!(Renderer::input_rows("🦜🦜🦜🦜🦜", 10), 2);
        // "🦜🦜🦜🦜🦜a" = 11 celdas → 2 filas.
        assert_eq!(Renderer::input_rows("🦜🦜🦜🦜🦜a", 10), 2);
    }
}
