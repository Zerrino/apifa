//! Dates: dynamic values (`{{now}}`, `{{now-7d}}`, `{{today}}`…) resolved when a request is sent,
//! and parsing / formatting for the date picker.

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, SecondsFormat, TimeDelta, TimeZone, Timelike, Utc};

/// `now`, `now-7d`, `now + 2h`, `today`, `today+1d` → RFC 3339 timestamp. Units: s, m, h, d, w.
pub fn dynamic(expr: &str) -> Option<String> {
    let e: String = expr.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let (today, rest) = match (e.strip_prefix("now"), e.strip_prefix("today")) {
        (Some(r), _) => (false, r),
        (_, Some(r)) => (true, r),
        _ => return None,
    };
    let offset = if rest.is_empty() { TimeDelta::zero() } else { parse_offset(rest)? };
    if today {
        let midnight = Local::now().date_naive().and_hms_opt(0, 0, 0)?;
        let local = Local.from_local_datetime(&midnight).earliest()?;
        Some(local.checked_add_signed(offset)?.to_rfc3339_opts(SecondsFormat::Secs, true))
    } else {
        Some(Utc::now().checked_add_signed(offset)?.to_rfc3339_opts(SecondsFormat::Secs, true))
    }
}

fn parse_offset(s: &str) -> Option<TimeDelta> {
    let sign = match s.chars().next()? {
        '+' => 1,
        '-' => -1,
        _ => return None,
    };
    let body = &s[1..];
    let unit = body.chars().last()?;
    let n: i64 = body[..body.len() - unit.len_utf8()].parse().ok()?;
    let d = match unit {
        's' => TimeDelta::try_seconds(n)?,
        'm' => TimeDelta::try_minutes(n)?,
        'h' => TimeDelta::try_hours(n)?,
        'd' => TimeDelta::try_days(n)?,
        'w' => TimeDelta::try_weeks(n)?,
        _ => return None,
    };
    Some(d * sign)
}

/// Replaces the dynamic `{{now…}}` / `{{today…}}` placeholders of a text, leaving other `{{…}}` untouched.
pub fn expand(s: &str) -> String {
    if !s.contains("{{") {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        match dynamic(&after[..end]) {
            Some(v) => out.push_str(&v),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

pub fn is_dynamic(s: &str) -> bool {
    s.contains("{{now") || s.contains("{{today") || s.contains("{{ now") || s.contains("{{ today")
}

/// Accepts RFC 3339, `YYYY-MM-DD HH:MM[:SS]` or `YYYY-MM-DD` (local time).
pub fn parse(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Local));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return Local.from_local_datetime(&n).earliest();
        }
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    Local.from_local_datetime(&d.and_hms_opt(0, 0, 0)?).earliest()
}

/// Local wall-clock time → RFC 3339 with the local offset (e.g. `2026-10-15T09:00:00+02:00`).
pub fn format_local(y: i32, m: u32, d: u32, h: u32, min: u32) -> Option<String> {
    Some(Local.with_ymd_and_hms(y, m, d, h, min, 0).earliest()?.to_rfc3339_opts(SecondsFormat::Secs, true))
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1).and_then(|d| d.pred_opt()).map(|d| d.day()).unwrap_or(31)
}

/// 0 = Monday.
pub fn first_weekday(y: i32, m: u32) -> u32 {
    NaiveDate::from_ymd_opt(y, m, 1).map(|d| d.weekday().num_days_from_monday()).unwrap_or(0)
}

pub fn now_parts() -> (i32, u32, u32, u32, u32) {
    let n = Local::now();
    (n.year(), n.month(), n.day(), n.hour(), n.minute())
}

pub fn parts(d: &DateTime<Local>) -> (i32, u32, u32, u32, u32) {
    (d.year(), d.month(), d.day(), d.hour(), d.minute())
}

pub fn local_offset() -> String {
    Local::now().format("%:z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_values() {
        let now = dynamic("now").unwrap();
        assert!(now.ends_with('Z') && now.len() == 20, "{now}");
        let week_ago = DateTime::parse_from_rfc3339(&dynamic("now - 7d").unwrap()).unwrap();
        let diff = Utc::now() - week_ago.with_timezone(&Utc);
        assert!((diff.num_seconds() - 7 * 86_400).abs() < 5);
        assert!(dynamic("today").unwrap().contains("T00:00:00"));
        assert!(dynamic("later").is_none());
        assert!(dynamic("now+3x").is_none());
        // Out of the calendar's range: no date (it used to panic).
        assert!(dynamic("now+100000000d").is_none());
        assert!(dynamic("today-100000000d").is_none());
    }

    #[test]
    fn expansion() {
        let s = expand("range[created_at]={{now-1d}},{{now}}&x={{login}}");
        assert!(!s.contains("{{now"));
        assert!(s.ends_with("&x={{login}}"));
    }

    #[test]
    fn parsing() {
        assert!(parse("2026-10-15T09:00:00Z").is_some());
        assert!(parse("2026-10-15 09:30").is_some());
        assert!(parse("2026-10-15").is_some());
        assert!(parse("tomorrow").is_none());
        assert_eq!(days_in_month(2028, 2), 29);
    }
}
