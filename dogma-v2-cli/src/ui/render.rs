//! # Renderer — Orquestador del UI
//!
//! Coordina los módulos de rendering: chat, spinner, status bar, tools.
//! Layout:
//! ```text
//! Row 0-N-5: chat area (scrollable)
//! Row N-4:   tool calls
//! Row N-3:   ▎ input
//! Row N-2:   separator
//! Row N-1:   status bar (model, context, tokens)
//! ```

use crossterm::terminal;
use dogma_v2_core::models::events::AgentEvent;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::{ChatRenderer, Spinner, StatusBar, ToolDisplay};

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
            crossterm::cursor::Show,
        );

        let backend = CrosstermBackend::new(std::io::stderr());
        let terminal = Terminal::new(backend).expect("failed to create terminal");
        self.terminal = Some(terminal);

        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);
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

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),       // chat
                    Constraint::Length(1),    // tools
                    Constraint::Length(1),    // separator
                    Constraint::Length(1),    // input
                    Constraint::Length(1),    // separator
                    Constraint::Length(1),    // status bar
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

    fn scroll_to_bottom(&mut self) {
        let Some(terminal) = self.terminal.as_ref() else {
            return;
        };
        let area = terminal.size().unwrap_or(ratatui::layout::Size {
            width: 80,
            height: 24,
        });
        let chat_height = area.height.saturating_sub(6) as usize;
        self.chat.scroll_to_bottom(chat_height);
    }

    // ── Public API (mantenida para compatibilidad con main.rs) ──────

    pub fn set_model(&mut self, model: &str) {
        self.status.set_model(model);
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
            AgentEvent::SubAgentSpawned {
                description,
                ..
            } => {
                self.tools.start(&description);
                self.draw();
            }
            AgentEvent::StageChanged { .. } => {}
            AgentEvent::ToolExecuted {
                tool_name,
                duration_ms,
                ..
            } => {
                self.tools.finish(
                    &tool_name,
                    &format!("done in {duration_ms}ms"),
                );
                self.draw();
            }
            AgentEvent::GoalEvaluated {
                completed,
                ..
            } => {
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
                self.scroll_to_bottom();
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
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn show_error(&mut self, msg: &str) {
        self.chat.show_error(msg);
        self.status.set_busy(false);
        self.scroll_to_bottom();
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
        let Some(terminal) = self.terminal.as_ref() else {
            return;
        };
        let area = terminal.size().unwrap_or(ratatui::layout::Size {
            width: 80,
            height: 24,
        });
        let chat_height = area.height.saturating_sub(6) as usize;
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
    let paragraph = Paragraph::new(input_buffer.to_string());
    frame.render_widget(paragraph, area);
    let cursor_x = area.x + input_buffer.len() as u16;
    frame.set_cursor_position((cursor_x, area.y));
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
}
