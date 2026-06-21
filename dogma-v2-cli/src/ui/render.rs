//! # Renderer — UI declarativa con ratatui
//!
//! Layout minimalista sin bordes:
//! ```text
//! Row 0-N-3: chat area (scrollable)
//! Row N-2:   ▎ input
//! Row N-1:   🧠 model [bar] tokens
//! ```

use std::collections::HashMap;
use std::time::Instant;

use crossterm::terminal;
use dogma_v2_core::models::events::AgentEvent;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

const CONTEXT_BAR_WIDTH: usize = 10;

pub struct Renderer {
    terminal: Option<Terminal<CrosstermBackend<std::io::Stderr>>>,
    /// Texto completo del chat (todas las respuestas concatenadas).
    chat_content: String,
    /// Texto de thinking/reasoning actual.
    thinking_text: String,
    /// Buffer de input del usuario.
    input_buffer: String,
    /// Modelo activo.
    model: String,
    /// Tokens totales.
    total_tokens: u64,
    /// Contexto usado (0.0-1.0).
    context_used: f32,
    /// Si el agente está procesando.
    busy: bool,
    /// Si hay thinking activo.
    is_thinking: bool,
    /// Scroll offset del chat (en líneas).
    scroll_offset: u16,
    /// Task tracking (para futuro uso).
    #[allow(dead_code)]
    task_lines: HashMap<String, TaskInfo>,
    /// Si ya fue inicializado.
    initialized: bool,
}

struct TaskInfo {
    #[allow(dead_code)]
    description: String,
    #[allow(dead_code)]
    spawned_at: Instant,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            terminal: None,
            chat_content: String::new(),
            thinking_text: String::new(),
            input_buffer: String::new(),
            model: "unknown".into(),
            total_tokens: 0,
            context_used: 0.0,
            busy: false,
            is_thinking: false,
            scroll_offset: 0,
            task_lines: HashMap::new(),
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

    /// Renderiza todo el UI de forma declarativa.
    fn draw(&mut self) {
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };

        // Extraer datos que necesitamos en el closure
        let chat_content = self.chat_content.clone();
        let thinking_text = self.thinking_text.clone();
        let is_thinking = self.is_thinking;
        let input_buffer = self.input_buffer.clone();
        let model = self.model.clone();
        let total_tokens = self.total_tokens;
        let context_used = self.context_used;
        let busy = self.busy;
        let scroll_offset = self.scroll_offset;

        let _ = terminal.draw(|frame| {
            let area = frame.area();
            let inner = inset(area, 2); // margen de 2 espacios

            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(3),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(inner);

            render_chat(
                frame,
                chunks[0],
                &chat_content,
                &thinking_text,
                is_thinking,
                scroll_offset,
            );
            render_separator(frame, chunks[1]);
            render_input(frame, chunks[2], &input_buffer);
            render_separator(frame, chunks[3]);
            render_metadata(frame, chunks[4], &model, total_tokens, context_used, busy);
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
        let chat_height = area.height.saturating_sub(5) as usize; // chat + 2 separators + input + metadata
        let line_count = self.chat_content.lines().count();
        if line_count > chat_height {
            self.scroll_offset = (line_count - chat_height) as u16;
        } else {
            self.scroll_offset = 0;
        }
    }

    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        self.draw();
    }

