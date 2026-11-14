//! Version comparison the way the package catalog orders versions.
//!
//! Program versions are free text: `2.47.1`, `24.09`, `1.0.0-rc2`,
//! `17.14.13 (August 2025)`. The catalog compares them part by part, split
//! at dots: the leading number of each part decides, and whatever follows
//! the number breaks ties, with a bare number ranking above a number with a
//! suffix (so `1.0` is newer than `1.0-beta`). Trailing zero parts do not
//! count (`1.0.0` equals `1`).

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Part {
    number: u64,
    rest: String,
}

impl Part {
    fn parse(text: &str) -> Part {
        let text = text.trim();
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        match text[..digits].parse::<u64>() {
            Ok(number) => Part {
                number,
                rest: text[digits..].trim().to_lowercase(),
            },
            // No leading number.
            Err(_) if digits == 0 => Part {
                number: 0,
                rest: text.to_lowercase(),
            },
            // A digit run too long to be a number counts as text.
            Err(_) => Part {
                number: 0,
                rest: text.to_lowercase(),
            },
        }
    }

    fn is_zero(&self) -> bool {
        self.number == 0 && self.rest.is_empty()
    }
}

impl Ord for Part {
    fn cmp(&self, other: &Self) -> Ordering {
        self.number.cmp(&other.number).then_with(|| {
            match (self.rest.is_empty(), other.rest.is_empty()) {
                (true, true) => Ordering::Equal,
                // A bare number is the finished release of its pre-releases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.rest.cmp(&other.rest),
            }
        })
    }
}

impl PartialOrd for Part {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A version as the catalog orders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogVersion {
    parts: Vec<Part>,
}

impl CatalogVersion {
    /// Parses version text. Returns `None` for text that says nothing about
    /// the version: empty, "Unknown", or an upper bound such as `< 2.0`.
    pub(crate) fn parse(text: &str) -> Option<CatalogVersion> {
        let text = text.trim();
        if text.is_empty() || text.eq_ignore_ascii_case("unknown") || text.starts_with(['<', '>']) {
            return None;
        }
        // "v1.2" and "Version 1.2" are read as "1.2".
        let first_digit = text.find(|c: char| c.is_ascii_digit());
        let first_dot = text.find('.');
        let text = match (first_digit, first_dot) {
            (Some(digit), Some(dot)) if digit < dot => &text[digit..],
            (Some(digit), None) => &text[digit..],
            _ => text,
        };
        let mut parts: Vec<Part> = text.split('.').map(Part::parse).collect();
        while parts.last().is_some_and(Part::is_zero) {
            parts.pop();
        }
        Some(CatalogVersion { parts })
    }
}

impl Ord for CatalogVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        // The shorter version is padded with zero parts, so that "1.0"
        // meets "1.0-beta" part for part and wins.
        let zero = Part {
            number: 0,
            rest: String::new(),
        };
        for index in 0..self.parts.len().max(other.parts.len()) {
            let mine = self.parts.get(index).unwrap_or(&zero);
            let theirs = other.parts.get(index).unwrap_or(&zero);
            match mine.cmp(theirs) {
                Ordering::Equal => {}
                decided => return decided,
            }
        }
        Ordering::Equal
    }
}

impl PartialOrd for CatalogVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// True when `installed` is an older version than `available`. Text that
/// cannot be read as a version on either side is never "older".
pub(crate) fn is_older(installed: &str, available: &str) -> bool {
    match (
        CatalogVersion::parse(installed),
        CatalogVersion::parse(available),
    ) {
        (Some(installed), Some(available)) => installed < available,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> CatalogVersion {
        CatalogVersion::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    #[test]
    fn numbers_compare_as_numbers() {
        assert!(v("2.10") > v("2.9"));
        assert!(v("10.0") > v("9.99.99"));
        assert!(v("24.09") > v("23.01"));
        assert!(v("8.9.9.1") > v("8.8.4"));
        assert!(v("157.0.8103.79") > v("157.0.8103.78"));
        assert!(v("1.2.3") < v("1.2.3.1"));
        assert!(v("3") > v("2.99"));
    }

    #[test]
    fn trailing_zeros_and_prefixes_do_not_matter() {
        assert_eq!(v("1.0.0"), v("1"));
        assert_eq!(v("1.2.0.0"), v("1.2"));
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("Version 4.5"), v("4.5"));
        assert_eq!(v(" 1.2 "), v("1.2"));
        assert_eq!(v("0"), v("0.0"));
        assert_eq!(v("1.02"), v("1.2"));
    }

    #[test]
    fn suffixes_rank_below_the_bare_number() {
        assert!(v("1.0") > v("1.0-beta"));
        assert!(v("1.0-rc2") > v("1.0-rc1"));
        assert!(v("1.0-RC1") == v("1.0-rc1"));
        assert!(v("17.14.45") > v("17.14.13 (August 2025)"));
        assert!(v("2.0-beta") > v("1.9"));
        // Text without any number sorts by the text itself.
        assert!(v("b") > v("a"));
    }

    #[test]
    fn unreadable_versions_are_never_older() {
        assert!(CatalogVersion::parse("").is_none());
        assert!(CatalogVersion::parse("Unknown").is_none());
        assert!(CatalogVersion::parse("< 5.0.1").is_none());
        assert!(!is_older("Unknown", "9.9"));
        assert!(!is_older("", "9.9"));
        assert!(!is_older("1.0", ""));
        assert!(is_older("1.0", "1.1"));
        assert!(!is_older("1.1", "1.1"));
        assert!(!is_older("1.2", "1.1"));
        assert!(is_older(
            "99999999999999999999999.1",
            "99999999999999999999999.2"
        ));
    }
}
