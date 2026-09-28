//! Display-only structured log extraction. Raw records remain the source for search/export.
use crate::ui::theme;
use ratatui::style::{Modifier, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
    Unknown,
}
impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRC",
            Self::Debug => "DBG",
            Self::Info => "INF",
            Self::Warn => "WRN",
            Self::Error => "ERR",
            Self::Fatal => "FTL",
            Self::Unknown => " · ",
        }
    }
    pub fn style(self) -> Style {
        Style::default()
            .fg(match self {
                Self::Trace | Self::Debug | Self::Unknown => theme::MIST,
                Self::Info => theme::TEAL,
                Self::Warn => theme::AMBER,
                Self::Error | Self::Fatal => theme::CORAL,
            })
            .add_modifier(Modifier::BOLD)
    }
    fn parse(value: &str) -> Self {
        match value
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_ascii_uppercase()
            .as_str()
        {
            "TRACE" | "TRC" => Self::Trace,
            "DEBUG" | "DBG" => Self::Debug,
            "INFO" | "INFORMATION" | "INF" => Self::Info,
            "WARN" | "WARNING" | "WRN" => Self::Warn,
            "ERROR" | "ERR" | "SEVERE" => Self::Error,
            "FATAL" | "CRITICAL" | "PANIC" => Self::Fatal,
            _ => Self::Unknown,
        }
    }
}
pub fn present(body: &str, pretty: bool, message: bool) -> (String, Severity) {
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    let severity = json
        .as_ref()
        .and_then(|v| {
            v.get("level")
                .or_else(|| v.get("severity"))
                .or_else(|| v.pointer("/log/level"))
        })
        .and_then(|v| v.as_str())
        .map(Severity::parse)
        .unwrap_or_else(|| {
            // Only explicit level tokens near the start; "no errors" is not an error level.
            body.split_whitespace()
                .take(3)
                .map(|w| Severity::parse(w.strip_prefix("level=").unwrap_or(w)))
                .find(|v| *v != Severity::Unknown)
                .unwrap_or(Severity::Unknown)
        });
    let text = if pretty {
        json.as_ref()
            .and_then(|v| serde_json::to_string_pretty(v).ok())
            .unwrap_or_else(|| body.into())
    } else if message {
        json.as_ref()
            .and_then(|v| {
                ["message", "msg", "log", "body"]
                    .iter()
                    .find_map(|key| v.get(*key).and_then(|v| v.as_str()))
            })
            .filter(|s| !s.is_empty())
            .unwrap_or(body)
            .into()
    } else {
        body.into()
    };
    // JSON escapes can reintroduce terminal control bytes after ingestion sanitization.
    (
        text.chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
            .collect::<String>()
            .replace('\t', "    "),
        severity,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn message_view_preserves_unicode_and_explicit_level() {
        let raw = r#"{"level":"INFO","message":"no errors · 支払い完了","logger_name":"com.example.LongLogger","request_id":"trace-7"}"#;
        let (text, level) = present(raw, false, true);
        assert_eq!(text, "no errors · 支払い完了");
        assert_eq!(level, Severity::Info);
        assert!(present(raw, true, true).0.contains("trace-7"));
        assert_eq!(present(raw, false, false).0, raw);
    }
    #[test]
    fn decoded_controls_cannot_escape_the_terminal_and_unknown_json_is_retained() {
        assert!(
            !present(r#"{"message":"hello\u001b[2J\nnext"}"#, false, true)
                .0
                .contains('\x1b')
        );
        assert_eq!(
            present(r#"{"data":{"result":42}}"#, false, true).0,
            r#"{"data":{"result":42}}"#
        );
        assert_eq!(present("not json {", false, true).0, "not json {");
    }
}
