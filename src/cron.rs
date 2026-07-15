//! Cadence: five-field cron, evaluated in the creating timezone (DST-correct), with each
//! occurrence persisted as a UTC `next_fire_at`.
//!
//! We deliberately implement our own minute-stepping evaluator rather than pull a cron crate:
//! the spec's contract is precise (five fields, DST-correct evaluation in a named tz), and
//! stepping wall-clock minutes forward in the creating tz — then converting each candidate
//! local time to UTC — is the simplest thing that is correct across DST transitions ("9am
//! weekdays stays 9am"). Occurrences are computed occasionally (once per fire), so the O(days)
//! step is cheap.

use crate::error::{OrcrError, Result};
use chrono::{DateTime, Datelike, Duration as ChronoDuration, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// A parsed five-field cron expression: `minute hour day-of-month month day-of-week`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: FieldSet, // 0-59
    hour: FieldSet,   // 0-23
    dom: FieldSet,    // 1-31
    month: FieldSet,  // 1-12
    dow: FieldSet,    // 0-6 (0 = Sunday); 7 normalized to 0
    /// True if both day-of-month and day-of-week are restricted (not `*`). Standard cron
    /// semantics: when both are restricted, a match on **either** fires.
    dom_restricted: bool,
    dow_restricted: bool,
}

/// The allowed values of one cron field, as a bitset over the field's domain.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FieldSet {
    allowed: Vec<bool>,
    base: u32,
}

impl FieldSet {
    fn contains(&self, v: u32) -> bool {
        v.checked_sub(self.base)
            .and_then(|i| self.allowed.get(i as usize).copied())
            .unwrap_or(false)
    }
}

impl Cron {
    /// Parse a five-field cron expression. Whitespace-separated; each field supports `*`,
    /// a single value, `a-b` ranges, `a,b,c` lists, `*/n` steps, and `a-b/n` stepped ranges.
    pub fn parse(expr: &str) -> Result<Cron> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(OrcrError::invalid_request(
                format!(
                    "cron expression `{expr}` must have exactly 5 fields \
                     (minute hour day-of-month month day-of-week), got {}",
                    fields.len()
                ),
                "invalid_cron",
            ));
        }
        let minute = parse_field(fields[0], 0, 59, "minute")?;
        let hour = parse_field(fields[1], 0, 23, "hour")?;
        let dom = parse_field(fields[2], 1, 31, "day-of-month")?;
        let month = parse_field(fields[3], 1, 12, "month")?;
        // Day-of-week: accept 0-7 (0 and 7 both Sunday); normalize onto 0-6.
        let dow = parse_dow(fields[4])?;
        Ok(Cron {
            minute,
            hour,
            dom,
            month,
            dow,
            dom_restricted: fields[2] != "*",
            dow_restricted: fields[4] != "*",
        })
    }

    /// The next fire strictly after `after`, evaluated in timezone `tz`, returned as UTC.
    /// Returns `None` if no occurrence is found within ~4 years (an unsatisfiable expression
    /// such as Feb 30).
    pub fn next_after(&self, after: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        // Start from the minute after `after`, in local wall-clock time.
        let local = after.with_timezone(&tz);
        // Truncate to the minute, then advance by one so we never re-fire `after` itself.
        let mut cursor = local
            .with_second(0)
            .and_then(|d| d.with_nanosecond(0))
            .unwrap_or(local)
            + ChronoDuration::minutes(1);

        // Cap the search: 4 years of minutes is a safe bound for any legal expression.
        const MAX_MINUTES: i64 = 4 * 366 * 24 * 60;
        for _ in 0..MAX_MINUTES {
            let naive = cursor.naive_local();
            // chrono `num_days_from_sunday`: Sun=0..Sat=6 — exactly cron's day-of-week domain.
            let dow = naive.weekday().num_days_from_sunday();
            if self.matches(
                naive.hour(),
                naive.minute(),
                naive.day(),
                naive.month(),
                dow,
            ) {
                // Convert this local wall-clock minute to UTC. In a spring-forward gap the
                // local time does not exist → skip it; in a fall-back fold take the earliest.
                match tz.from_local_datetime(&naive) {
                    chrono::LocalResult::Single(dt) => return Some(dt.with_timezone(&Utc)),
                    chrono::LocalResult::Ambiguous(dt, _) => return Some(dt.with_timezone(&Utc)),
                    chrono::LocalResult::None => { /* nonexistent local time — skip */ }
                }
            }
            cursor += ChronoDuration::minutes(1);
        }
        None
    }

    fn matches(&self, hour: u32, minute: u32, dom: u32, month: u32, dow: u32) -> bool {
        if !self.minute.contains(minute) || !self.hour.contains(hour) || !self.month.contains(month)
        {
            return false;
        }
        // Standard day matching: if both dom and dow are restricted, fire when EITHER matches;
        // otherwise both (each being `*` or a match) must hold.
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => self.dom.contains(dom) || self.dow.contains(dow),
            _ => self.dom.contains(dom) && self.dow.contains(dow),
        }
    }
}

