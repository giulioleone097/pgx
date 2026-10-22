//! TTL durations and expiry math. Pure and timezone-free: durations are whole
//! seconds, timestamps are Unix epoch seconds (UTC), and `now` is always passed
//! in so the logic is deterministic under test. Nothing here touches the clock,
//! the filesystem or the engine.
use crate::errors::{PgxError, Result};

/// A per-database time-to-live policy. The default everywhere is `Unlimited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ttl {
    /// No expiration deadline — the database is permanent.
    Unlimited,
    /// Expire this many seconds after creation (or after `pgx ttl` sets it).
    Seconds(u64),
}

/// Reject anything longer than ten years: it is almost certainly a mistake, and
/// the cap keeps `created + ttl` far from overflowing u64 epoch seconds.
pub const MAX_TTL_SECONDS: u64 = 3650 * 86_400;

impl Ttl {
    pub fn as_seconds(&self) -> Option<u64> {
        match self {
            Ttl::Unlimited => None,
            Ttl::Seconds(s) => Some(*s),
        }
    }

    /// A compact human label for the TTL column: `unlimited`, `30s`, `5m`,
    /// `1h`, `24h`, `7d`, or a composite like `1h30m` for odd values.
    pub fn label(&self) -> String {
        match self {
            Ttl::Unlimited => "unlimited".to_string(),
            Ttl::Seconds(s) => compact_duration(*s),
        }
    }

    /// The expiry instant for a database created (or re-TTL'd) at `base_epoch`.
    /// `None` for unlimited. Errors only if the (capped) arithmetic overflows,
    /// which the MAX_TTL cap makes unreachable for realistic clocks.
    pub fn expires_at(&self, base_epoch: u64) -> Result<Option<u64>> {
        match self {
            Ttl::Unlimited => Ok(None),
            Ttl::Seconds(s) => base_epoch
                .checked_add(*s)
                .map(Some)
                .ok_or_else(|| PgxError::usage("TTL expiry overflows the clock".to_string())),
        }
    }
}

/// Parse a TTL argument: `unlimited`/`none`/`never`, or `<positive int><unit>`
/// where unit is `s`, `m`, `h` or `d`. Rejects zero, signs, fractions, missing
/// or unknown units, trailing junk, and values past MAX_TTL_SECONDS. No bare
/// number (a unit is required, so `5` is an error, not "5 seconds").
pub fn parse_ttl(s: &str) -> Result<Ttl> {
    let t = s.trim();
    let bad = |why: &str| {
        PgxError::usage(format!("invalid TTL {s:?}: {why}")).hint(
            "use a positive duration like 30s, 5m, 1h, 24h or 7d, or `unlimited` to never expire",
        )
    };
    if t.is_empty() {
        return Err(bad("empty"));
    }
    if matches!(
        t.to_ascii_lowercase().as_str(),
        "unlimited" | "none" | "never"
    ) {
        return Ok(Ttl::Unlimited);
    }
    let (unit_start, _) = t.char_indices().next_back().expect("nonempty TTL");
    let (num, unit) = t.split_at(unit_start);
    let per = match unit {
        "s" => 1u64,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => {
            return Err(bad(
                "expected a trailing unit s, m, h or d (e.g. 5m), or `unlimited`",
            ))
        }
    };
    // Digits only: no sign, no fraction, no whitespace, no bare unit.
    if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad(
            "the amount before the unit must be a non-negative whole number",
        ));
    }
    let n: u64 = num
        .parse()
        .map_err(|_| bad("the amount is too large to represent"))?;
    if n == 0 {
        return Err(bad("must be greater than zero"));
    }
    let secs = n
        .checked_mul(per)
        .filter(|s| *s <= MAX_TTL_SECONDS)
        .ok_or_else(|| bad("longer than the maximum of 3650d"))?;
    Ok(Ttl::Seconds(secs))
}

/// `3m 12s`, `47m 31s`, `23h 40m`, `6d 3h`, `never`, or `0s` when already due.
/// Shows the two most-significant non-zero units.
pub fn humanize_remaining(expires_at: Option<u64>, now: u64) -> String {
    let Some(exp) = expires_at else {
        return "never".to_string();
    };
    let mut rem = exp.saturating_sub(now);
    if rem == 0 {
        return "0s".to_string();
    }
    let d = rem / 86_400;
    rem %= 86_400;
    let h = rem / 3_600;
    rem %= 3_600;
    let m = rem / 60;
    let s = rem % 60;
    let parts = [(d, "d"), (h, "h"), (m, "m"), (s, "s")];
    // Most-significant non-zero unit, plus the next unit only if it is non-zero
    // (so an exact 5 minutes reads "5m", not "5m 0s").
    let Some(first) = parts.iter().position(|(v, _)| *v != 0) else {
        return "0s".to_string();
    };
    let mut shown = vec![format!("{}{}", parts[first].0, parts[first].1)];
    if let Some((v, u)) = parts.get(first + 1) {
        if *v != 0 {
            shown.push(format!("{v}{u}"));
        }
    }
    shown.join(" ")
}

