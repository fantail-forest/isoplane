//! Structured log rendering.

use serde_json::Value;

/// One structured log line as `time level target message fields`.
pub fn format_log_line(line: &str, color: bool) -> String {
    let st = crate::render::Style::new(color);
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let ts = v["timestamp"].as_str().unwrap_or("");
    let level = v["level"].as_str().unwrap_or("INFO");
    let target = v["target"].as_str().unwrap_or("");
    let msg = v["fields"]["message"].as_str().unwrap_or("");
    let level_s = match level {
        "ERROR" => st.red(level),
        "WARN" => st.yellow(level),
        "DEBUG" | "TRACE" => st.dim(level),
        _ => st.green(level),
    };
    let mut extras = Vec::new();
    if let Some(fields) = v["fields"].as_object() {
        for (k, val) in fields {
            if k != "message" {
                extras.push(format!("{k}={val}"));
            }
        }
    }
    let ts_short = ts.split('T').nth(1).unwrap_or(ts);
    format!(
        "{} {:<5} {} {}{}",
        st.dim(ts_short),
        level_s,
        st.dim(target),
        msg,
        if extras.is_empty() {
            String::new()
        } else {
            st.dim(&format!("  {}", extras.join(" ")))
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_lines_render_message_first_then_fields() {
        let line = r#"{"timestamp":"2026-08-24T10:00:00Z","level":"INFO","target":"wispd",
            "fields":{"message":"guest log","module":"quotes@v1","dispatch":7}}"#;
        let out = format_log_line(line, false);
        assert!(out.contains("guest log"), "the message is rendered: {out}");
        assert!(
            out.contains("module=\"quotes@v1\"") && out.contains("dispatch=7"),
            "the other fields follow it: {out}"
        );
        assert!(out.contains("10:00:00Z"), "the time is rendered: {out}");
    }

    #[test]
    fn a_non_json_line_passes_through_unchanged() {
        assert_eq!(
            format_log_line("plain text", false),
            "plain text",
            "a line that is not JSON is not touched"
        );
    }
}
