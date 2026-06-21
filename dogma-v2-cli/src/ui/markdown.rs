//! # Markdown — Renderizado de markdown en terminal
//!
//! Soporta: headings, bold, italic, code, inline code, lists,
//! blockquotes, tablas, links, horizontal rules.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Renderiza texto markdown en líneas de terminal.
pub fn render_markdown(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();

    for raw_line in text.lines() {
        // Fenced code blocks
        if let Some(rest) = raw_line.strip_prefix("```") {
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
            lines.push(Line::from(Span::styled(
                format!("  {raw_line}"),
                Style::default().fg(Color::Green),
            )));
            continue;
        }

        // Blockquote
        if let Some(content) = raw_line.strip_prefix("> ") {
            lines.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(content.to_string(), Style::default().fg(Color::DarkGray)),
            ]));
            continue;
        }

        // Heading 1
        if let Some(content) = raw_line.strip_prefix("# ") {
            lines.push(Line::from(Span::styled(
                content.to_string(),
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Heading 2
        if let Some(content) = raw_line.strip_prefix("## ") {
            lines.push(Line::from(Span::styled(
                content.to_string(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Heading 3
        if let Some(content) = raw_line.strip_prefix("### ") {
            lines.push(Line::from(Span::styled(
                content.to_string(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )));
            continue;
        }

        // Horizontal rule
        if raw_line.starts_with("---") || raw_line.starts_with("***") {
            lines.push(Line::from(Span::styled(
                "─".repeat(60),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Unordered list
        if raw_line.starts_with("- ") || raw_line.starts_with("* ") {
            let content = &raw_line[2..];
            lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(Color::DarkGray)),
                Span::raw(parse_inline(content)),
            ]));
            continue;
        }

        // Ordered list
        if let Some(rest) = parse_ordered_list(raw_line) {
            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::raw(parse_inline(rest)),
            ]));
            continue;
        }

        // Table row (starts with |)
        if raw_line.starts_with('|') {
            if raw_line.contains("---") {
                // Separator row — skip
                continue;
            }
            let cells: Vec<&str> = raw_line.split('|').filter(|s| !s.is_empty()).collect();
            let mut spans = vec![Span::raw("  ")];
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
                }
                spans.push(Span::raw(cell.trim().to_string()));
            }
            lines.push(Line::from(spans));
            continue;
        }

        // Regular text with inline formatting
        lines.push(Line::from(Span::raw(parse_inline(raw_line))));
    }

    lines
}

/// Parsea formato inline: **bold**, *italic*, `code`, [text](url).
fn parse_inline(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next(); // consume second *
                if let Some(end) = find_closing(text, &mut chars, '*') {
                    result.push_str(&format!("[bold:{end}]"));
                } else {
                    result.push_str("**");
                }
            }
            '*' if chars.peek() != Some(&'*') => {
                if let Some(end) = find_closing(text, &mut chars, '*') {
                    result.push_str(&format!("[italic:{end}]"));
                } else {
                    result.push('*');
                }
            }
            '`' => {
                if let Some(end) = find_closing(text, &mut chars, '`') {
                    result.push_str(&format!("[code:{end}]"));
                } else {
                    result.push('`');
                }
            }
            '[' => {
                // Simple link: [text](url)
                if let Some(end_bracket) = find_closing(text, &mut chars, ']') {
                    if chars.peek() == Some(&'(') {
                        chars.next(); // consume (
                        if let Some(end_paren) = find_closing(text, &mut chars, ')') {
                            result.push_str(&format!("[link:{end_bracket}]({end_paren})"));
                            continue;
                        }
                    }
                    result.push_str(&format!("[{end_bracket}]"));
                } else {
                    result.push('[');
                }
            }
            _ => result.push(c),
        }
    }
    result
}

/// Busca el cierre de un delimitador en el texto restante.
fn find_closing(_text: &str, chars: &mut std::iter::Peekable<std::str::Chars>, delimiter: char) -> Option<String> {
    let mut buf = String::new();
    for c in chars.by_ref() {
        if c == delimiter {
            return Some(buf);
        }
        buf.push(c);
    }
    None
}

/// Detecta si una línea es una lista ordenada ("1. ", "2. ", etc.).
fn parse_ordered_list(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix(|c: char| c.is_ascii_digit()) {
        if let Some(rest) = rest.strip_prefix(". ") {
            return Some(rest);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heading() {
        let lines = render_markdown("# Title");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_code_block() {
        let lines = render_markdown("```\nfn main() {}\n```");
        assert_eq!(lines.len(), 3); // opening, code, closing
    }

    #[test]
    fn test_blockquote() {
        let lines = render_markdown("> quoted text");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_list() {
        let lines = render_markdown("- item one\n- item two");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_table() {
        let lines = render_markdown("| a | b |\n|---|---|\n| 1 | 2 |");
        // separator row is skipped
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn test_horizontal_rule() {
        let lines = render_markdown("---");
        assert_eq!(lines.len(), 1);
    }
}
