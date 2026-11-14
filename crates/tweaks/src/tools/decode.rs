//! Turning the raw output of console tools into readable text while it is
//! still arriving.
//!
//! Two things make this more than a `String::from_utf8`:
//!
//! * `sfc.exe` writes UTF-16 to a pipe, most other tools write the OEM code
//!   page of the system, and a read can end in the middle of a character.
//! * Tools draw progress by sending a carriage return and writing the line
//!   again. A log pane needs the latest version of that line, not all of them.

use zest_core::process::decode_console;

/// How the bytes of one stream are encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    /// Not enough output seen yet.
    Unknown,
    /// UTF-16 little endian, as written by `sfc.exe`.
    Utf16,
    /// UTF-8 or the OEM code page.
    Narrow,
}

/// Decodes the output of one stream chunk by chunk.
#[derive(Debug)]
pub struct StreamDecoder {
    encoding: Encoding,
    /// Bytes that do not form complete characters yet.
    pending: Vec<u8>,
}

impl Default for StreamDecoder {
    fn default() -> Self {
        StreamDecoder {
            encoding: Encoding::Unknown,
            pending: Vec::new(),
        }
    }
}

/// How much undecoded narrow text may wait for a line end.
const MAX_PENDING: usize = 4096;

impl StreamDecoder {
    /// Feeds the next bytes and returns the text that is complete so far.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        if self.encoding == Encoding::Unknown {
            if self.pending.len() < 4 {
                return String::new();
            }
            self.encoding = if looks_like_utf16(&self.pending) {
                Encoding::Utf16
            } else {
                Encoding::Narrow
            };
        }
        match self.encoding {
            Encoding::Utf16 => self.take_utf16(false),
            Encoding::Narrow => self.take_narrow(false),
            Encoding::Unknown => String::new(),
        }
    }

    /// Returns whatever is left once the stream has ended.
    pub fn finish(&mut self) -> String {
        match self.encoding {
            Encoding::Utf16 => self.take_utf16(true),
            Encoding::Unknown if looks_like_utf16(&self.pending) => self.take_utf16(true),
            Encoding::Narrow | Encoding::Unknown => self.take_narrow(true),
        }
    }

    fn take_utf16(&mut self, all: bool) -> String {
        let start = if self.pending.starts_with(&[0xFF, 0xFE]) {
            2
        } else {
            0
        };
        let mut units: Vec<u16> = self.pending[start..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let mut consumed = start + units.len() * 2;
        // The second half of a surrogate pair may still be on its way.
        if !all && units.last().is_some_and(|u| (0xD800..0xDC00).contains(u)) {
            units.pop();
            consumed -= 2;
        }
        if all {
            consumed = self.pending.len();
        }
        self.pending.drain(..consumed);
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    }

    fn take_narrow(&mut self, all: bool) -> String {
        // A line end is a safe place to cut: no code page uses the bytes of
        // CR or LF as part of a longer character.
        let cut = if all || self.pending.len() > MAX_PENDING {
            self.pending.len()
        } else {
            match self
                .pending
                .iter()
                .rposition(|b| *b == b'\n' || *b == b'\r')
            {
                Some(last) => last + 1,
                None => return String::new(),
            }
        };
        let ready: Vec<u8> = self.pending.drain(..cut).collect();
        decode_console(&ready)
    }
}

/// Guesses UTF-16 from the zero high bytes that Latin text has in it.
fn looks_like_utf16(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return true;
    }
    let sample = &bytes[..bytes.len().min(256)];
    let pairs = sample.len() / 2;
    if pairs == 0 {
        return false;
    }
    let zero_high = sample.chunks_exact(2).filter(|pair| pair[1] == 0).count();
    let zero_low = sample.chunks_exact(2).filter(|pair| pair[0] == 0).count();
    // Mostly "x\0" pairs and hardly any "\0x" ones.
    zero_high * 10 >= pairs * 6 && zero_low * 10 <= pairs
}

/// The readable log of a tool: finished lines plus the line being written.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    lines: Vec<String>,
    current: String,
    /// A carriage return was seen; the next character starts the line over.
    restart_line: bool,
    /// Lines dropped from the top to keep the log bounded.
    dropped: usize,
}

/// More lines than any of the tools writes; a runaway tool cannot fill memory.
const MAX_LINES: usize = 4000;