    pub fn tick(&mut self) {
        // Tick para mantener la UI viva (futuro: animaciones)
    }

    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SubAgentSpawned {
                goal_id,
                description,
                ..
            } => {
                self.task_lines.insert(
                    goal_id,
                    TaskInfo {
                        description,
                        spawned_at: Instant::now(),
                    },
                );
            }
            AgentEvent::StageChanged { .. } | AgentEvent::ToolExecuted { .. } => {}
            AgentEvent::GoalEvaluated { goal_id, .. }
            | AgentEvent::SubAgentTerminated { goal_id, .. } => {
                self.task_lines.remove(&goal_id);
            }
            AgentEvent::StatusUpdate {
                context_used,
                total_tokens,
                model,
            } => {
                self.context_used = context_used;
                self.total_tokens = total_tokens;
                if !model.is_empty() {
                    self.model = model;
                }
                self.draw();
            }
            AgentEvent::ThinkingChunk { content } => {
                self.thinking_text.push_str(&content);
                self.is_thinking = true;
                self.draw();
            }
            AgentEvent::ContentChunk { content } => {
                self.chat_content.push_str(&content);
                self.scroll_to_bottom();
                self.draw();
            }
        }
    }

    pub fn finish_response(&mut self) {
        self.thinking_text.clear();
        self.is_thinking = false;
        self.busy = false;
        self.chat_content.push_str("\n\n");
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn show_error(&mut self, msg: &str) {
        self.thinking_text.clear();
        self.is_thinking = false;
        self.busy = false;
        self.chat_content.push_str(&format!("[error] {msg}\n\n"));
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn show_input(&mut self, buffer: &str) {
        self.input_buffer = buffer.to_string();
        self.draw();
    }

    pub fn reset_output(&mut self) {
        self.chat_content.clear();
        self.thinking_text.clear();
        self.scroll_offset = 0;
        self.draw();
    }

    pub fn show_sent(&mut self, prompt: &str) {
        self.chat_content.push_str(&format!(">>> {prompt}\n\n"));
        self.scroll_to_bottom();
        self.draw();
    }

    pub fn show_queued(&self, _prompt: &str) {
        // Por implementar: mostrar prompts encolados
    }

    pub fn show_busy(&mut self) {
        self.busy = true;
        self.draw();
    }

    /// Scroll hacia arriba (para releer).
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
        self.draw();
    }

    /// Scroll hacia abajo.
    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
        self.clamp_scroll();
        self.draw();
    }

    /// Scroll al inicio.
    pub fn scroll_top(&mut self) {
        self.scroll_offset = 0;
        self.draw();
    }

    /// Scroll al final.
    pub fn scroll_bottom(&mut self) {
        self.scroll_to_bottom();
        self.draw();
    }

    fn clamp_scroll(&mut self) {
        let Some(terminal) = self.terminal.as_ref() else {
            return;
        };
        let area = terminal.size().unwrap_or(ratatui::layout::Size {
            width: 80,
            height: 24,
        });
        let chat_height = area.height.saturating_sub(3) as usize;
        let line_count = self.chat_content.lines().count();
        let max_scroll = line_count.saturating_sub(chat_height) as u16;
        self.scroll_offset = self.scroll_offset.min(max_scroll);
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

pub fn format_tokens(count: u64) -> String {
    match count {
        0..=999 => count.to_string(),
        1000..=999_999 => format!("{:.1}K", count as f64 / 1000.0),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// Insets un Rect por un margen en izquierda y derecha.
fn inset(area: Rect, margin: u16) -> Rect {
    Rect {
        x: area.x + margin,
        y: area.y,
        width: area.width.saturating_sub(margin * 2),
        height: area.height,
    }
}

// ── Funciones de renderizado (fuera de impl para evitar borrow conflicts) ──

fn render_chat(
    frame: &mut ratatui::Frame,
    area: Rect,
    chat_content: &str,
    thinking_text: &str,
    is_thinking: bool,
    scroll_offset: u16,
) {
    let mut lines: Vec<Line> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    // Thinking text (dimmed)
    if is_thinking && !thinking_text.is_empty() {
        for line in thinking_text.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(""));
    }

    // Chat content con markdown
    for line in chat_content.lines() {
        // Code blocks
        if let Some(rest) = line.strip_prefix("```") {
            if in_code_block {
                in_code_block = false;
                code_lang.clear();
                lines.push(Line::from(Span::styled(
                    "```",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                in_code_block = true;
                code_lang = rest.trim().to_string();
                let header = if code_lang.is_empty() {
                    "```".to_string()
                } else {
                    format!("```{code_lang}")
                };
                lines.push(Line::from(Span::styled(
                    header,
                    Style::default().fg(Color::DarkGray),
                )));
            }
            continue;
        }

        if in_code_block {
            // Dentro de code block: syntax highlighting básico
            let styled_line = highlight_code_line(line, &code_lang);
            lines.push(styled_line);
            continue;
        }

        // Líneas normales con markdown
        if line.starts_with(">>> ") {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                format!("### {rest}"),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                format!("## {rest}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                format!("# {rest}"),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let mut spans = vec![Span::styled("  • ", Style::default().fg(Color::DarkGray))];
            spans.extend(parse_inline_markdown(rest));
            lines.push(Line::from(spans));
        } else if let Some(rest) = line.strip_prefix("> ") {
            let mut spans = vec![Span::styled("  │ ", Style::default().fg(Color::DarkGray))];
            spans.extend(parse_inline_markdown(rest));
            let styled: Vec<Span> = spans
                .into_iter()
                .map(|s| {
                    Span::styled(
                        s.content.to_string(),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )
                })
                .collect();
            lines.push(Line::from(styled));
        } else if line.starts_with("---") || line.starts_with("***") {
            lines.push(Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(parse_inline_markdown(line)));
        }
    }

    if !lines.is_empty() {
        lines.push(Line::from(""));
    }

    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

/// Syntax highlighting básico para líneas de código.
fn highlight_code_line<'a>(line: &'a str, lang: &str) -> Line<'a> {
    let mut spans: Vec<Span> = Vec::new();

    // Keywords según el lenguaje
    let keywords: &[&str] = match lang {
        "rust" | "rs" => &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "use", "mod", "crate", "self",
            "super", "async", "await", "match", "if", "else", "for", "while", "loop", "return",
            "break", "continue", "where", "trait", "type", "const", "static", "unsafe", "extern",
        ],
        "python" | "py" => &[
            "def", "class", "import", "from", "return", "if", "elif", "else", "for", "while",
            "with", "as", "try", "except", "finally", "raise", "yield", "lambda", "pass", "break",
            "continue", "and", "or", "not", "in", "is", "True", "False", "None",
        ],
        "javascript" | "js" | "typescript" | "ts" => &[
            "function",
            "const",
            "let",
            "var",
            "return",
            "if",
            "else",
            "for",
            "while",
            "do",
            "switch",
            "case",
            "break",
            "continue",
            "new",
            "this",
            "class",
            "extends",
            "import",
            "export",
            "from",
            "default",
            "async",
            "await",
            "try",
            "catch",
            "finally",
            "throw",
            "typeof",
            "instanceof",
        ],
        "bash" | "sh" => &[
            "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac",
            "function", "return", "exit", "echo", "export", "local", "source", "eval",
        ],
        _ => &[],
    };

    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Strings
        if chars[i] == '"' || chars[i] == '\'' {
            let quote = chars[i];
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1; // skip escaped char
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1; // skip closing quote
            }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(Color::Green)));
            continue;
        }

        // Comments
        if chars[i] == '#' && lang != "python" {
            let s: String = chars[i..].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(Color::DarkGray)));
            break;
        }
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            let s: String = chars[i..].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(Color::DarkGray)));
            break;
        }

        // Numbers
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric()) {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect();
            spans.push(Span::styled(s, Style::default().fg(Color::Magenta)));
            continue;
        }

        // Words (check for keywords)
        if chars[i].is_alphabetic() || chars[i] == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if keywords.contains(&word.as_str()) {
                spans.push(Span::styled(
                    word,
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ));
            } else if word.starts_with(|c: char| c.is_uppercase()) {
                spans.push(Span::styled(word, Style::default().fg(Color::Yellow)));
            } else {
                spans.push(Span::raw(word));
            }
            continue;
        }

        // Other characters
        spans.push(Span::raw(chars[i].to_string()));
        i += 1;
    }

    if spans.is_empty() {
        Line::from(Span::raw(line.to_string()))
    } else {
        Line::from(spans)
    }
}

