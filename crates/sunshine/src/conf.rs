//! Reading `sunshine.conf` (`key = value` lines, `#` comments) and working out Sunshine's ports.

use std::collections::HashMap;
use std::path::Path;

/// Sunshine's default base port (the GameStream HTTP port).
pub const DEFAULT_PORT: u16 = 47989;

/// All the ports Sunshine listens on. Every port is a fixed offset from the base `port` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ports {
    pub http: u16,
    pub https: u16,
    pub web_ui: u16,
    pub rtsp: u16,
    pub video: u16,
    pub control: u16,
    pub audio: u16,
}

impl Ports {
    pub fn from_base(base: u16) -> Ports {
        let at = |offset: i32| (base as i32 + offset).clamp(1, u16::MAX as i32) as u16;
        Ports {
            http: base,
            https: at(-5),
            web_ui: at(1),
            rtsp: at(21),
            video: at(9),
            control: at(10),
            audio: at(11),
        }
    }
}

impl Default for Ports {
    fn default() -> Self {
        Ports::from_base(DEFAULT_PORT)
    }
}

pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
        })
        .collect()
}

/// The config text with `key` set to `value` (replacing its line, or added at the end), or with
/// its line removed when `value` is `None`. Comments, other lines and the line endings stay as
/// they were.
pub fn with_value(text: &str, key: &str, value: Option<&str>) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let is_key = |line: &str| {
        let line = line.trim();
        !line.starts_with('#') && line.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key))
    };
    let mut lines: Vec<String> = Vec::new();
    let mut done = false;
    for line in text.lines() {
        if is_key(line) {
            if let (Some(v), false) = (value, done) {
                lines.push(format!("{key} = {v}"));
            }
            done = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if let (Some(v), false) = (value, done) {
        lines.push(format!("{key} = {v}"));
    }
    let mut out = lines.join(nl);
    if !out.is_empty() {
        out.push_str(nl);
    }
    out
}

/// Ports from the config file; defaults when the file or the setting is missing.
pub fn read_ports(path: &Path) -> Ports {
    let base = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse(&text).get("port").and_then(|p| p.parse::<u16>().ok()))
        .filter(|p| *p > 1024)
        .unwrap_or(DEFAULT_PORT);
    Ports::from_base(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ports_match_sunshine() {
        let p = Ports::default();
        assert_eq!((p.https, p.http, p.web_ui, p.rtsp), (47984, 47989, 47990, 48010));
        assert_eq!((p.video, p.control, p.audio), (47998, 47999, 48000));
    }

    #[test]
    fn sets_and_removes_values() {
        let text = "# mine\r\ngamepad_driver = vigembus\r\n";
        let set = with_value(text, "output_name", Some("{abc}"));
        assert_eq!(set, "# mine\r\ngamepad_driver = vigembus\r\noutput_name = {abc}\r\n");
        let again = with_value(&set, "output_name", Some("{def}"));
        assert_eq!(parse(&again).get("output_name").map(String::as_str), Some("{def}"));
        assert_eq!(again.matches("output_name").count(), 1);
        assert_eq!(with_value(&again, "output_name", None), text);
        // A commented-out line isn't the setting.
        assert_eq!(with_value("# output_name = x\n", "output_name", Some("y")), "# output_name = x\noutput_name = y\n");
        assert_eq!(with_value("", "output_name", Some("y")), "output_name = y\n");
    }

    #[test]
    fn parses_config() {
        let c = parse("# comment\ngamepad_driver = vigembus\n PORT= 50000 \nbroken line\n");
        assert_eq!(c.get("gamepad_driver").map(String::as_str), Some("vigembus"));
        assert_eq!(c.get("port").map(String::as_str), Some("50000"));
        assert_eq!(c.len(), 2);
    }
}