impl Transcript {
    /// Appends decoded text, applying line ends and carriage returns.
    pub fn push_str(&mut self, text: &str) {
        for ch in text.chars() {
            match ch {
                '\n' => self.end_line(),
                '\r' => self.restart_line = true,
                // Stray NULs show up when a tool mixes encodings.
                '\0' => {}
                ch => {
                    if self.restart_line {
                        self.current.clear();
                        self.restart_line = false;
                    }
                    self.current.push(ch);
                }
            }
        }
    }

    fn end_line(&mut self) {
        self.restart_line = false;
        let line = std::mem::take(&mut self.current);
        let line = line.trim_end();
        // Tools pad their output with blank lines; one in a row is enough.
        if line.is_empty() && self.lines.last().is_none_or(|l| l.is_empty()) {
            return;
        }
        self.lines.push(line.to_owned());
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
            self.dropped += excess;
        }
    }

    /// True when nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.current.trim().is_empty()
    }

    /// Number of lines, counting the one being written.
    pub fn line_count(&self) -> usize {
        if !self.current.trim().is_empty() {
            return self.lines.len() + 1;
        }
        // A blank line at the very end is not shown.
        self.lines.len() - usize::from(self.lines.last().is_some_and(|l| l.is_empty()))
    }

    /// The whole log as text.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if self.dropped > 0 {
            out.push_str(&format!("({} earlier lines not shown)\n", self.dropped));
        }
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(self.current.trim_end());
        while out.ends_with('\n') {
            out.pop();
        }
        out
    }

    /// The progress the tool last reported, from 0 to 1, read from the
    /// newest line that carries a percentage.
    pub fn fraction(&self) -> Option<f32> {
        std::iter::once(&self.current)
            .chain(self.lines.iter().rev().take(3))
            .find_map(|line| last_percent(line))
            .map(|percent| (percent / 100.0).clamp(0.0, 1.0))
    }
}