/// Parsea formato inline: **bold**, `code`.
fn parse_inline_markdown(text: &str) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut current = String::new();

    while i < len {
        // **bold**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing(&chars, i + 2, '*') {
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let bold: String = chars[i + 2..end].iter().collect();
                spans.push(Span::styled(
                    bold,
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                i = end + 1;
                continue;
            }
        }
        // `code`
        if chars[i] == '`' {
            if let Some(end) = find_closing(&chars, i + 1, '`') {
                if !current.is_empty() {
                    spans.push(Span::raw(current.clone()));
                    current.clear();
                }
                let code: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(code, Style::default().fg(Color::Yellow)));
                i = end + 1;
                continue;
            }
        }
        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        spans.push(Span::raw(current));
    }

    if spans.is_empty() {
        spans.push(Span::raw(text.to_string()));
    }

    spans
}

/// Busca un carácter de cierre en un slice de chars.
fn find_closing(chars: &[char], start: usize, close: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == close)
}

fn render_input(frame: &mut ratatui::Frame, area: Rect, input_buffer: &str) {
    let input_text = input_buffer.to_string();
    let paragraph = Paragraph::new(input_text);
    frame.render_widget(paragraph, area);

    let cursor_x = area.x + input_buffer.len() as u16;
    frame.set_cursor_position((cursor_x, area.y));
}

/// Línea separadora gris.
fn render_separator(frame: &mut ratatui::Frame, area: Rect) {
    let separator = "─".repeat(area.width as usize);
    let paragraph = Paragraph::new(Span::styled(
        separator,
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(paragraph, area);
}

fn render_metadata(
    frame: &mut ratatui::Frame,
    area: Rect,
    model: &str,
    total_tokens: u64,
    context_used: f32,
    busy: bool,
) {
    let ctx_pct = (context_used.clamp(0.0, 1.0) * 100.0) as usize;
    let filled = (ctx_pct * CONTEXT_BAR_WIDTH) / 100;
    let empty = CONTEXT_BAR_WIDTH.saturating_sub(filled);
    let bar: String = std::iter::repeat_n('█', filled)
        .chain(std::iter::repeat_n('░', empty))
        .collect();

    let icon = if busy { "⠋" } else { " " };

    let metadata = format!(
        "{icon} 🧠 {model}  [{bar}] {ctx_pct:3}%  🪙 {tokens}",
        tokens = format_tokens(total_tokens),
    );

    let style = Style::default().fg(Color::DarkGray);
    let paragraph = Paragraph::new(Span::styled(metadata, style));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1000), "1.0K");
        assert_eq!(format_tokens(1234), "1.2K");
        assert_eq!(format_tokens(1_000_000), "1.0M");
    }
}
