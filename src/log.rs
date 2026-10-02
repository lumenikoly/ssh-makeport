use std::collections::VecDeque;

pub const LOG_BYTES: usize = 64 * 1024;
const LOG_LINES: usize = 1200;
const MAX_LINE_BYTES: usize = 8192;

#[derive(Default, Debug)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    bytes: usize,
}

impl LogBuffer {
    pub fn push(&mut self, line: impl Into<String>) {
        let mut line = line.into();
        if line.len() > LOG_BYTES {
            let mut end = LOG_BYTES;
            while !line.is_char_boundary(end) { end -= 1; }
            line.truncate(end);
        }
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > LOG_BYTES || self.lines.len() > LOG_LINES {
            if let Some(old) = self.lines.pop_front() { self.bytes -= old.len() + 1; }
        }
    }
    pub fn text(&self) -> String { self.lines.iter().map(String::as_str).collect::<Vec<_>>().join("\n") }
    pub fn clear(&mut self) { self.lines.clear(); self.bytes = 0; }
}

// Stream decoder with a hard bound even if a process never writes a newline.
// Incomplete UTF-8 characters are kept until the next read.
#[derive(Default, Debug)]
pub struct LineDecoder { pending: Vec<u8>, after_cr: bool }

impl LineDecoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in bytes {
            if byte == b'\n' && self.after_cr { self.after_cr = false; continue; }
            self.after_cr = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                if byte == b'\n' || !self.pending.is_empty() {
                    lines.push(String::from_utf8_lossy(&self.pending).into_owned());
                    self.pending.clear();
                }
            } else {
                self.pending.push(byte);
                if self.pending.len() >= MAX_LINE_BYTES {
                    let end = match std::str::from_utf8(&self.pending) {
                        Err(error) if error.error_len().is_none() => error.valid_up_to(),
                        _ => self.pending.len(),
                    };
                    lines.push(String::from_utf8_lossy(&self.pending[..end]).into_owned());
                    self.pending.drain(..end);
                }
            }
        }
        lines
    }
    pub fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() { None } else {
            let result = String::from_utf8_lossy(&self.pending).into_owned();
            self.pending.clear();
            Some(result)
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum AnsiState { #[default] Text, Escape, Csi, String, StringEscape }

#[derive(Default, Debug)]
pub struct AnsiFilter { state: AnsiState }

impl AnsiFilter {
    // Includes OSC hyperlinks / clipboard escape sequences. Never interpret them.
    pub fn clean(&mut self, input: &str) -> String {
        let mut out = String::new();
        for ch in input.chars() {
            match self.state {
                AnsiState::Text => match ch {
                    '\u{1b}' => self.state = AnsiState::Escape,
                    '\t' => out.push(ch),
                    c if !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') => out.push(c),
                    _ => {}
                },
                AnsiState::Escape => self.state = match ch {
                    '[' => AnsiState::Csi,
                    ']' | 'P' | '^' | '_' => AnsiState::String,
                    _ => AnsiState::Text,
                },
                AnsiState::Csi => if ('@'..='~').contains(&ch) { self.state = AnsiState::Text; },
                AnsiState::String => match ch {
                    '\u{7}' => self.state = AnsiState::Text,
                    '\u{1b}' => self.state = AnsiState::StringEscape,
                    _ => {}
                },
                AnsiState::StringEscape => self.state = if ch == '\\' { AnsiState::Text } else { AnsiState::String },
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_utf8_across_reads() {
        let mut decoder = LineDecoder::default();
        assert!(decoder.feed(&[0xd0]).is_empty());
        assert_eq!(decoder.feed(&[0x90, b'\n']), vec!["А"]);
    }
    #[test]
    fn crlf_does_not_create_extra_blank_lines() {
        let mut decoder = LineDecoder::default();
        assert_eq!(decoder.feed(b"one\r"), vec!["one"]);
        assert_eq!(decoder.feed(b"\ntwo\r\n"), vec!["two"]);
    }
    #[test]
    fn giant_lines_cannot_grow_without_bound() {
        let mut decoder = LineDecoder::default();
        let lines = decoder.feed(&vec![b'a'; 100_000]);
        assert!(lines.iter().all(|s| s.len() <= MAX_LINE_BYTES));
        assert!(decoder.pending.len() < MAX_LINE_BYTES);
    }
    #[test]
    fn strips_ansi_and_osc_across_chunks() {
        let mut filter = AnsiFilter::default();
        assert_eq!(filter.clean("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(filter.clean("\x1b]52;c;secret"), "");
        assert_eq!(filter.clean("\x07text"), "text");
    }
    #[test]
    fn ring_keeps_newest_output_only() {
        let mut log = LogBuffer::default();
        for _ in 0..2000 { log.push("x".repeat(100)); }
        log.push("last");
        assert!(log.bytes <= LOG_BYTES);
        assert!(log.text().ends_with("last"));
    }
}
