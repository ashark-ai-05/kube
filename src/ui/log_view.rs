//! A visual-row log viewport. Stable sequence anchors survive incoming logs and
//! ring eviction; only visible records are laid out, with a bounded layout cache.
use crate::{
    logs::{LogBuffer, LogLine},
    ui::theme,
};
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::{collections::VecDeque, ops::Range, sync::Arc};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Anchor {
    sequence: u64,
    row: usize,
}
struct Record {
    text: String,
    prefix: String,
    gutter: usize,
    rows: Vec<Range<usize>>,
    style: Style,
    severity: crate::ui::log_format::Severity,
}

pub struct LogView {
    pub wrap: bool,
    pub fold: bool,
    pub filter_matches: bool,
    pub pretty: bool,
    pub raw: bool,
    expanded: Option<u64>,
    visible_sequences: Vec<u64>,
    pub horizontal: usize,
    pub selected_match: Option<u64>,
    anchor: Option<Anchor>,
    start: Anchor,
    width: u16,
    height: usize,
    single_pod: bool,
    cache_key: (u16, bool, bool, bool, bool, bool, Option<u64>),
    cache: VecDeque<(u64, Arc<Record>)>,
}
impl Default for LogView {
    fn default() -> Self {
        Self {
            wrap: true,
            fold: true,
            filter_matches: false,
            pretty: false,
            raw: false,
            expanded: None,
            visible_sequences: vec![],
            horizontal: 0,
            selected_match: None,
            anchor: None,
            start: Anchor::default(),
            width: 100,
            height: 20,
            single_pod: true,
            cache_key: (0, false, false, false, false, false, None),
            cache: VecDeque::new(),
        }
    }
}
impl LogView {
    pub fn reset(&mut self) {
        self.cache.clear();
        self.anchor = None;
        self.start = Anchor::default();
        self.selected_match = None;
        self.expanded = None;
        self.visible_sequences.clear();
    }
    pub fn format_label(&self, searching: bool) -> &'static str {
        if self.pretty {
            "JSON"
        } else if self.raw || searching {
            "RAW"
        } else {
            "MESSAGE"
        }
    }
    pub fn toggle_details(&mut self, row: usize) {
        if let Some(sequence) = self.visible_sequences.get(row).copied() {
            self.expanded = if self.expanded == Some(sequence) {
                None
            } else {
                Some(sequence)
            };
            self.anchor = Some(Anchor { sequence, row: 0 });
        }
    }
    pub fn following(&self) -> bool {
        self.anchor.is_none()
    }
    pub fn end(&mut self) {
        self.anchor = None;
    }
    pub fn toggle_follow(&mut self) {
        self.anchor = if self.following() {
            Some(self.start)
        } else {
            None
        };
    }
    pub fn home(&mut self, logs: &LogBuffer) {
        self.anchor = self
            .sequence(logs, 0)
            .map(|sequence| Anchor { sequence, row: 0 });
    }
    fn grouped(&self, logs: &LogBuffer) -> bool {
        self.fold && logs.search_regex().is_none() && !self.filter_matches
    }
    fn sequence(&self, logs: &LogBuffer, index: usize) -> Option<u64> {
        if self.grouped(logs) {
            logs.folded_sequence(index)
        } else {
            logs.sequence(index, self.filter_matches)
        }
    }
    fn position(&self, logs: &LogBuffer, sequence: u64) -> usize {
        if self.grouped(logs) {
            logs.folded_position(sequence)
        } else {
            logs.position(sequence, self.filter_matches)
        }
    }
    fn summary(&self, logs: &LogBuffer, sequence: u64) -> Option<String> {
        if self.grouped(logs) {
            logs.fold_summary(sequence)
        } else {
            None
        }
    }
    fn row_count(&mut self, logs: &LogBuffer, sequence: u64) -> usize {
        self.record(logs, sequence).rows.len() + usize::from(self.summary(logs, sequence).is_some())
    }
    fn count(&self, logs: &LogBuffer) -> usize {
        if self.grouped(logs) {
            logs.folded_len()
        } else if self.filter_matches {
            logs.matched()
        } else {
            logs.len()
        }
    }
    fn record(&mut self, logs: &LogBuffer, sequence: u64) -> Arc<Record> {
        let raw = self.raw || logs.search_regex().is_some();
        let single_source = logs.single_source().is_some() && !raw;
        let key = (
            self.width,
            self.wrap,
            self.pretty,
            self.single_pod,
            raw,
            single_source,
            self.expanded,
        );
        if self.cache_key != key {
            self.cache.clear();
            self.cache_key = key;
        }
        self.cache.retain(|(id, _)| *id >= logs.first_sequence());
        if let Some((_, record)) = self.cache.iter().find(|(id, _)| *id == sequence) {
            return record.clone();
        }
        let record = Arc::new(layout(
            logs.line(sequence).expect("visible sequence exists"),
            self.width,
            self.wrap,
            self.pretty || self.expanded == Some(sequence),
            self.single_pod,
            !raw,
            single_source,
        ));
        if self.cache.len() >= 128 {
            self.cache.pop_front();
        }
        self.cache.push_back((sequence, record.clone()));
        record
    }
    fn tail(&mut self, logs: &LogBuffer) -> Option<Anchor> {
        let mut remaining = self.height.max(1);
        for index in (0..self.count(logs)).rev() {
            let sequence = self.sequence(logs, index)?;
            let count = self.row_count(logs, sequence);
            if count >= remaining {
                return Some(Anchor {
                    sequence,
                    row: count - remaining,
                });
            }
            remaining -= count;
        }
        self.sequence(logs, 0)
            .map(|sequence| Anchor { sequence, row: 0 })
    }
    fn normalize(&mut self, logs: &LogBuffer, anchor: Anchor) -> Option<(usize, Anchor)> {
        let index = self
            .position(logs, anchor.sequence)
            .min(self.count(logs).checked_sub(1)?);
        let sequence = self.sequence(logs, index)?;
        let row = if sequence == anchor.sequence {
            anchor
                .row
                .min(self.row_count(logs, sequence).saturating_sub(1))
        } else {
            0
        };
        Some((index, Anchor { sequence, row }))
    }
    pub fn scroll(&mut self, logs: &LogBuffer, delta: i32) {
        let Some((mut index, mut current)) =
            self.normalize(logs, self.anchor.unwrap_or(self.start))
        else {
            return;
        };
        for _ in 0..delta.unsigned_abs().min(1000) {
            if delta < 0 {
                if current.row > 0 {
                    current.row -= 1;
                } else if index > 0 {
                    index -= 1;
                    current.sequence = self.sequence(logs, index).unwrap();
                    current.row = self.row_count(logs, current.sequence).saturating_sub(1);
                }
            } else {
                let count = self.row_count(logs, current.sequence);
                if current.row + 1 < count {
                    current.row += 1;
                } else if index + 1 < self.count(logs) {
                    index += 1;
                    current = Anchor {
                        sequence: self.sequence(logs, index).unwrap(),
                        row: 0,
                    };
                }
            }
        }
        self.anchor = Some(current);
    }
    pub fn jump_match(&mut self, logs: &LogBuffer, backwards: bool, include_current: bool) {
        if logs.matched() == 0 || logs.search_regex().is_none() {
            return;
        }
        let origin = self
            .selected_match
            .filter(|_| !include_current)
            .unwrap_or(self.start.sequence);
        let mut index = logs.position(origin, true);
        if backwards {
            index = if index == 0 {
                logs.matched() - 1
            } else {
                index - 1
            };
        } else if !include_current && logs.sequence(index, true) == Some(origin) {
            index += 1;
        }
        index %= logs.matched();
        let sequence = logs.sequence(index, true).unwrap();
        let record = self.record(logs, sequence);
        let first = logs
            .search_regex()
            .and_then(|re| re.find(&record.text))
            .map(|m| m.start())
            .unwrap_or(0);
        let row = record
            .rows
            .iter()
            .position(|r| r.contains(&first))
            .unwrap_or(0);
        self.anchor = Some(Anchor { sequence, row });
        self.selected_match = Some(sequence);
    }
    pub fn rows(
        &mut self,
        logs: &LogBuffer,
        width: u16,
        height: u16,
        single_pod: bool,
    ) -> Vec<Line<'static>> {
        self.width = width.max(1);
        self.height = height as usize;
        self.single_pod = single_pod;
        self.visible_sequences.clear();
        if height == 0 || width == 0 {
            return vec![];
        }
        let Some(anchor) = self.anchor.or_else(|| self.tail(logs)) else {
            return vec![];
        };
        let Some((mut index, current)) = self.normalize(logs, anchor) else {
            return vec![];
        };
        self.start = current;
        if self.anchor.is_some() {
            self.anchor = Some(current);
        }
        let mut row = current.row;
        let mut result = Vec::with_capacity(self.height);
        while result.len() < self.height {
            let Some(sequence) = self.sequence(logs, index) else {
                break;
            };
            let record = self.record(logs, sequence);
            let matches: Vec<Range<usize>> = logs
                .search_regex()
                .map(|re| re.find_iter(&record.text).map(|m| m.range()).collect())
                .unwrap_or_default();
            for range in record.rows.iter().skip(row) {
                if result.len() == self.height {
                    break;
                }
                let first_row = range.start == 0;
                let prefix = if first_row {
                    record.prefix.clone()
                } else {
                    format!("{}│ ", " ".repeat(record.gutter.saturating_sub(2)))
                };
                let mut spans = if first_row {
                    vec![
                        Span::styled(prefix, theme::muted_style()),
                        Span::styled(
                            format!(" {} ", record.severity.label()),
                            record.severity.style().bg(theme::SURFACE),
                        ),
                        Span::styled(" │ ", theme::muted_style()),
                    ]
                } else {
                    vec![Span::styled(prefix, theme::muted_style())]
                };
                let range = if self.wrap {
                    range.clone()
                } else {
                    horizontal_slice(
                        &record.text,
                        range.clone(),
                        self.horizontal,
                        (width as usize).saturating_sub(record.gutter),
                    )
                };
                spans.extend(highlight(&record.text, range, &matches, record.style));
                result.push(Line::from(spans));
                self.visible_sequences.push(sequence);
            }
            if result.len() < self.height
                && let Some(summary) = self.summary(logs, sequence)
            {
                result.push(Line::styled(
                    format!("{}{}", " ".repeat(record.gutter), summary),
                    theme::muted_style(),
                ));
                self.visible_sequences.push(sequence);
            }
            row = 0;
            index += 1;
        }
        result
    }
}

