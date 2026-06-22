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
            let mut spans = vec![Span::styled("  • ", Style::default().fg(Color::DarkGray))];
            spans.extend(parse_inline(content));
            lines.push(Line::from(spans));
            continue;
        }

        // Ordered list
        if let Some(rest) = parse_ordered_list(raw_line) {
            let mut spans = vec![Span::styled("  ", Style::default())];
            spans.extend(parse_inline(rest));
            lines.push(Line::from(spans));
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
        let inline_spans = parse_inline(raw_line);
        lines.push(Line::from(inline_spans));
    }

    lines
}

/// Parsea formato inline: **bold**, *italic*, `code`, [text](url).
/// Retorna `Vec<Span>` con estilo aplicado.
fn parse_inline(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut current = String::new();

    while i < len {
        // **bold**
        if i + 1 < len && chars[i] == '*' && chars[i + 1] == '*' {
            if let Some(end) = find_closing_char(&chars, i + 2, '*') {
                if !current.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut current)));
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
        // *italic*
        if chars[i] == '*' && (i + 1 >= len || chars[i + 1] != '*') {
            if let Some(end) = find_closing_char(&chars, i + 1, '*') {
                if !current.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut current)));
                }
                let italic: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(
                    italic,
                    Style::default().add_modifier(Modifier::ITALIC),
                ));
                i = end + 1;
                continue;
            }
        }
        // `code`
        if chars[i] == '`' {
            if let Some(end) = find_closing_char(&chars, i + 1, '`') {
                if !current.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut current)));
                }
                let code: String = chars[i + 1..end].iter().collect();
                spans.push(Span::styled(code, Style::default().fg(Color::Yellow)));
                i = end + 1;
                continue;
            }
        }
        // [text](url) — simple link display
        if chars[i] == '[' {
            if let Some(end_bracket) = find_closing_char(&chars, i + 1, ']') {
                if end_bracket + 1 < len && chars[end_bracket + 1] == '(' {
                    if let Some(end_paren) = find_closing_char(&chars, end_bracket + 2, ')') {
                        if !current.is_empty() {
                            spans.push(Span::raw(std::mem::take(&mut current)));
                        }
                        let link_text: String = chars[i + 1..end_bracket].iter().collect();
                        let url: String = chars[end_bracket + 2..end_paren].iter().collect();
                        spans.push(Span::styled(
                            link_text,
                            Style::default().fg(Color::Blue),
                        ));
                        spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(Color::DarkGray),
                        ));
                        i = end_paren + 1;
                        continue;
                    }
                }
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
fn find_closing_char(chars: &[char], start: usize, close: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == close)
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
