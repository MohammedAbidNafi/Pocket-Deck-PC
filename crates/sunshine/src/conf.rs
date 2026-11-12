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
    fn parses_config() {
        let c = parse("# comment\ngamepad_driver = vigembus\n PORT= 50000 \nbroken line\n");
        assert_eq!(c.get("gamepad_driver").map(String::as_str), Some("vigembus"));
        assert_eq!(c.get("port").map(String::as_str), Some("50000"));
        assert_eq!(c.len(), 2);
    }
}
