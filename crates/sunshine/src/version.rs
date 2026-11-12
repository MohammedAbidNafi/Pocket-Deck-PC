//! Comparing Sunshine version strings.
//!
//! Older releases use `0.23.1`; current ones are date based, like `2026.914.233613`.
//! Both compare correctly as dotted numbers.

fn parts(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['.', '-', '+'])
        .map_while(|p| p.parse::<u64>().ok())
        .collect()
}

/// True when `version` is older than `minimum`. Unparseable versions are never "older".
pub fn older_than(version: &str, minimum: &str) -> bool {
    let (a, b) = (parts(version), parts(minimum));
    if a.is_empty() {
        return false;
    }
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::older_than;

    #[test]
    fn compares_both_schemes() {
        assert!(older_than("0.23.1", "2025.0.0"));
        assert!(!older_than("2026.914.233613", "2025.0.0"));
        assert!(older_than("v2025.118.1", "2025.200"));
        assert!(!older_than("2025.200", "2025.200.0"));
        assert!(!older_than("unknown", "2025.0.0"));
    }
}