fn parse_dow(spec: &str) -> Result<FieldSet> {
    // Parse over 0-7 then fold 7 → 0 (both Sunday) onto a 0-6 bitset.
    let raw = parse_field(spec, 0, 7, "day-of-week")?;
    let mut allowed = vec![false; 7];
    for v in 0..=7u32 {
        if raw.contains(v) {
            allowed[(v % 7) as usize] = true;
        }
    }
    Ok(FieldSet { allowed, base: 0 })
}

fn parse_field(spec: &str, min: u32, max: u32, name: &str) -> Result<FieldSet> {
    let mut allowed = vec![false; (max - min + 1) as usize];
    let err = |msg: String| OrcrError::invalid_request(msg, "invalid_cron");
    for part in spec.split(',') {
        if part.is_empty() {
            return Err(err(format!("cron {name} field has an empty term")));
        }
        // Split off an optional step (`*/n`, `a-b/n`, `a/n`).
        let (range_part, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| err(format!("cron {name} field has an invalid step `{s}`")))?;
                if step == 0 {
                    return Err(err(format!("cron {name} field step must be > 0")));
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (lo, hi) = if range_part == "*" {
            (min, max)
        } else if let Some((a, b)) = range_part.split_once('-') {
            let a: u32 = a
                .parse()
                .map_err(|_| err(format!("cron {name} field has an invalid value `{a}`")))?;
            let b: u32 = b
                .parse()
                .map_err(|_| err(format!("cron {name} field has an invalid value `{b}`")))?;
            (a, b)
        } else {
            let v: u32 = range_part.parse().map_err(|_| {
                err(format!(
                    "cron {name} field has an invalid value `{range_part}`"
                ))
            })?;
            (v, v)
        };
        if lo < min || hi > max || lo > hi {
            return Err(err(format!(
                "cron {name} field value out of range {min}-{max}: `{part}`"
            )));
        }
        let mut v = lo;
        while v <= hi {
            allowed[(v - min) as usize] = true;
            v += step;
        }
    }
    Ok(FieldSet { allowed, base: min })
}

/// The IANA name of the host's current timezone, or `"UTC"` if it cannot be determined.
pub fn local_tz_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// Parse an IANA timezone name into a [`Tz`], defaulting to UTC on an unknown name.
pub fn tz_from_name(name: &str) -> Tz {
    name.parse::<Tz>().unwrap_or(chrono_tz::UTC)
}

/// Human-readable cadence: the cron in local words plus its UTC-offset context. Best-effort
/// prose used only for the `loop create` echo.
pub fn describe(cadence_kind: &str, cadence_value: &str, tz: &str) -> String {
    match cadence_kind {
        "once" => format!("once at {cadence_value} ({tz})"),
        _ => format!("cron `{cadence_value}` in {tz}"),
    }
}

/// Render a UTC-ms fire time as a human local+UTC timestamp for the `loop create` echo
/// (cadence in words, local + UTC). Falls back to a bare UTC render if the tz or
/// timestamp cannot be resolved.
pub fn describe_next_fire(next_fire_at: i64, tz: &str) -> String {
    let Some(utc) = Utc.timestamp_millis_opt(next_fire_at).single() else {
        return format!("{next_fire_at} (UTC ms)");
    };
    let tzn = tz_from_name(tz);
    let local = utc.with_timezone(&tzn);
    format!(
        "{} {} ({} UTC)",
        local.format("%a %Y-%m-%d %H:%M"),
        tz,
        utc.format("%a %Y-%m-%d %H:%M"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn parse_rejects_wrong_field_count() {
        assert!(Cron::parse("* * * *").is_err());
        assert!(Cron::parse("* * * * * *").is_err());
        assert!(Cron::parse("0 9 * * 1-5").is_ok());
    }

    #[test]
    fn every_30_minutes() {
        let c = Cron::parse("*/30 * * * *").unwrap();
        let tz = tz_from_name("UTC");
        let next = c.next_after(utc(2026, 1, 1, 10, 5), tz).unwrap();
        assert_eq!(next, utc(2026, 1, 1, 10, 30));
        let next2 = c.next_after(next, tz).unwrap();
        assert_eq!(next2, utc(2026, 1, 1, 11, 0));
    }

    #[test]
    fn describe_next_fire_renders_local_and_utc() {
        // 2026-03-09 13:00 UTC == 09:00 EDT (America/New_York, post spring-forward).
        let ms = utc(2026, 3, 9, 13, 0).timestamp_millis();
        let s = describe_next_fire(ms, "America/New_York");
        assert!(s.contains("2026-03-09 09:00"), "local render: {s}");
        assert!(s.contains("America/New_York"), "tz label: {s}");
        assert!(s.contains("2026-03-09 13:00 UTC"), "utc render: {s}");
        // An unresolvable timestamp still yields a stable fallback (never panics).
        assert_eq!(
            describe_next_fire(i64::MAX, "UTC"),
            "9223372036854775807 (UTC ms)"
        );
    }

    #[test]
    fn weekday_9am_skips_weekend() {
        // 2026-01-02 is a Friday; next weekday 9am after Fri 10:00 is Mon 2026-01-05 09:00.
        let c = Cron::parse("0 9 * * 1-5").unwrap();
        let tz = tz_from_name("UTC");
        let next = c.next_after(utc(2026, 1, 2, 10, 0), tz).unwrap();
        assert_eq!(next, utc(2026, 1, 5, 9, 0));
    }

    #[test]
    fn dst_spring_forward_9am_ny() {
        // US DST spring-forward 2026: Sunday March 8, 2026 (clocks 2am→3am).
        // "9am weekdays NY" — check Friday Mar 6 (EST, UTC-5 → 14:00 UTC) then the following
        // Monday Mar 9 (EDT, UTC-4 → 13:00 UTC): 9am local held, UTC offset shifted by DST.
        let c = Cron::parse("0 9 * * 1-5").unwrap();
        let tz = tz_from_name("America/New_York");
        // Fri Mar 6 09:00 EST = 14:00 UTC (pre-transition).
        let fri = c.next_after(utc(2026, 3, 6, 12, 0), tz).unwrap();
        assert_eq!(fri, utc(2026, 3, 6, 14, 0));
        // The next after Friday's fire is Monday Mar 9 09:00 EDT = 13:00 UTC (post-transition):
        // 9am local is held, the UTC offset shifted by one hour across the DST boundary.
        let mon = c.next_after(fri, tz).unwrap();
        assert_eq!(mon, utc(2026, 3, 9, 13, 0));
    }

    #[test]
    fn dst_fall_back_9am_ny() {
        // US DST fall-back 2026: Sunday November 1, 2026 (clocks 2am→1am).
        // Fri Oct 30 09:00 EDT = 13:00 UTC; next weekday Mon Nov 2 09:00 EST = 14:00 UTC.
        let c = Cron::parse("0 9 * * 1-5").unwrap();
        let tz = tz_from_name("America/New_York");
        // Fri Oct 30 09:00 EDT = 13:00 UTC (pre-transition).
        let fri = c.next_after(utc(2026, 10, 30, 12, 0), tz).unwrap();
        assert_eq!(fri, utc(2026, 10, 30, 13, 0));
        // Next weekday after fall-back: Mon Nov 2 09:00 EST = 14:00 UTC.
        let mon = c.next_after(fri, tz).unwrap();
        assert_eq!(mon, utc(2026, 11, 2, 14, 0));
    }

    #[test]
    fn list_and_range() {
        let c = Cron::parse("0 0,12 * * *").unwrap();
        let tz = tz_from_name("UTC");
        let a = c.next_after(utc(2026, 1, 1, 6, 0), tz).unwrap();
        assert_eq!(a, utc(2026, 1, 1, 12, 0));
        let b = c.next_after(a, tz).unwrap();
        assert_eq!(b, utc(2026, 1, 2, 0, 0));
    }

    #[test]
    fn day_of_month() {
        let c = Cron::parse("0 0 15 * *").unwrap();
        let tz = tz_from_name("UTC");
        let a = c.next_after(utc(2026, 1, 1, 0, 0), tz).unwrap();
        assert_eq!(a, utc(2026, 1, 15, 0, 0));
    }

    #[test]
    fn invalid_values_rejected() {
        assert!(Cron::parse("60 * * * *").is_err());
        assert!(Cron::parse("* 24 * * *").is_err());
        assert!(Cron::parse("* * 0 * *").is_err()); // dom min is 1
        assert!(Cron::parse("* * * 13 *").is_err());
        assert!(Cron::parse("* * * * 8").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
    }

    #[test]
    fn dow_seven_is_sunday() {
        let c7 = Cron::parse("0 9 * * 7").unwrap();
        let c0 = Cron::parse("0 9 * * 0").unwrap();
        let tz = tz_from_name("UTC");
        assert_eq!(
            c7.next_after(utc(2026, 1, 1, 0, 0), tz),
            c0.next_after(utc(2026, 1, 1, 0, 0), tz)
        );
    }
}
