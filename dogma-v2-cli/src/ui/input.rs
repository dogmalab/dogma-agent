//! # Input — Captura de teclado con soporte de secuencias de escape

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum InputEvent {
    Key(KeyEvent),
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

    // CSI sequence: ESC [ ...
    let mut param = [0u8; 1];
    if std::io::Read::read(reader, &mut param).is_err() {
        return;
    }

    let key = match param[0] {
        b'A' => make_key(KeyCode::Up, KeyModifiers::NONE),
        b'B' => make_key(KeyCode::Down, KeyModifiers::NONE),
        b'C' => make_key(KeyCode::Right, KeyModifiers::NONE),
        b'D' => make_key(KeyCode::Left, KeyModifiers::NONE),
        b'H' => make_key(KeyCode::Home, KeyModifiers::NONE),
        b'F' => make_key(KeyCode::End, KeyModifiers::NONE),
        b'Z' => make_key(KeyCode::BackTab, KeyModifiers::SHIFT),
        b'5' => {
            let mut tilde = [0u8; 1];
            let _ = std::io::Read::read(reader, &mut tilde);
            make_key(KeyCode::PageUp, KeyModifiers::NONE)
        }
        b'6' => {
            let mut tilde = [0u8; 1];
            let _ = std::io::Read::read(reader, &mut tilde);
            make_key(KeyCode::PageDown, KeyModifiers::NONE)
        }
        b'1' => {
            let mut tilde = [0u8; 1];
            let _ = std::io::Read::read(reader, &mut tilde);
            make_key(KeyCode::Home, KeyModifiers::NONE)
        }
        b'4' => {
            let mut tilde = [0u8; 1];
            let _ = std::io::Read::read(reader, &mut tilde);
            make_key(KeyCode::End, KeyModifiers::NONE)
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
        b'\r' | b'\n' => (KeyCode::Enter, KeyModifiers::NONE),
        b'\x7f' | b'\x08' => (KeyCode::Backspace, KeyModifiers::NONE),
        b'\t' => (KeyCode::Tab, KeyModifiers::NONE),
        b if (0x20..0x7f).contains(&b) => (KeyCode::Char(b as char), KeyModifiers::NONE),
        _ => (KeyCode::Char('?'), KeyModifiers::NONE),
    };
    make_key(code, modifiers)
}
