//! # Input — Captura de teclado con soporte de secuencias de escape

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    /// Rueda del ratón hacia arriba.
    ScrollUp,
    /// Rueda del ratón hacia abajo.
    ScrollDown,
    Tick,
    Quit,
}

pub fn spawn_input_reader() -> mpsc::UnboundedReceiver<InputEvent> {
    let (tx, rx) = mpsc::unbounded_channel();

    let tx_tick = tx.clone();
    std::thread::Builder::new()
        .name("input-tick".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if tx_tick.send(InputEvent::Tick).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn input-tick thread");

    std::thread::Builder::new()
        .name("input-reader".into())
        .spawn(move || {
            let tty = std::fs::File::options()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .expect("failed to open /dev/tty");
            let mut reader = std::io::BufReader::new(tty);
            let mut buf = [0u8; 1];

            loop {
                match std::io::Read::read(&mut reader, &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        let byte = buf[0];
                        if byte == 0x03 {
                            let _ = tx.send(InputEvent::Quit);
                            break;
                        }
                        if byte == 0x1b {
                            handle_escape(&mut reader, &tx);
                        } else {
                            let _ = tx.send(InputEvent::Key(byte_to_keyevent(byte)));
                        }
                    }
                    Err(e) => {
                        tracing::debug!("tty read error: {e}");
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn input-reader thread");

    rx
}

/// Lee una secuencia de escape CSI y envía el KeyEvent correspondiente.
///
/// Consume TODOS los bytes de la secuencia hasta encontrar un terminador
/// (letra mayúscula, `~`, o byte fuera de rango).
fn handle_escape(
    reader: &mut std::io::BufReader<std::fs::File>,
    tx: &mpsc::UnboundedSender<InputEvent>,
) {
    let mut second = [0u8; 1];
    if std::io::Read::read(reader, &mut second).is_err() {
        let _ = tx.send(InputEvent::Key(make_key(KeyCode::Esc, KeyModifiers::NONE)));
        return;
    }

    if second[0] != b'[' {
        // ESC + otro byte → ESC + char
        let _ = tx.send(InputEvent::Key(make_key(KeyCode::Esc, KeyModifiers::NONE)));
        let _ = tx.send(InputEvent::Key(byte_to_keyevent(second[0])));
        return;
    }

    // SGR mouse: ESC [ < button;x;y M/m — rueda del ratón.
    let mut third = [0u8; 1];
    if std::io::Read::read(reader, &mut third).is_err() {
        return;
    }
    if third[0] == b'<' {
        let mut mouse_buf = vec![b'<'];
        loop {
            let mut byte = [0u8; 1];
            if std::io::Read::read(reader, &mut byte).is_err() {
                return;
            }
            mouse_buf.push(byte[0]);
            if byte[0] == b'M' || byte[0] == b'm' {
                resolve_mouse_sgr(&mouse_buf, tx);
                return;
            }
            if mouse_buf.len() > 20 {
                return;
            }
        }
    }

    // CSI sequence: ESC [ ... — leer todo hasta terminador
    let mut param_buf = vec![third[0]];
    let mut byte = [0u8; 1];
    loop {
        if std::io::Read::read(reader, &mut byte).is_err() {
            return;
        }
        let b = byte[0];
        // Terminadores CSI: letra mayúscula (A-Z) o tilde (~)
        if b.is_ascii_uppercase() || b == b'~' {
            param_buf.push(b);
            resolve_csi(&param_buf, tx);
            return;
        }
        // Si el byte es menor que 0x20 (control char) o mayor que 0x7E (~),
        // probablemente no es parte de la secuencia — ignorar
        if !(0x20..=0x7E).contains(&b) {
            return;
        }
        param_buf.push(b);
        // Límite de seguridad
        if param_buf.len() > 10 {
            return;
        }
    }
}

/// Resuelve una secuencia SGR de ratón (rueda). Formato:
/// `ESC[<button;x;yM` (presionar) o `...m` (soltar).
///
/// Botones: 64 = rueda arriba, 65 = rueda abajo. Clicks/motion se ignoran.
fn resolve_mouse_sgr(mouse_buf: &[u8], tx: &mpsc::UnboundedSender<InputEvent>) {
    // mouse_buf = "<button;x;yM"
    let mut num: u32 = 0;
    for &b in &mouse_buf[1..] {
        if b.is_ascii_digit() {
            num = num.saturating_mul(10).saturating_add(u32::from(b - b'0'));
        } else {
            break;
        }
    }
    match num {
        64 => {
            let _ = tx.send(InputEvent::ScrollUp);
        }
        65 => {
            let _ = tx.send(InputEvent::ScrollDown);
        }
        _ => {} // clicks u otros botones: ignorar
    }
}

/// Resuelve una secuencia CSI completa.
fn resolve_csi(param_buf: &[u8], tx: &mpsc::UnboundedSender<InputEvent>) {
    let key = match param_buf.last() {
        Some(b'A') => make_key(KeyCode::Up, KeyModifiers::NONE),
        Some(b'B') => make_key(KeyCode::Down, KeyModifiers::NONE),
        Some(b'C') => make_key(KeyCode::Right, KeyModifiers::NONE),
        Some(b'D') => make_key(KeyCode::Left, KeyModifiers::NONE),
        Some(b'H') => make_key(KeyCode::Home, KeyModifiers::NONE),
        Some(b'F') => make_key(KeyCode::End, KeyModifiers::NONE),
        Some(b'Z') => make_key(KeyCode::BackTab, KeyModifiers::SHIFT),
        Some(b'~') => {
            // ESC [ N ~ — soporta 1-2 dígitos
            let param_str = std::str::from_utf8(&param_buf[..param_buf.len() - 1]).unwrap_or("");
            match param_str {
                // PageUp: 5, 15, 25
                s if s.ends_with('5') => make_key(KeyCode::PageUp, KeyModifiers::NONE),
                // PageDown: 6, 16, 26
                s if s.ends_with('6') => make_key(KeyCode::PageDown, KeyModifiers::NONE),
                // Home: 1, 7, 17
                s if s == "1" || s == "7" || s == "17" => {
                    make_key(KeyCode::Home, KeyModifiers::NONE)
                }
                // End: 4, 8, 18, 24
                s if s == "4" || s == "8" || s == "18" || s == "24" => {
                    make_key(KeyCode::End, KeyModifiers::NONE)
                }
                // Delete: 3, 23
                s if s == "3" || s == "23" => make_key(KeyCode::Delete, KeyModifiers::NONE),
                // Insert: 2, 22
                s if s == "2" || s == "22" => make_key(KeyCode::Insert, KeyModifiers::NONE),
                _ => make_key(KeyCode::Char('?'), KeyModifiers::NONE),
            }
        }
        _ => make_key(KeyCode::Char('?'), KeyModifiers::NONE),
    };

    let _ = tx.send(InputEvent::Key(key));
}

fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn byte_to_keyevent(byte: u8) -> KeyEvent {
    let (code, modifiers) = match byte {
        b'\r' => (KeyCode::Enter, KeyModifiers::NONE),
        b'\n' => (KeyCode::Char('j'), KeyModifiers::CONTROL), // Ctrl+J = newline
        b'\x7f' | b'\x08' => (KeyCode::Backspace, KeyModifiers::NONE),
        b'\t' => (KeyCode::Tab, KeyModifiers::NONE),
        b if (0x20..0x7f).contains(&b) => (KeyCode::Char(b as char), KeyModifiers::NONE),
        _ => (KeyCode::Char('?'), KeyModifiers::NONE),
    };
    make_key(code, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recv(rx: &mut mpsc::UnboundedReceiver<InputEvent>) -> InputEvent {
        match rx.try_recv() {
            Ok(ev) => ev,
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[test]
    fn test_mouse_wheel_up() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        resolve_mouse_sgr(b"<64;10;5M", &tx);
        assert!(matches!(recv(&mut rx), InputEvent::ScrollUp));
    }

    #[test]
    fn test_mouse_wheel_down() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        resolve_mouse_sgr(b"<65;10;5M", &tx);
        assert!(matches!(recv(&mut rx), InputEvent::ScrollDown));
    }

    #[test]
    fn test_mouse_click_ignored() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        resolve_mouse_sgr(b"<0;10;5M", &tx);
        resolve_mouse_sgr(b"<35;10;5M", &tx);
        assert!(rx.try_recv().is_err(), "clicks must not produce events");
    }
}