/// Compact a whole-seconds duration into a short label, preferring a single
/// unit when it divides evenly (`300` -> `5m`), else a two-unit composite.
fn compact_duration(mut secs: u64) -> String {
    for (per, unit) in [(86_400u64, "d"), (3_600, "h"), (60, "m")] {
        if secs.is_multiple_of(per) {
            return format!("{}{unit}", secs / per);
        }
    }
    // Composite: largest two units.
    let d = secs / 86_400;
    secs %= 86_400;
    let h = secs / 3_600;
    secs %= 3_600;
    let m = secs / 60;
    let s = secs % 60;
    let parts = [(d, "d"), (h, "h"), (m, "m"), (s, "s")];
    let shown: Vec<String> = parts
        .iter()
        .filter(|(v, _)| *v != 0)
        .take(2)
        .map(|(v, u)| format!("{v}{u}"))
        .collect();
    if shown.is_empty() {
        "0s".to_string()
    } else {
        shown.join("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_durations() {
        assert_eq!(parse_ttl("30s").unwrap(), Ttl::Seconds(30));
        assert_eq!(parse_ttl("5m").unwrap(), Ttl::Seconds(300));
        assert_eq!(parse_ttl("15m").unwrap(), Ttl::Seconds(900));
        assert_eq!(parse_ttl("1h").unwrap(), Ttl::Seconds(3_600));
        assert_eq!(parse_ttl("6h").unwrap(), Ttl::Seconds(21_600));
        assert_eq!(parse_ttl("24h").unwrap(), Ttl::Seconds(86_400));
        assert_eq!(parse_ttl("7d").unwrap(), Ttl::Seconds(604_800));
    }

    #[test]
    fn parses_unlimited_aliases_case_insensitively() {
        for s in ["unlimited", "UNLIMITED", "none", "Never", "never"] {
            assert_eq!(parse_ttl(s).unwrap(), Ttl::Unlimited);
        }
    }

    #[test]
    fn parses_general_positive_durations() {
        assert_eq!(parse_ttl("90s").unwrap(), Ttl::Seconds(90));
        assert_eq!(parse_ttl("2h").unwrap(), Ttl::Seconds(7_200));
        assert_eq!(parse_ttl("3d").unwrap(), Ttl::Seconds(259_200));
        assert_eq!(parse_ttl(" 10m ").unwrap(), Ttl::Seconds(600));
    }

    #[test]
    fn rejects_zero_negative_malformed_and_overflow() {
        for bad in [
            "",
            "0s",
            "0m",
            "-5m",
            "5",
            "m",
            "5x",
            "5é",
            "é",
            "5🙂",
            "1.5h",
            "1y",
            "5 m",
            "0x10s",
            "+5m",
            "99999999999999999999d",
            "5s5s",
            "abc",
            "d",
            "s",
        ] {
            assert!(parse_ttl(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_values_past_the_cap() {
        assert!(parse_ttl("3651d").is_err());
        assert_eq!(parse_ttl("3650d").unwrap(), Ttl::Seconds(MAX_TTL_SECONDS));
    }

    #[test]
    fn expiry_math_is_checked_and_unlimited_is_none() {
        assert_eq!(Ttl::Unlimited.expires_at(1000).unwrap(), None);
        assert_eq!(Ttl::Seconds(300).expires_at(1000).unwrap(), Some(1300));
        assert!(Ttl::Seconds(10).expires_at(u64::MAX).is_err());
    }

    #[test]
    fn labels_are_compact() {
        assert_eq!(Ttl::Unlimited.label(), "unlimited");
        assert_eq!(Ttl::Seconds(30).label(), "30s");
        assert_eq!(Ttl::Seconds(300).label(), "5m");
        assert_eq!(Ttl::Seconds(3_600).label(), "1h");
        // Canonical largest-unit fallback; display uses the user's original
        // string (stored in metadata), so a `--ttl 24h` still lists as "24h".
        assert_eq!(Ttl::Seconds(86_400).label(), "1d");
        assert_eq!(Ttl::Seconds(604_800).label(), "7d");
        assert_eq!(Ttl::Seconds(5_400).label(), "90m"); // divides evenly into minutes
        assert_eq!(Ttl::Seconds(5_430).label(), "1h30m"); // no single unit → composite
    }

    #[test]
    fn remaining_shows_two_units_or_never() {
        assert_eq!(humanize_remaining(None, 1000), "never");
        assert_eq!(humanize_remaining(Some(1300), 1000), "5m");
        assert_eq!(humanize_remaining(Some(1000 + 192), 1000), "3m 12s");
        assert_eq!(humanize_remaining(Some(1000 + 2851), 1000), "47m 31s");
        assert_eq!(humanize_remaining(Some(1000 + 85200), 1000), "23h 40m");
        assert_eq!(humanize_remaining(Some(1000), 1000), "0s");
        assert_eq!(humanize_remaining(Some(900), 1000), "0s"); // already due
    }
}
