pub mod stream;
use regex::Regex;
use std::collections::VecDeque;

pub const MAX_LINE_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const DEFAULT_MAX_LINES: usize = 100_000;

#[derive(Debug, Clone)]
pub struct LogLine {
    pub source: String,
    pub text: String,
}
impl LogLine {
    pub fn bytes(&self) -> usize {
        self.source.len() + self.text.len()
    }
}

pub fn safe_text(text: &str) -> String {
    // Terminal control characters are data, never terminal instructions.
    text.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect()
}

#[derive(Clone, Debug)]
struct FoldGroup {
    first: u64,
    last: u64,
    stack: bool,
}
/// Strip the Kubernetes timestamp only; repeated messages retain their full text in the ring.
pub fn message_body(text: &str) -> &str {
    text.split_once(' ')
        .filter(|(head, _)| {
            head.is_ascii() && head.len() >= 20 && head.as_bytes().get(10) == Some(&b'T')
        })
        .map(|(_, body)| body)
        .unwrap_or(text)
}
fn stack_frame(text: &str) -> bool {
    let body = message_body(text).trim_start();
    body.strip_prefix("at ")
        .is_some_and(|frame| frame.contains('(') && frame.ends_with(')'))
        || body
            .strip_prefix("... ")
            .and_then(|s| s.strip_suffix(" more"))
            .is_some_and(|count| count.parse::<u32>().is_ok())
}
pub struct LogBuffer {
    sources: std::collections::HashMap<String, usize>,
    folds: VecDeque<FoldGroup>,
    lines: VecDeque<LogLine>,
    matches: VecDeque<u64>,
    first: u64,
    next: u64,
    bytes: usize,
    max_bytes: usize,
    max_lines: usize,
    filter: Option<Regex>,
    matching_added: u64,
    pub dropped: u64,
}
impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
    }
}
impl LogBuffer {
    pub fn new(max_lines: usize, max_bytes: usize) -> Self {
        Self {
            sources: std::collections::HashMap::new(),
            lines: VecDeque::new(),
            folds: VecDeque::new(),
            matches: VecDeque::new(),
            first: 0,
            next: 0,
            bytes: 0,
            max_bytes,
            max_lines,
            filter: None,
            matching_added: 0,
            dropped: 0,
        }
    }
    fn accepts(&self, line: &LogLine) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|re| re.is_match(&line.text) || re.is_match(&line.source))
    }
    pub fn push(&mut self, mut line: LogLine) {
        if line.bytes() > self.max_bytes || self.max_lines == 0 {
            self.dropped += 1;
            return;
        }
        line.text = safe_text(&line.text);
        let stack = stack_frame(&line.text);
        let merge = self.lines.back().is_some_and(|previous| {
            previous.source == line.source
                && ((stack && stack_frame(&previous.text))
                    || message_body(&previous.text) == message_body(&line.text))
        });
        if merge && let Some(group) = self.folds.back_mut() {
            group.last = self.next;
        } else {
            self.folds.push_back(FoldGroup {
                first: self.next,
                last: self.next,
                stack,
            });
        }
        if self.accepts(&line) {
            self.matching_added += 1;
            self.matches.push_back(self.next);
        }
        self.next += 1;
        self.bytes += line.bytes();
        *self.sources.entry(line.source.clone()).or_default() += 1;
        self.lines.push_back(line);
        while self.lines.len() > self.max_lines || self.bytes > self.max_bytes {
            if let Some(old) = self.lines.pop_front() {
                if let Some(count) = self.sources.get_mut(&old.source) {
                    *count -= 1;
                    if *count == 0 {
                        self.sources.remove(&old.source);
                    }
                }
                self.bytes -= old.bytes();
                self.first += 1;
                self.dropped += 1;
            }
        }
        while self
            .folds
            .front()
            .is_some_and(|group| group.last < self.first)
        {
            self.folds.pop_front();
        }
        if let Some(group) = self.folds.front_mut() {
            group.first = group.first.max(self.first);
        }
        while self.matches.front().is_some_and(|seq| *seq < self.first) {
            self.matches.pop_front();
        }
    }
    pub fn filter(&mut self, value: &str) -> Result<(), regex::Error> {
        let regex = if value.is_empty() {
            None
        } else if let Some(pattern) = value.strip_prefix("re:") {
            Some(Regex::new(pattern)?)
        } else {
            Some(Regex::new(&format!("(?i){}", regex::escape(value)))?)
        };
        self.filter = regex;
        self.matches = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| self.accepts(line))
            .map(|(i, _)| self.first + i as u64)
            .collect();
        Ok(())
    }
    pub fn folded_len(&self) -> usize {
        self.folds.len()
    }
    pub fn single_source(&self) -> Option<&str> {
        (self.sources.len() == 1).then(|| self.sources.keys().next().unwrap().as_str())
    }
    pub fn folded_sequence(&self, index: usize) -> Option<u64> {
        self.folds.get(index).map(|g| g.first)
    }
    pub fn folded_position(&self, sequence: u64) -> usize {
        self.folds.partition_point(|g| g.last < sequence)
    }
    pub fn fold_summary(&self, sequence: u64) -> Option<String> {
        let group = self.folds.get(self.folded_position(sequence))?;
        let count = group.last - group.first + 1;
        (count > 1).then(|| {
            if group.stack {
                format!("↳ {} more stack frames · v expand", count - 1)
            } else {
                format!("↳ {count} repeated entries · v expand")
            }
        })
    }
    pub fn first_sequence(&self) -> u64 {
        self.first
    }
    pub fn sequence(&self, index: usize, filtered: bool) -> Option<u64> {
        if filtered {
            self.matches.get(index).copied()
        } else {
            (index < self.lines.len()).then_some(self.first + index as u64)
        }
    }
    pub fn position(&self, sequence: u64, filtered: bool) -> usize {
        if filtered {
            self.matches.partition_point(|id| *id < sequence)
        } else {
            sequence.saturating_sub(self.first) as usize
        }
    }
    pub fn line(&self, sequence: u64) -> Option<&LogLine> {
        sequence
            .checked_sub(self.first)
            .and_then(|i| self.lines.get(i as usize))
    }
    pub fn search_regex(&self) -> Option<&Regex> {
        self.filter.as_ref()
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn matching_added(&self) -> u64 {
        self.matching_added
    }
    pub fn matched(&self) -> usize {
        self.matches.len()
    }
    pub fn matching(&self) -> impl DoubleEndedIterator<Item = &LogLine> {
        self.matches
            .iter()
            .filter_map(|i| self.lines.get((*i - self.first) as usize))
    }
    pub fn visible(&self, offset: usize, height: usize) -> Vec<&LogLine> {
        let end = self.matches.len().saturating_sub(offset);
        let start = end.saturating_sub(height);
        self.matches
            .range(start..end)
            .filter_map(|i| self.lines.get((*i - self.first) as usize))
            .collect()
    }
}