/// The last `12%` / `12.5 %` style number in a line.
fn last_percent(line: &str) -> Option<f32> {
    let sign = line.rfind('%')?;
    let before = line[..sign].trim_end();
    let start = before
        .rfind(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .map_or(0, |i| {
            i + before[i..].chars().next().map_or(1, char::len_utf8)
        });
    let number = before[start..].replace(',', ".");
    let value: f32 = number.parse().ok()?;
    (0.0..=100.0).contains(&value).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn utf16_without_a_byte_order_mark_is_recognised() {
        // What sfc.exe writes to a pipe.
        let bytes = utf16("\r\nBeginning system scan.  This process will take some time.\r\n");
        let mut decoder = StreamDecoder::default();
        let mut text = decoder.push(&bytes);
        text.push_str(&decoder.finish());
        assert_eq!(
            text,
            "\r\nBeginning system scan.  This process will take some time.\r\n"
        );
    }

    #[test]
    fn utf16_survives_being_cut_anywhere() {
        let original = "Verification 100% complete. \u{1F34A} Überprüfung \u{65E5}\u{672C}\r\n";
        let bytes = utf16(original);
        for chunk in [1usize, 2, 3, 5, 7] {
            let mut decoder = StreamDecoder::default();
            let mut text = String::new();
            for piece in bytes.chunks(chunk) {
                text.push_str(&decoder.push(piece));
            }
            text.push_str(&decoder.finish());
            assert_eq!(text, original, "chunk size {chunk}");
        }
    }

    #[test]
    fn utf16_with_a_byte_order_mark_drops_the_mark() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(utf16("ok\r\n"));
        let mut decoder = StreamDecoder::default();
        let mut text = decoder.push(&bytes);
        text.push_str(&decoder.finish());
        assert_eq!(text, "ok\r\n");
    }

    #[test]
    fn narrow_text_is_released_line_by_line() {
        let mut decoder = StreamDecoder::default();
        assert_eq!(decoder.push(b"Deployment Image Serv"), "");
        assert_eq!(
            decoder.push(b"icing and Management tool\r\nVersion"),
            "Deployment Image Servicing and Management tool\r\n"
        );
        assert_eq!(decoder.finish(), "Version");
    }

    #[test]
    fn utf8_is_not_mangled_when_cut_inside_a_character() {
        let original = "Größe des Komponentenspeichers: 7,31 GB\r\n";
        let bytes = original.as_bytes();
        let split = original.find('ö').unwrap() + 1;
        let mut decoder = StreamDecoder::default();
        let mut text = decoder.push(&bytes[..split]);
        text.push_str(&decoder.push(&bytes[split..]));
        text.push_str(&decoder.finish());
        assert_eq!(text, original);
    }

    #[test]
    fn oem_code_page_bytes_decode_to_something_printable() {
        // 0x81 is "ü" in the western OEM code pages and never valid UTF-8 on
        // its own, so this takes the code page route. What it decodes to
        // depends on the code page of the machine running the test.
        let mut decoder = StreamDecoder::default();
        let mut text = decoder.push(b"Pr\x81fung l\x84uft\r\n");
        text.push_str(&decoder.finish());
        assert!(
            text.starts_with("Pr") && text.ends_with("ft\r\n"),
            "{text:?}"
        );
    }

    #[test]
    fn short_output_is_still_delivered_at_the_end() {
        let mut decoder = StreamDecoder::default();
        assert_eq!(decoder.push(b"ok"), "");
        assert_eq!(decoder.finish(), "ok");

        let mut decoder = StreamDecoder::default();
        assert_eq!(decoder.push(&utf16("a")), "");
        assert_eq!(decoder.finish(), "a");
    }

    #[test]
    fn a_long_line_without_an_end_does_not_wait_forever() {
        let mut decoder = StreamDecoder::default();
        let text = decoder.push(&vec![b'x'; MAX_PENDING + 10]);
        assert_eq!(text.len(), MAX_PENDING + 10);
    }

    #[test]
    fn carriage_returns_rewrite_the_progress_line() {
        let mut log = Transcript::default();
        log.push_str("Image Version: 10.0.26100.1\r\n\r\n");
        log.push_str("[==                         3.8%                           ] ");
        assert_eq!(log.fraction(), Some(0.038));
        log.push_str("\r[===========================52.0%                          ] ");
        log.push_str("\r[==========================100.0%==========================] ");
        log.push_str("\r\nThe operation completed successfully.\r\n");
        assert_eq!(
            log.text(),
            "Image Version: 10.0.26100.1\n\n[==========================100.0%==========================]\nThe operation completed successfully."
        );
    }

    #[test]
    fn blank_runs_collapse_and_trailing_space_goes() {
        let mut log = Transcript::default();
        assert!(log.is_empty());
        log.push_str("\r\n\r\nFirst   \r\n\r\n\r\n\r\nSecond\r\n\r\n");
        assert_eq!(log.text(), "First\n\nSecond");
        assert_eq!(log.line_count(), 3);
        assert!(!log.is_empty());
    }

    #[test]
    fn a_split_line_end_is_one_line_end() {
        let mut log = Transcript::default();
        log.push_str("one\r");
        log.push_str("\ntwo");
        assert_eq!(log.text(), "one\ntwo");
    }

    #[test]
    fn percentages_are_read_from_each_tool_style() {
        assert_eq!(last_percent("Verification 12% complete."), Some(12.0));
        assert_eq!(
            last_percent("[=====                      10.5%                          ]"),
            Some(10.5)
        );
        assert_eq!(
            last_percent("Progress: 5 of 100 done; Stage: 12%; Total:  3%; ETA: 0:01:10"),
            Some(3.0)
        );
        assert_eq!(last_percent("Überprüfung 45,5 % abgeschlossen"), Some(45.5));
        assert_eq!(last_percent("no number here %"), None);
        assert_eq!(last_percent("nothing"), None);
        assert_eq!(last_percent("950% is not progress"), None);
    }

    #[test]
    fn progress_falls_back_to_recent_lines_only() {
        let mut log = Transcript::default();
        log.push_str("Stage 1: 40%\r\n");
        assert_eq!(log.fraction(), Some(0.4));
        log.push_str("a\r\nb\r\nc\r\nd\r\n");
        assert_eq!(log.fraction(), None, "old progress is not current progress");
    }

    #[test]
    fn the_log_is_bounded() {
        let mut log = Transcript::default();
        for i in 0..(MAX_LINES + 25) {
            log.push_str(&format!("line {i}\n"));
        }
        assert_eq!(log.line_count(), MAX_LINES);
        assert!(
            log.text()
                .starts_with("(25 earlier lines not shown)\nline 25\n")
        );
    }
}
