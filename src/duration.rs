//! Duration parsing (spec §6: "Durations always carry units — `45s`, `20m`, `3h`").
//!
//! A duration is a positive integer followed by a unit suffix. Supported units:
//! `ms`, `s`, `m`, `h`, `d`. A bare number (no unit) is rejected, and zero/negative
//! values are rejected — the config layer requires "units and must be positive".

use crate::error::OrcrError;
use std::time::Duration;

/// Parse a duration string like `45s`, `20m`, `3h`, `500ms`, `2d`.
///
/// Returns [`OrcrError`] with code `invalid_request` (reason `bad_duration`) when the
/// string has no unit, an unknown unit, a non-numeric magnitude, or a non-positive value.
pub fn parse_duration(s: &str) -> Result<Duration, OrcrError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(bad(s, "empty duration"));
    }

    // Split the leading digits from the trailing unit.
    let split = t
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| bad(s, "duration is missing a unit (expected e.g. 45s, 20m, 3h)"))?;
    if split == 0 {
        return Err(bad(s, "duration must start with a number"));
    }
    let (num_str, unit) = t.split_at(split);
    let num: u64 = num_str
        .parse()
        .map_err(|_| bad(s, "duration magnitude is not a valid integer"))?;
    if num == 0 {
        return Err(bad(s, "duration must be positive"));
    }

    let dur = match unit {
        "ms" => Duration::from_millis(num),
        "s" => Duration::from_secs(num),
        "m" => Duration::from_secs(num.saturating_mul(60)),
        "h" => Duration::from_secs(num.saturating_mul(3600)),
        "d" => Duration::from_secs(num.saturating_mul(86_400)),
        other => {
            return Err(bad(
                s,
                format!("unknown duration unit `{other}` (use ms, s, m, h, or d)"),
            ))
        }
    };
    Ok(dur)
}

fn bad(value: &str, msg: impl Into<String>) -> OrcrError {
    use serde_json::json;
    OrcrError::new(crate::error::ErrorCode::InvalidRequest, msg.into()).with_details(json!({
        "reason": "bad_duration",
        "value": value,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("20m").unwrap(), Duration::from_secs(1200));
        assert_eq!(parse_duration("3h").unwrap(), Duration::from_secs(10800));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
    }

    #[test]
    fn rejects_missing_unit() {
        let e = parse_duration("30").unwrap_err();
        assert_eq!(e.details["reason"], "bad_duration");
    }

    #[test]
    fn rejects_unknown_unit() {
        assert!(parse_duration("5w").is_err());
        assert!(parse_duration("5min").is_err());
    }

    #[test]
    fn rejects_zero_and_nonpositive() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("0m").is_err());
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("s").is_err());
        assert!(parse_duration("").is_err());
    }
}