/// A hostile or accidental newline-free stream cannot allocate without limit.
#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    truncated: bool,
}
impl Decoder {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = vec![];
        for &b in chunk {
            if b == b'\n' {
                out.push(self.finish());
            } else if self.bytes.len() < MAX_LINE_BYTES {
                self.bytes.push(b);
            } else {
                self.truncated = true;
            }
        }
        out
    }
    pub fn finish(&mut self) -> String {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        self.bytes.clear();
        if self.truncated {
            text.push_str(" …[line truncated]");
            self.truncated = false;
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(s: &str) -> LogLine {
        LogLine {
            source: "pod/app".into(),
            text: s.into(),
        }
    }
    #[test]
    fn wraparound_preserves_filter_indexes_and_byte_limits() {
        let mut b = LogBuffer::new(3, 100);
        b.filter("error").unwrap();
        for s in ["error 1", "ok", "error 2", "error 3"] {
            b.push(line(s));
        }
        assert_eq!(b.len(), 3);
        assert_eq!(b.dropped, 1);
        assert_eq!(
            b.matching().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["error 2", "error 3"]
        );
        b.filter("").unwrap();
        assert_eq!(b.matched(), 3);
        for _ in 0..1000 {
            b.push(line(&"x".repeat(70)));
            assert!(b.bytes() <= 100);
        }
    }
    #[test]
    fn oversized_lines_and_invalid_regex_cannot_break_the_view() {
        let mut b = LogBuffer::new(10, 100);
        b.push(line("error"));
        assert!(b.filter("re:[").is_err());
        assert_eq!(b.matched(), 1);
        b.push(line(&"x".repeat(1000)));
        assert_eq!(b.len(), 1);
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&vec![b'a'; MAX_LINE_BYTES * 4]).is_empty());
        assert!(decoder.feed(b"\n")[0].ends_with("[line truncated]"));
        assert_eq!(decoder.feed(b"next\n"), vec!["next"]);
    }
    #[test]
    fn control_sequences_cannot_change_terminal_state() {
        let mut b = LogBuffer::default();
        b.push(line("hello\u{1b}[2J\u{07}world"));
        assert!(!b.visible(0, 1)[0].text.contains('\u{1b}'));
    }
}
