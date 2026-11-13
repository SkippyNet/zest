//! Human friendly formatting helpers shared by the GUI and the CLI.

use std::time::{Duration, SystemTime};

/// Formats a byte count using binary units, e.g. `1.46 GB`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".to_owned()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Formats an integer with thousands separators, e.g. `12,345`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Formats a duration compactly, e.g. `850 ms`, `12.4 s`, `3m 05s`, `2h 10m`.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        return format!("{ms} ms");
    }
    let secs = d.as_secs();
    if secs < 60 {
        return format!("{:.1} s", d.as_secs_f64());
    }
    if secs < 3600 {
        return format!("{}m {:02}s", secs / 60, secs % 60);
    }
    format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
}

/// Describes how long ago `time` was, e.g. `3 days ago`.
pub fn format_age(time: SystemTime) -> String {
    let Ok(elapsed) = SystemTime::now().duration_since(time) else {
        return "just now".to_owned();
    };
    let secs = elapsed.as_secs();
    let (value, unit) = match secs {
        0..=59 => return "just now".to_owned(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        86_400..=2_591_999 => (secs / 86_400, "day"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

/// Formats a timestamp in local time as `2026-01-31 14:05`.
pub fn format_timestamp(time: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = time.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Returns `count noun` with a naive English plural, e.g. `1 file`, `2 files`.
pub fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{} {noun}s", format_count(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes() {
        assert_eq!(format_bytes(0), "0 bytes");
        assert_eq!(format_bytes(1), "1 byte");
        assert_eq!(format_bytes(1023), "1023 bytes");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(format_bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }

    #[test]
    fn counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(Duration::from_millis(850)), "850 ms");
        assert_eq!(format_duration(Duration::from_secs(185)), "3m 05s");
        assert_eq!(format_duration(Duration::from_secs(7800)), "2h 10m");
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "file"), "1 file");
        assert_eq!(plural(2500, "file"), "2,500 files");
    }
}
