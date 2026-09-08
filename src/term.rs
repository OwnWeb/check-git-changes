use std::io::{Read, Write};
use std::ops::Range;
use std::process::{Command, Stdio};

const DEFAULT_TERMINAL_ROWS: usize = 24;
const DEFAULT_TERMINAL_COLUMNS: usize = 100;
const ESCAPE_CHAR: char = '\x1b';
const RESET: &str = "\x1b[0m";
const ESCAPE: u8 = 0x1b;
const CTRL_C: u8 = 0x03;

pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
    Act,
    Char(char),
    Interrupt,
    Unknown,
}

/// Puts the terminal in raw mode and restores the original settings on drop.
pub struct RawTerminal {
    saved_settings: String,
}

impl RawTerminal {
    pub fn acquire() -> Option<Self> {
        let saved_settings = stty(&["-g"])?.trim().to_string();
        let terminal = Self { saved_settings };
        terminal.enter_raw_mode()?;
        Some(terminal)
    }

    /// Restores cooked mode for the duration of `action`, so child processes get a
    /// normal terminal: credential prompts, pagers and progress bars all keep working.
    pub fn suspended<T>(&self, action: impl FnOnce() -> T) -> T {
        self.restore();
        let result = action();
        let _ = self.enter_raw_mode();
        result
    }

    fn enter_raw_mode(&self) -> Option<()> {
        stty(&["raw", "-echo"]).map(|_| ())
    }

    fn restore(&self) {
        let _ = stty(&[&self.saved_settings]);
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        self.restore();
    }
}

fn stty(args: &[&str]) -> Option<String> {
    let output = Command::new("stty").args(args).stdin(Stdio::inherit()).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn read_key() -> Option<Key> {
    let mut byte = [0u8; 1];
    std::io::stdin().read_exact(&mut byte).ok()?;
    Some(match byte[0] {
        CTRL_C => Key::Interrupt,
        b' ' => Key::Select,
        b'\r' | b'\n' => Key::Act,
        ESCAPE => read_arrow_key()?,
        other => Key::Char(other.to_ascii_lowercase() as char),
    })
}

fn read_arrow_key() -> Option<Key> {
    // ponytail: a bare Esc press swallows the next two bytes, q and ctrl-c are the documented exits.
    let mut sequence = [0u8; 2];
    std::io::stdin().read_exact(&mut sequence).ok()?;
    Some(match sequence {
        [b'[', b'A'] => Key::Up,
        [b'[', b'B'] => Key::Down,
        [b'[', b'C'] => Key::Right,
        [b'[', b'D'] => Key::Left,
        _ => Key::Unknown,
    })
}

pub fn terminal_size() -> (usize, usize) {
    parse_size(&stty(&["size"]).unwrap_or_default())
        .unwrap_or((DEFAULT_TERMINAL_ROWS, DEFAULT_TERMINAL_COLUMNS))
}

fn parse_size(raw: &str) -> Option<(usize, usize)> {
    let mut fields = raw.split_whitespace();
    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
}

/// Cuts a line to `max_width` visible characters, stepping over ANSI escapes so a
/// colour code is never sliced in half. A wrapped line would desynchronise the repaint.
pub fn clamp_visible(line: &str, max_width: usize) -> String {
    let mut clamped = String::new();
    let mut visible = 0;
    let mut characters = line.chars();
    let mut cut = false;
    while let Some(character) = characters.next() {
        if character == ESCAPE_CHAR {
            clamped.push(character);
            for code in characters.by_ref() {
                clamped.push(code);
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if visible == max_width {
            cut = true;
            break;
        }
        clamped.push(character);
        visible += 1;
    }
    if cut {
        clamped.push_str(RESET);
    }
    clamped
}

/// Which slice of `count` rows to show so that `cursor` stays visible.
pub fn visible_window(count: usize, cursor: usize, capacity: usize) -> Range<usize> {
    if capacity == 0 || count <= capacity {
        return 0..count;
    }
    let start = cursor.saturating_sub(capacity / 2).min(count - capacity);
    start..start + capacity
}

/// Redraws a block of lines in place, without leaving the normal screen buffer.
pub struct Screen {
    lines_drawn: usize,
}

impl Screen {
    pub fn new() -> Self {
        Self { lines_drawn: 0 }
    }

    pub fn repaint(&mut self, lines: &[String], max_width: usize) {
        let mut frame = self.rewind();
        for line in lines {
            frame.push_str(&clamp_visible(line, max_width));
            frame.push_str("\r\n");
        }
        self.lines_drawn = lines.len();
        print!("{frame}");
        let _ = std::io::stdout().flush();
    }

    pub fn clear(&mut self) {
        let frame = self.rewind();
        self.lines_drawn = 0;
        print!("{frame}");
        let _ = std::io::stdout().flush();
    }

    fn rewind(&self) -> String {
        let mut frame = String::new();
        if self.lines_drawn > 0 {
            frame.push_str(&format!("\x1b[{}A", self.lines_drawn));
        }
        frame.push_str("\x1b[J");
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_without_slicing_escape_codes() {
        assert_eq!(clamp_visible("abcdef", 3), "abc\x1b[0m");
        assert_eq!(clamp_visible("abc", 10), "abc");
        assert_eq!(clamp_visible("\x1b[33mabcdef\x1b[0m", 3), "\x1b[33mabc\x1b[0m");
        assert_eq!(clamp_visible("abc", 0), "\x1b[0m");
    }

    #[test]
    fn reads_rows_and_columns() {
        assert_eq!(parse_size("48 173\n"), Some((48, 173)));
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn keeps_the_cursor_inside_the_window() {
        assert_eq!(visible_window(3, 0, 10), 0..3);
        assert_eq!(visible_window(10, 0, 4), 0..4);
        assert_eq!(visible_window(10, 5, 4), 3..7);
        assert_eq!(visible_window(10, 9, 4), 6..10);
        assert_eq!(visible_window(10, 5, 0), 0..10);
    }
}