fn layout(
    line: &LogLine,
    width: u16,
    wrap: bool,
    pretty: bool,
    single_pod: bool,
    message: bool,
    single_source: bool,
) -> Record {
    let (time, body) = line
        .text
        .split_once(' ')
        .filter(|(head, _)| {
            head.is_ascii() && head.len() >= 20 && head.as_bytes().get(10) == Some(&b'T')
        })
        .map(|(head, body)| (&head[11..19], body))
        .unwrap_or(("        ", line.text.as_str()));
    let source = if single_pod {
        line.source.rsplit('/').next().unwrap_or(&line.source)
    } else {
        line.source
            .split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(&line.source)
    };
    let source_width = if single_source {
        0
    } else if single_pod && width >= 70 {
        10
    } else if width >= 100 {
        22
    } else if width >= 70 {
        12
    } else {
        0
    };
    let source = ellipsis(source, source_width);
    let prefix = if width < 30 {
        String::new()
    } else if source_width == 0 {
        format!("{time} ")
    } else {
        format!("{time} {source:source_width$} ")
    };
    let gutter = UnicodeWidthStr::width(prefix.as_str()) + 8;
    let (text, severity) = crate::ui::log_format::present(body, pretty, message);
    let rows = wrapped_ranges(&text, (width as usize).saturating_sub(gutter).max(1), wrap);
    let style = theme::text_style();
    Record {
        text,
        prefix,
        gutter,
        rows,
        style,
        severity,
    }
}
pub fn ellipsis(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in value.graphemes(true) {
        let n = UnicodeWidthStr::width(g);
        if used + n >= width {
            break;
        }
        out.push_str(g);
        used += n;
    }
    out.push('…');
    out
}
fn wrapped_ranges(text: &str, width: usize, wrap: bool) -> Vec<Range<usize>> {
    let mut result = vec![];
    let mut base = 0;
    for line in text.split('\n') {
        if !wrap || line.is_empty() {
            result.push(base..base + line.len());
        } else {
            let mut start = 0;
            while start < line.len() {
                let mut end = start;
                let mut used = 0;
                let mut boundary = None;
                for (offset, g) in line[start..].grapheme_indices(true) {
                    let n = UnicodeWidthStr::width(g);
                    if used + n > width && end > start {
                        break;
                    }
                    end = start + offset + g.len();
                    used += n;
                    if g.chars().all(char::is_whitespace) {
                        boundary = Some(end);
                    }
                }
                if end < line.len()
                    && let Some(word_end) = boundary
                {
                    end = word_end;
                }
                result.push(base + start..base + end);
                start = end;
            }
        }
        base += line.len() + 1;
    }
    result
}
fn horizontal_slice(text: &str, range: Range<usize>, offset: usize, width: usize) -> Range<usize> {
    let mut skipped = 0;
    let mut used = 0;
    let mut start = range.end;
    let mut end = range.end;
    for (i, g) in text[range.clone()].grapheme_indices(true) {
        let n = UnicodeWidthStr::width(g);
        if skipped < offset {
            skipped += n;
            continue;
        }
        if used + n > width {
            break;
        }
        if start == range.end {
            start = range.start + i;
        }
        end = range.start + i + g.len();
        used += n;
    }
    start..end
}
fn highlight(
    text: &str,
    range: Range<usize>,
    matches: &[Range<usize>],
    style: Style,
) -> Vec<Span<'static>> {
    if matches.is_empty() {
        return vec![Span::styled(text[range].to_string(), style)];
    }
    let mut spans = vec![];
    let mut cursor = range.start;
    for found in matches
        .iter()
        .skip_while(|m| m.end <= range.start)
        .take_while(|m| m.start < range.end)
    {
        let start = found.start.max(range.start);
        let end = found.end.min(range.end);
        if start >= end {
            continue;
        }
        if cursor < start {
            spans.push(Span::styled(text[cursor..start].to_string(), style));
        }
        spans.push(Span::styled(
            text[start..end].to_string(),
            Style::default().fg(theme::INK).bg(theme::AMBER),
        ));
        cursor = end;
    }
    if cursor < range.end {
        spans.push(Span::styled(text[cursor..range.end].to_string(), style));
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(text: &str) -> LogLine {
        LogLine {
            source: "payments/web-7fbcb78/app".into(),
            text: text.into(),
        }
    }
    fn plain(rows: &[Line<'_>]) -> String {
        rows.iter()
            .map(|r| {
                r.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn structured_messages_expand_on_demand_and_search_reveals_hidden_fields() {
        let raw = r#"2026-09-28T12:34:56Z {"level":"INFO","message":"Connected successfully","logger_name":"client.network","request_id":"trace-secret-42"}"#;
        let mut logs = LogBuffer::default();
        logs.push(line(raw));
        let mut view = LogView::default();
        let rendered = plain(&view.rows(&logs, 100, 20, true));
        assert!(rendered.contains("INF"));
        assert!(rendered.contains("Connected successfully"));
        assert!(!rendered.contains("logger_name"));
        view.toggle_details(0);
        let rendered = plain(&view.rows(&logs, 100, 20, true));
        assert!(rendered.contains("logger_name"));
        assert!(rendered.contains("trace-secret-42"));
        view.toggle_details(0);
        logs.filter("trace-secret-42").unwrap();
        let rows = view.rows(&logs, 100, 20, true);
        let highlighted: String = rows
            .iter()
            .flat_map(|r| &r.spans)
            .filter(|s| s.style.bg == Some(theme::AMBER))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(highlighted, "trace-secret-42");
        assert_eq!(logs.matching().next().unwrap().text, raw);
        logs.filter("").unwrap();
        assert!(!plain(&view.rows(&logs, 100, 20, true)).contains("logger_name"));
    }
    #[test]
    fn source_labels_return_for_mixed_containers_and_eviction_drops_old_sources() {
        let mut logs = LogBuffer::new(2, 1024);
        logs.push(line("first"));
        assert!(logs.single_source().is_some());
        logs.push(LogLine {
            source: "demo/pod/sidecar".into(),
            text: "second".into(),
        });
        assert!(logs.single_source().is_none());
        let mut view = LogView::default();
        assert!(plain(&view.rows(&logs, 100, 20, true)).contains("sidecar"));
        logs.push(LogLine {
            source: "demo/pod/sidecar".into(),
            text: "third".into(),
        });
        assert_eq!(logs.single_source(), Some("demo/pod/sidecar"));
    }
    #[test]
    fn wrapping_preserves_unicode_words_and_long_unbroken_messages() {
        for text in [
            "one two three four",
            "你好吗 👩🏽‍💻 café sample",
            "abcdefghijklmnopqrstuvwxyz0123456789",
        ] {
            let ranges = wrapped_ranges(text, 8, true);
            assert_eq!(
                ranges.iter().map(|r| &text[r.clone()]).collect::<String>(),
                text
            );
            for range in ranges {
                assert!(UnicodeWidthStr::width(&text[range]) <= 8);
            }
        }
    }
    #[test]
    fn scrolls_inside_one_long_record_and_pause_survives_arrivals() {
        let mut logs = LogBuffer::new(3, 10000);
        logs.push(line(&"word ".repeat(200)));
        let mut view = LogView::default();
        view.rows(&logs, 50, 5, true);
        view.home(&logs);
        view.rows(&logs, 50, 5, true);
        view.scroll(&logs, 8);
        assert_eq!(view.anchor.unwrap().row, 8);
        let before = plain(&view.rows(&logs, 50, 5, true));
        logs.push(line("new line"));
        assert_eq!(plain(&view.rows(&logs, 50, 5, true)), before);
        for _ in 0..4 {
            logs.push(line("replacement"));
        }
        assert!(plain(&view.rows(&logs, 50, 5, true)).contains("replacement"));
    }
    #[test]
    fn search_preserves_context_highlights_and_moves_between_matches() {
        let mut logs = LogBuffer::default();
        for text in ["before", "error one", "between", "error two", "after"] {
            logs.push(line(text));
        }
        logs.filter("error").unwrap();
        let mut view = LogView::default();
        let rows = view.rows(&logs, 80, 12, true);
        assert!(plain(&rows).contains("between"));
        assert!(
            rows.iter()
                .flat_map(|r| &r.spans)
                .any(|s| s.style.bg == Some(theme::AMBER))
        );
        view.jump_match(&logs, false, true);
        assert_eq!(view.selected_match, Some(1));
        view.jump_match(&logs, false, false);
        assert_eq!(view.selected_match, Some(3));
        view.jump_match(&logs, true, false);
        assert_eq!(view.selected_match, Some(1));
        view.filter_matches = true;
        assert!(!plain(&view.rows(&logs, 80, 12, true)).contains("between"));
    }
    #[test]
    fn metadata_is_compact_and_horizontal_scroll_reveals_hidden_tail() {
        let mut logs = LogBuffer::default();
        logs.push(line(
            "2026-09-28T12:34:56.000Z abcdefghijklmnopqrstuvwxyz tail-token",
        ));
        let mut view = LogView {
            wrap: false,
            ..Default::default()
        };
        let start = plain(&view.rows(&logs, 35, 5, true));
        assert!(!start.contains("payments/web"));
        assert!(start.contains("12:34:56"));
        view.horizontal = 20;
        assert!(plain(&view.rows(&logs, 35, 5, true)).contains("tail-token"));
    }
    #[test]
    fn folding_preserves_raw_records_and_search_expands_hidden_frames() {
        let mut logs = LogBuffer::default();
        for text in [
            "error: timeout",
            " at first.fn(Main.java:1)",
            " at hidden.fn(Main.java:2)",
            " at last.fn(Main.java:3)",
            "retry",
            "retry",
        ] {
            logs.push(line(text));
        }
        let mut view = LogView::default();
        let folded = plain(&view.rows(&logs, 100, 20, true));
        assert!(folded.contains("2 more stack frames"));
        assert!(folded.contains("2 repeated entries"));
        assert!(!folded.contains("hidden.fn"));
        assert_eq!(logs.matching().count(), 6);
        logs.filter("hidden.fn").unwrap();
        view.jump_match(&logs, false, true);
        assert!(plain(&view.rows(&logs, 100, 20, true)).contains("hidden.fn"));
        logs.filter("").unwrap();
        view.fold = false;
        view.home(&logs);
        assert!(plain(&view.rows(&logs, 100, 20, true)).contains("last.fn"));
    }
    #[test]
    fn folded_groups_survive_ring_eviction_and_respect_container_boundaries() {
        let mut logs = LogBuffer::new(3, 1024);
        for n in 0..20 {
            logs.push(line(&format!("2026-09-28T12:34:{n:02}Z retry")));
        }
        assert_eq!(logs.folded_len(), 1);
        assert_eq!(logs.folded_sequence(0), Some(17));
        assert!(logs.fold_summary(17).unwrap().contains("3 repeated"));
        let mut view = LogView::default();
        view.home(&logs);
        view.scroll(&logs, 1);
        assert!(plain(&view.rows(&logs, 100, 1, true)).contains("3 repeated"));
        logs.push(LogLine {
            source: "another/app".into(),
            text: "retry".into(),
        });
        assert_eq!(logs.folded_len(), 2);
        assert!(!plain(&view.rows(&logs, 100, 20, true)).is_empty());
    }
}
