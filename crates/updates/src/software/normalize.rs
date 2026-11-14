//! Name and publisher normalisation compatible with the public package
//! catalog.
//!
//! The catalog does not list programs under the names Windows shows for
//! them. It stores a *normalised* form of every known display name and
//! publisher: version numbers, processor architecture, language tags,
//! bracketed remarks, company suffixes ("Inc", "GmbH") and punctuation are
//! taken out, and what remains is joined and lower-cased. "Mozilla Firefox
//! 131.0 (x64 en-US)" by "Mozilla Corporation" becomes `mozillafirefox` by
//! `mozilla`.
//!
//! To find an installed program in the catalog its name has to go through
//! exactly the same steps, so this module follows the rules the Windows
//! Package Manager publishes in its source code step by step. Where a rule
//! exists only for one vendor's unusual naming it is left out; that can only
//! make a match be missed, never a wrong one be made.

use windows::Win32::Globalization::{NormalizationKC, NormalizeString};

/// Processor architecture named in a display name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arch {
    X86,
    X64,
    Arm,
    Arm64,
}

impl Arch {
    /// The spelling the catalog appends to a name, as in `7zip(X64)`.
    fn tag(self) -> &'static str {
        match self {
            Arch::X86 => "X86",
            Arch::X64 => "X64",
            Arch::Arm => "Arm",
            Arch::Arm64 => "Arm64",
        }
    }
}

/// A normalised program name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedName {
    /// The name without architecture.
    pub name: String,
    /// The architecture that was named, if any.
    pub arch: Option<Arch>,
}

impl NormalizedName {
    /// The name with its architecture appended, when one was named.
    pub(crate) fn with_arch(&self) -> Option<String> {
        self.arch
            .map(|arch| format!("{}({})", self.name, arch.tag()))
    }
}

// ── Character classes ────────────────────────────────────────────────

fn is_letter(c: char) -> bool {
    c.is_alphabetic()
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || (!c.is_ascii() && c.is_numeric())
}

fn is_alnum(c: char) -> bool {
    is_letter(c) || is_digit(c)
}

/// Dash punctuation.
fn is_dash(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{58A}' | '\u{5BE}' | '\u{2010}'
            ..='\u{2015}'
                | '\u{2E3A}'
                | '\u{2E3B}'
                | '\u{301C}'
                | '\u{3030}'
                | '\u{FE58}'
                | '\u{FE63}'
                | '\u{FF0D}'
    )
}

/// Connector punctuation.
fn is_connector(c: char) -> bool {
    matches!(c, '_' | '\u{203F}' | '\u{2040}' | '\u{2054}' | '\u{FF3F}')
}

/// "Other" punctuation: what is neither a bracket, a dash, a quote mark of
/// the curly kind nor a connector.
fn is_other_punct(c: char) -> bool {
    matches!(
        c,
        '!' | '"' | '#' | '%' | '&' | '\'' | '*' | ',' | '.' | '/' | ':' | ';' | '?' | '@' | '\\'
            | '\u{A1}' | '\u{A7}' | '\u{B6}' | '\u{B7}' | '\u{BF}' | '\u{2016}' | '\u{2017}'
            | '\u{2020}'..='\u{2027}' | '\u{2030}'..='\u{2038}' | '\u{203B}'..='\u{203E}'
            | '\u{3001}'..='\u{3003}' | '\u{30FB}' | '\u{FF01}'..='\u{FF03}'
            | '\u{FF05}'..='\u{FF07}' | '\u{FF0A}' | '\u{FF0C}' | '\u{FF0E}' | '\u{FF0F}'
            | '\u{FF1A}' | '\u{FF1B}' | '\u{FF1F}' | '\u{FF20}' | '\u{FF3C}'
    )
}

/// A character that can join the parts of a version number.
fn is_joiner(c: char) -> bool {
    is_other_punct(c) || is_dash(c) || is_connector(c)
}

fn is_open_bracket(c: char) -> bool {
    matches!(
        c,
        '(' | '['
            | '{'
            | '\u{2045}'
            | '\u{207D}'
            | '\u{208D}'
            | '\u{2329}'
            | '\u{3008}'
            | '\u{300A}'
            | '\u{300C}'
            | '\u{300E}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{FF08}'
            | '\u{FF3B}'
            | '\u{FF5B}'
    )
}

fn is_close_bracket(c: char) -> bool {
    matches!(
        c,
        ')' | ']'
            | '}'
            | '\u{2046}'
            | '\u{207E}'
            | '\u{208E}'
            | '\u{232A}'
            | '\u{3009}'
            | '\u{300B}'
            | '\u{300D}'
            | '\u{300F}'
            | '\u{3011}'
            | '\u{3015}'
            | '\u{3017}'
            | '\u{FF09}'
            | '\u{FF3D}'
            | '\u{FF5D}'
    )
}

// ── Matching helpers ─────────────────────────────────────────────────

fn at(text: &[char], index: usize) -> Option<char> {
    text.get(index).copied()
}

/// Length of `word` if `text` continues with it at `index`. The text has
/// been lower-cased before, so `word` is always given in lower case.
fn lit(text: &[char], index: usize, word: &str) -> Option<usize> {
    let mut len = 0;
    for expected in word.chars() {
        if at(text, index + len)? != expected {
            return None;
        }
        len += 1;
    }
    Some(len)
}

fn lit_any(text: &[char], index: usize, words: &[&str]) -> Option<usize> {
    words.iter().find_map(|word| lit(text, index, word))
}

/// Number of consecutive characters from `index` that satisfy `test`.
fn run(text: &[char], index: usize, test: impl Fn(char) -> bool) -> usize {
    text[index.min(text.len())..]
        .iter()
        .take_while(|c| test(**c))
        .count()
}

/// Removes every non-overlapping match, scanning from the left.
/// `matcher(text, index)` returns the length of a match starting at `index`.
fn remove_all(text: &mut Vec<char>, matcher: impl Fn(&[char], usize) -> Option<usize>) -> bool {
    let mut out = Vec::with_capacity(text.len());
    let mut index = 0;
    let mut changed = false;
    while index < text.len() {
        match matcher(text, index) {
            Some(len) if len > 0 => {
                index += len;
                changed = true;
            }
            _ => {
                out.push(text[index]);
                index += 1;
            }
        }
    }
    *text = out;
    changed
}

// ── Architecture ─────────────────────────────────────────────────────

/// True at the start of the text or after anything but a letter or digit.
fn after_boundary(text: &[char], index: usize) -> bool {
    index == 0 || !is_alnum(text[index - 1])
}

/// Length of an optional " edition" that follows an architecture.
fn edition(text: &[char], index: usize) -> usize {
    match at(text, index) {
        Some(c) if c.is_whitespace() => lit(text, index + 1, "edition").map_or(0, |n| n + 1),
        _ => 0,
    }
}

/// A named architecture such as `x64`, not followed by a further digit.
fn arch_word(text: &[char], index: usize, words: &[&str]) -> Option<usize> {
    if !after_boundary(text, index) {
        return None;
    }
    let len = lit_any(text, index, words)?;
    if at(text, index + len).is_some_and(is_digit) {
        return None;
    }
    Some(len + edition(text, index + len))
}

fn match_x64(text: &[char], index: usize) -> Option<usize> {
    arch_word(text, index, &["x64", "amd64"]).or_else(|| {
        // x86-64, x86_64
        if !after_boundary(text, index) {
            return None;
        }
        let x86 = lit(text, index, "x86")?;
        let joiner = at(text, index + x86).filter(|c| is_dash(*c) || is_connector(*c))?;
        let _ = joiner;
        let len = x86 + 1 + lit(text, index + x86 + 1, "64")?;
        if at(text, index + len).is_some_and(is_digit) {
            return None;
        }
        Some(len + edition(text, index + len))
    })
}

fn match_x86(text: &[char], index: usize) -> Option<usize> {
    arch_word(text, index, &["x32", "x86"])
}

fn match_arm64(text: &[char], index: usize) -> Option<usize> {
    arch_word(text, index, &["arm64", "aarch64"])
}

fn match_arm(text: &[char], index: usize) -> Option<usize> {
    arch_word(text, index, &["arm32"])
}

/// `<digits>[ -_]bit[s][ edition]`, e.g. "64-bit" or "32 bits".
fn bits(text: &[char], index: usize, numbers: &[&str]) -> Option<usize> {
    if !after_boundary(text, index) {
        return None;
    }
    let mut len = lit_any(text, index, numbers)?;
    if at(text, index + len).is_some_and(|c| is_dash(c) || is_connector(c) || c.is_whitespace()) {
        // The separator is optional; "64bit" is as good as "64-bit".
        if lit(text, index + len + 1, "bit").is_some() {
            len += 1;
        }
    }
    len += lit(text, index + len, "bit")?;
    if at(text, index + len) == Some('s') {
        len += 1;
    }
    Some(len + edition(text, index + len))
}

fn match_64bit(text: &[char], index: usize) -> Option<usize> {
    bits(text, index, &["64"])
}

fn match_32bit(text: &[char], index: usize) -> Option<usize> {
    bits(text, index, &["32"])
}

fn match_32_or_64bit(text: &[char], index: usize) -> Option<usize> {
    bits(text, index, &["64/32", "64\\32", "32/64", "32\\64"])
}

/// Removes architecture markers and reports which architecture was named.
fn remove_architecture(text: &mut Vec<char>) -> Option<Arch> {
    // "32/64 bit" contains "64 bit", and "x86-64" contains "x86": the order
    // of these checks matters.
    if remove_all(text, match_32_or_64bit) {
        None
    } else if remove_all(text, match_x64) || remove_all(text, match_64bit) {
        Some(Arch::X64)
    } else if remove_all(text, match_x86) || remove_all(text, match_32bit) {
        Some(Arch::X86)
    } else if remove_all(text, match_arm64) {
        Some(Arch::Arm64)
    } else if remove_all(text, match_arm) {
        Some(Arch::Arm)
    } else {
        None
    }
}

// ── Language tags ────────────────────────────────────────────────────

/// Language tags that are taken out of names (lower case, sorted).
const LOCALES: &[&str] = &[
    "af-za",
    "am-et",
    "ar-ae",
    "ar-bh",
    "ar-dz",
    "ar-eg",
    "ar-iq",
    "ar-jo",
    "ar-kw",
    "ar-lb",
    "ar-ly",
    "ar-ma",
    "ar-om",
    "ar-qa",
    "ar-sa",
    "ar-sy",
    "ar-tn",
    "ar-ye",
    "arn-cl",
    "as-in",
    "az-cyrl-az",
    "az-latn-az",
    "ba-ru",
    "be-by",
    "bg-bg",
    "bn-bd",
    "bn-in",
    "bo-cn",
    "br-fr",
    "bs-cyrl-ba",
    "bs-latn-ba",
    "ca-es",
    "ca-es-valencia",
    "co-fr",
    "cs-cz",
    "cy-gb",
    "da-dk",
    "de-at",
    "de-ch",
    "de-de",
    "de-li",
    "de-lu",
    "dsb-de",
    "dv-mv",
    "el-gr",
    "en-au",
    "en-bz",
    "en-ca",
    "en-gb",
    "en-ie",
    "en-in",
    "en-jm",
    "en-my",
    "en-nz",
    "en-ph",
    "en-sg",
    "en-tt",
    "en-us",
    "en-za",
    "en-zw",
    "es-ar",
    "es-bo",
    "es-cl",
    "es-co",
    "es-cr",
    "es-do",
    "es-ec",
    "es-es",
    "es-gt",
    "es-hn",
    "es-mx",
    "es-ni",
    "es-pa",
    "es-pe",
    "es-pr",
    "es-py",
    "es-sv",
    "es-us",
    "es-uy",
    "es-ve",
    "et-ee",
    "eu-es",
    "fa-ir",
    "fi-fi",
    "fil-ph",
    "fo-fo",
    "fr-be",
    "fr-ca",
    "fr-ch",
    "fr-fr",
    "fr-lu",
    "fr-mc",
    "fy-nl",
    "ga-ie",
    "gd-db",
    "gl-es",
    "gsw-fr",
    "gu-in",
    "ha-latn-ng",
    "he-il",
    "hi-in",
    "hr-ba",
    "hr-hr",
    "hsb-de",
    "hu-hu",
    "hy-am",
    "id-id",
    "ig-ng",
    "ii-cn",
    "is-is",
    "it-ch",
    "it-it",
    "iu-cans-ca",
    "iu-latn-ca",
    "ja-jp",
    "ka-ge",
    "kk-kz",
    "kl-gl",
    "km-kh",
    "kn-in",
    "ko-kr",
    "kok-in",
    "ky-kg",
    "lb-lu",
    "lo-la",
    "lt-lt",
    "lv-lv",
    "mi-nz",
    "mk-mk",
    "ml-in",
    "mn-mn",
    "mn-mong-cn",
    "moh-ca",
    "mr-in",
    "ms-bn",
    "ms-my",
    "mt-mt",
    "nb-no",
    "ne-np",
    "nl-be",
    "nl-nl",
    "nn-no",
    "nso-za",
    "oc-fr",
    "or-in",
    "pa-in",
    "pl-pl",
    "prs-af",
    "ps-af",
    "pt-br",
    "pt-pt",
    "qut-gt",
    "quz-bo",
    "quz-ec",
    "quz-pe",
    "rm-ch",
    "ro-ro",
    "ru-ru",
    "rw-rw",
    "sa-in",
    "sah-ru",
    "se-fi",
    "se-no",
    "se-se",
    "si-lk",
    "sk-sk",
    "sl-si",
    "sma-no",
    "sma-se",
    "smj-no",
    "smj-se",
    "smn-fi",
    "sms-fi",
    "sq-al",
    "sr-cyrl-ba",
    "sr-cyrl-cs",
    "sr-cyrl-me",
    "sr-cyrl-rs",
    "sr-latn-ba",
    "sr-latn-cs",
    "sr-latn-me",
    "sr-latn-rs",
    "sv-fi",
    "sv-se",
    "sw-ke",
    "syr-sy",
    "ta-in",
    "te-in",
    "tg-cyrl-tj",
    "th-th",
    "tk-tm",
    "tn-za",
    "tr-tr",
    "tt-ru",
    "tzm-latn-dz",
    "ug-cn",
    "uk-ua",
    "ur-pk",
    "uz-cyrl-uz",
    "uz-latn-uz",
    "vi-vn",
    "wo-sn",
    "xh-za",
    "yo-ng",
    "zh-cn",
    "zh-hk",
    "zh-mo",
    "zh-sg",
    "zh-tw",
    "zu-za",
];

/// A language tag such as `en-us` or `sr-latn-rs`, standing free of letters.
fn match_locale(text: &[char], index: usize) -> Option<usize> {
    let ascii_letter = |c: char| c.is_ascii_alphabetic();
    if index > 0 && ascii_letter(text[index - 1]) {
        return None;
    }
    for language in [3usize, 2] {
        if run(text, index, is_letter) < language {
            continue;
        }
        let mut len = language;
        if at(text, index + len) == Some('-')
            && let Some(script) = lit_any(text, index + len + 1, &["cans", "cyrl", "latn", "mong"])
        {
            len += 1 + script;
        }
        if at(text, index + len) != Some('-') {
            continue;
        }
        len += 1;
        if !(at(text, index + len).is_some_and(is_letter)
            && at(text, index + len + 1).is_some_and(is_letter))
        {
            continue;
        }
        len += 2;
        if at(text, index + len).is_some_and(ascii_letter) {
            continue;
        }
        if let Some(valencia) = lit(text, index + len, "-valencia") {
            len += valencia;
        }
        let tag: String = text[index..index + len].iter().collect();
        if LOCALES.binary_search(&tag.as_str()).is_ok() {
            return Some(len);
        }
    }
    None
}

// ── Version numbers ──────────────────────────────────────────────────

/// Words that introduce a version number.
const VERSION_WORDS: &[&str] = &[
    "v",
    "ver",
    "version",
    "versi\u{f3}n",
    "vers\u{e3}o",
    "versie",
    "wersja",
    "build",
    "release",
    "rc",
    "sp",
];

/// One `.<digits>` part of a version, with an optional pre-release letter
/// as in `.0rc1` or `-b2`. Returns its length.
fn version_part(text: &[char], index: usize) -> Option<usize> {
    if !at(text, index).is_some_and(is_joiner) {
        return None;
    }
    let start = index + 1;
    let marked = |from: usize| -> Option<usize> {
        let marker = lit_any(text, from, &["rc", "b", "a", "r", "sp", "k"])?;
        let digits = run(text, from + marker, is_digit);
        (digits > 0).then_some(from + marker + digits)
    };
    let end = if at(text, start).is_some_and(is_digit) {
        marked(start + 1)
    } else {
        None
    }
    .or_else(|| marked(start))
    .or_else(|| {
        let digits = run(text, start, is_digit);
        (digits > 0).then_some(start + digits)
    })?;
    Some(end - index)
}

/// `1.2.3` and everything glued to it: digits, at least one further part,
/// then any run of letters and digits joined by single punctuation marks.
fn version_core(text: &[char], index: usize) -> Option<usize> {
    let mut len = run(text, index, is_digit);
    if len == 0 {
        return None;
    }
    len += version_part(text, index + len)?;
    while let Some(part) = version_part(text, index + len) {
        len += part;
    }
    loop {
        let joiner = usize::from(at(text, index + len).is_some_and(is_joiner));
        let word = run(text, index + len + joiner, is_alnum);
        if word == 0 {
            break;
        }
        len += joiner + word;
    }
    Some(len)
}

/// A dotted version, optionally introduced by a word: `version 2.1.0`.
fn match_version_delimited(text: &[char], index: usize) -> Option<usize> {
    if index == 0 || !is_letter(text[index - 1]) {
        for word in VERSION_WORDS {
            let Some(word_len) = lit(text, index, word) else {
                continue;
            };
            let after = index + word_len;
            // One character that is not a letter may stand between the
            // word and the number.
            if at(text, after).is_some_and(|c| !is_letter(c))
                && let Some(core) = version_core(text, after + 1)
            {
                return Some(word_len + 1 + core);
            }
            if let Some(core) = version_core(text, after) {
                return Some(word_len + core);
            }
        }
    }
    version_core(text, index)
}

/// A short version introduced by a word or letter: `v2`, `sp1`, `r 3.5`.
fn match_version_word(text: &[char], index: usize) -> Option<usize> {
    const WORDS: &[&str] = &[
        "p",
        "v",
        "r",
        "ver",
        "version",
        "versi\u{f3}n",
        "vers\u{e3}o",
        "versie",
        "wersja",
        "build",
        "release",
        "rc",
        "sp",
    ];
    // An optional "for " in front belongs to the match.
    let lead = lit(text, index, "for")
        .filter(|n| at(text, index + n).is_some_and(char::is_whitespace))
        .map(|n| n + 1);
    for prefix in [lead, Some(0)].into_iter().flatten() {
        let start = index + prefix;
        if start > 0 && is_letter(text[start - 1]) {
            continue;
        }
        for word in WORDS {
            let Some(word_len) = lit(text, start, word) else {
                continue;
            };
            let after = start + word_len;
            let not_letter = |i: usize| at(text, i).is_some_and(|c| !is_letter(c));
            let mut gaps = Vec::with_capacity(3);
            if not_letter(after) {
                gaps.push(1);
                if at(text, after + 1).is_some_and(is_letter) {
                    gaps.push(2);
                }
            }
            gaps.push(0);
            for gap in gaps {
                let mut end = after + gap;
                let number_start = end;
                loop {
                    if at(text, end).is_some_and(is_digit) {
                        end += 1;
                    } else if at(text, end) == Some('.') && at(text, end + 1).is_some_and(is_digit)
                    {
                        end += 2;
                    } else {
                        break;
                    }
                }
                if end == number_start {
                    continue;
                }
                if let Some(marker) = lit_any(text, end, &["rc", "b", "a", "r", "v", "sp"]) {
                    end += marker;
                }
                if at(text, end).is_some_and(is_digit) {
                    end += 1;
                }
                return Some(end - index);
            }
        }
    }
    None
}

/// A version that starts with a letter: `r2019.1`, `v1.2.3`.
fn match_version_letter(text: &[char], index: usize) -> Option<usize> {
    if index > 0 && is_letter(text[index - 1]) {
        return None;
    }
    let body = |from: usize| -> Option<usize> {
        if !at(text, from).is_some_and(is_letter) {
            return None;
        }
        let digits = run(text, from + 1, is_digit);
        if digits == 0 {
            return None;
        }
        let mut end = from + 1 + digits;
        let mut parts = 0;
        while at(text, end).is_some_and(is_joiner) {
            let more = run(text, end + 1, is_digit);
            if more == 0 {
                break;
            }
            end += 1 + more;
            parts += 1;
        }
        (parts > 0).then_some(end)
    };
    for word in VERSION_WORDS {
        if let Some(word_len) = lit(text, index, word)
            && at(text, index + word_len).is_some_and(|c| !is_letter(c))
            && let Some(end) = body(index + word_len + 1)
        {
            return Some(end - index);
        }
    }
    body(index).map(|end| end - index)
}

// ── Brackets, paths and other clutter ────────────────────────────────

/// `(…)` at the very start of a name.
fn remove_prefix_parens(text: &mut Vec<char>) -> bool {
    if text.first() != Some(&'(') {
        return false;
    }
    match text.iter().position(|c| *c == ')') {
        Some(close) => {
            text.drain(..=close);
            true
        }
        None => false,
    }
}

/// `()`, `[]` and `""` with nothing but spaces inside.
fn match_empty_parens(text: &[char], index: usize) -> Option<usize> {
    let close = match at(text, index)? {
        '(' => ')',
        '[' => ']',
        '"' => '"',
        _ => return None,
    };
    let spaces = run(text, index + 1, char::is_whitespace);
    (at(text, index + 1 + spaces) == Some(close)).then_some(spaces + 2)
}

/// `(…)` or `[…]` without further brackets of the same kind inside.
fn match_plain_bracket(text: &[char], index: usize) -> Option<usize> {
    let (open, close) = match at(text, index)? {
        '(' => ('(', ')'),
        '[' => ('[', ']'),
        _ => return None,
    };
    let inner = run(text, index + 1, |c| c != open && c != close);
    (at(text, index + 1 + inner) == Some(close)).then_some(inner + 2)
}

/// From the first opening bracket to the last closing one, or from the first
/// to the last quotation mark.
fn remove_bracket_enclosed(text: &mut Vec<char>) -> bool {
    let brackets = text
        .iter()
        .position(|c| is_open_bracket(*c))
        .and_then(|open| {
            let close = text.iter().rposition(|c| is_close_bracket(*c))?;
            (close > open).then_some((open, close))
        });
    let quotes = text.iter().position(|c| *c == '"').and_then(|open| {
        let close = text.iter().rposition(|c| *c == '"')?;
        (close > open).then_some((open, close))
    });
    // The leftmost of the two wins, as it would in a single left-to-right scan.
    let span = match (brackets, quotes) {
        (Some(b), Some(q)) => Some(if q.0 < b.0 { q } else { b }),
        (b, q) => b.or(q),
    };
    match span {
        Some((open, close)) => {
            text.drain(open..=close);
            true
        }
        None => false,
    }
}

/// `http://`, `https://`, `ftp://` not glued to a preceding letter.
fn match_uri_protocol(text: &[char], index: usize) -> Option<usize> {
    if index > 0 && is_letter(text[index - 1]) {
        return None;
    }
    lit_any(text, index, &["https://", "http://", "ftp://"])
}

/// A folder path such as `c:\program files\tool`, with an optional
/// "installed at" or "in" before it.
fn match_file_path(text: &[char], index: usize) -> Option<usize> {
    let lead = lit_any(text, index, &["installed at", "in"])
        .filter(|n| at(text, index + n).is_some_and(char::is_whitespace))
        .map(|n| n + 1);
    for prefix in [lead, Some(0)].into_iter().flatten() {
        let drive = index + prefix;
        if !matches!(at(text, drive), Some('c'..='f'))
            || at(text, drive + 1) != Some(':')
            || at(text, drive + 2) != Some('\\')
        {
            continue;
        }
        // Up to the last backslash, then to the end of that word.
        let last_backslash = text.iter().rposition(|c| *c == '\\').unwrap_or(drive + 2);
        let end = last_backslash + 1 + run(text, last_backslash + 1, |c| !c.is_whitespace());
        return Some(end - index);
    }
    None
}

fn trim_leading_symbols(text: &mut Vec<char>) -> bool {
    let symbols = run(text, 0, |c| !is_alnum(c));
    text.drain(..symbols);
    symbols > 0
}

fn trim_trailing(text: &mut Vec<char>, removable: impl Fn(char) -> bool) -> bool {
    let keep = text
        .iter()
        .rposition(|c| !removable(*c))
        .map_or(0, |last| last + 1);
    let changed = keep < text.len();
    text.truncate(keep);
    changed
}

/// " en" at the end of a name (the English edition).
fn remove_trailing_en(text: &mut Vec<char>) -> bool {
    let end = text
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(0, |last| last + 1);
    if end >= 3 && text[end - 2] == 'e' && text[end - 1] == 'n' && text[end - 3].is_whitespace() {
        text.truncate(end - 3);
        true
    } else {
        false
    }
}

/// `(kb123456)` loses its brackets so the number survives bracket removal.
fn unwrap_kb_numbers(text: &mut Vec<char>) {
    let mut out = Vec::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if text[index] == '('
            && let Some(kb) = lit(text, index + 1, "kb")
        {
            let digits = run(text, index + 1 + kb, |c| c.is_ascii_digit());
            let close = index + 1 + kb + digits;
            if digits > 0 && at(text, close) == Some(')') {
                out.extend_from_slice(&text[index + 1..close]);
                index = close + 1;
                continue;
            }
        }
        out.push(text[index]);
        index += 1;
    }
    *text = out;
}

// ── The two normalisations ───────────────────────────────────────────

/// Company-form words that are not part of a name (lower case, sorted).
const LEGAL_SUFFIXES: &[&str] = &[
    "ab",
    "ad",
    "ag",
    "aps",
    "as",
    "asa",
    "bv",
    "co",
    "company",
    "corp",
    "corporation",
    "cv",
    "doo",
    "ev",
    "ges",
    "gesmbh",
    "gmbh",
    "holding",
    "holdings",
    "inc",
    "incorporated",
    "kg",
    "ks",
    "limited",
    "llc",
    "lp",
    "ltd",
    "ltda",
    "mbh",
    "nv",
    "plc",
    "ps",
    "pty",
    "pvt",
    "sa",
    "sarl",
    "sc",
    "sca",
    "sl",
    "sp",
    "spa",
    "srl",
    "sro",
    "subsidiary",
];

fn is_legal_suffix(token: &str) -> bool {
    LEGAL_SUFFIXES.binary_search(&token).is_ok()
}

/// Compatibility-composes the text (so that "™" reads "tm" and full-width
/// letters read as plain ones), exactly as the catalog does.
fn compatibility_form(text: &str) -> String {
    if text.is_ascii() {
        return text.to_owned();
    }
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: both calls receive valid slices; the first only measures.
    let needed = unsafe { NormalizeString(NormalizationKC, &wide, None) };
    if needed <= 0 {
        return text.to_owned();
    }
    let mut buffer = vec![0u16; needed as usize];
    // SAFETY: `buffer` has the size the first call asked for.
    let written = unsafe { NormalizeString(NormalizationKC, &wide, Some(&mut buffer)) };
    if written <= 0 {
        return text.to_owned();
    }
    String::from_utf16_lossy(&buffer[..written as usize])
}

/// Lower-cases, composes, trims and unwraps a raw value.
fn prepare(value: &str) -> Vec<char> {
    let folded = compatibility_form(&value.to_lowercase());
    let mut text: Vec<char> = folded.trim().chars().collect();
    // Some installers append "@@" and an internal tag.
    if let Some(tag) =
        (3..text.len().saturating_sub(1)).find(|i| text[*i] == '@' && text[i + 1] == '@')
    {
        text.truncate(tag);
    }
    // A value wrapped whole in quotes or brackets loses the wrapping.
    while text.len() >= 2 && matches!((text[0], text[text.len() - 1]), ('"', '"') | ('(', ')')) {
        text.pop();
        text.remove(0);
    }
    text
}

/// Splits into words, optionally dropping company-form words, and joins
/// what is left without separators or punctuation.
fn join_words(text: &[char], is_separator: impl Fn(char) -> bool, stop_at_suffix: bool) -> String {
    let mut out = String::new();
    let mut first = true;
    for token in text.split(|c| is_separator(*c)) {
        if token.iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let word: String = token.iter().collect();
        // The first word always stays, whatever it is.
        if !first && is_legal_suffix(&word) {
            if stop_at_suffix {
                break;
            }
            continue;
        }
        first = false;
        out.extend(word.chars().filter(|c| is_alnum(*c)));
    }
    out
}

/// Normalises a program's display name.
pub(crate) fn normalize_name(name: &str) -> NormalizedName {
    let mut text = prepare(name);
    let arch = remove_architecture(&mut text);
    remove_all(&mut text, match_locale);
    unwrap_kb_numbers(&mut text);

    // Each pass can expose more to remove ("tool (v2 (beta))"), so repeat
    // until a whole pass changes nothing.
    loop {
        let mut changed = remove_prefix_parens(&mut text);
        changed |= remove_all(&mut text, match_empty_parens);
        changed |= remove_all(&mut text, match_file_path);
        changed |= remove_all(&mut text, match_version_letter);
        changed |= remove_all(&mut text, match_version_delimited);
        changed |= remove_all(&mut text, match_version_word);
        changed |= remove_trailing_en(&mut text);
        changed |= remove_all(&mut text, match_plain_bracket);
        changed |= remove_bracket_enclosed(&mut text);
        changed |= remove_all(&mut text, match_uri_protocol);
        changed |= trim_leading_symbols(&mut text);
        changed |= trim_trailing(&mut text, |c| !is_alnum(c));
        if !changed {
            break;
        }
    }
    NormalizedName {
        name: join_words(&text, |c| !(is_alnum(c) || c == '+' || c == '&'), false),
        arch,
    }
}

/// Removes words made of anything but letters, e.g. a lone "2" or "-".
fn remove_letterless_words(text: &mut Vec<char>) -> bool {
    let mut out: Vec<char> = Vec::with_capacity(text.len());
    let mut changed = false;
    let mut index = 0;
    while index < text.len() {
        if text[index].is_whitespace() {
            out.push(text[index]);
            index += 1;
            continue;
        }
        let word = run(text, index, |c| !c.is_whitespace());
        if text[index..index + word].iter().any(|c| is_letter(*c)) {
            out.extend_from_slice(&text[index..index + word]);
        } else {
            changed = true;
        }
        index += word;
    }
    *text = out;
    changed
}

/// The dots and slashes of spelled-out abbreviations: "s.r.o." → "sro.".
fn match_acronym_separator(text: &[char], index: usize) -> Option<usize> {
    if !matches!(at(text, index), Some('.' | '/')) || index == 0 {
        return None;
    }
    let single_letter_before =
        is_letter(text[index - 1]) && (index == 1 || !is_letter(text[index - 2]));
    let single_letter_after =
        at(text, index + 1).is_some_and(is_letter) && !at(text, index + 2).is_some_and(is_letter);
    (single_letter_before && single_letter_after).then_some(1)
}

/// Normalises a publisher name.
pub(crate) fn normalize_publisher(publisher: &str) -> String {
    let mut text = prepare(publisher);
    loop {
        let mut changed = remove_all(&mut text, match_version_delimited);
        changed |= remove_all(&mut text, match_version_word);
        changed |= remove_all(&mut text, match_plain_bracket);
        changed |= remove_bracket_enclosed(&mut text);
        changed |= remove_all(&mut text, match_uri_protocol);
        changed |= remove_letterless_words(&mut text);
        changed |= trim_trailing(&mut text, |c| !is_letter(c));
        changed |= remove_all(&mut text, match_acronym_separator);
        if !changed {
            break;
        }
    }
    join_words(&text, |c| !is_alnum(c), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(input: &str) -> String {
        normalize_name(input).name
    }

    fn full(input: &str) -> (String, Option<Arch>) {
        let n = normalize_name(input);
        (n.name, n.arch)
    }

    #[test]
    fn lists_are_sorted_for_binary_search() {
        assert!(LOCALES.windows(2).all(|w| w[0] < w[1]));
        assert!(LEGAL_SUFFIXES.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn names_as_windows_shows_them_match_the_catalog_spelling() {
        // Left: display names as installers register them. Right: the
        // normalised names the public catalog stores for those packages.
        for (display, expected, arch) in [
            (
                "Mozilla Firefox (x64 en-US)",
                "mozillafirefox",
                Some(Arch::X64),
            ),
            (
                "Mozilla Firefox 131.0 (x86 de)",
                "mozillafirefox",
                Some(Arch::X86),
            ),
            ("7-Zip 23.01 (x64)", "7zip", Some(Arch::X64)),
            ("7-Zip 24.09", "7zip", None),
            ("Git", "git", None),
            ("Git version 2.47.1", "git", None),
            ("VLC media player", "vlcmediaplayer", None),
            (
                "Microsoft Visual Studio Code (User)",
                "microsoftvisualstudiocode",
                None,
            ),
            ("Google Chrome", "googlechrome", None),
            ("Notepad++ (64-bit x64)", "notepad", Some(Arch::X64)),
            (
                "Microsoft Visual C++ 2015-2022 Redistributable (x64) - 14.36.32532",
                "microsoftvisualcredistributable",
                Some(Arch::X64),
            ),
            (
                "Microsoft Visual C++ 2015 Redistributable (x64) - 14.0.24215",
                "microsoftvisualc2015redistributable",
                Some(Arch::X64),
            ),
            ("PhpStorm 2025.1.2", "phpstorm", None),
            ("Docker Desktop", "dockerdesktop", None),
            ("Steam", "steam", None),
            ("NVM for Windows 1.2.2", "nvmforwindows", None),
            ("Zoom Workplace (64-bit)", "zoomworkplace", Some(Arch::X64)),
            ("Adobe Acrobat (64-bit)", "adobeacrobat", Some(Arch::X64)),
            ("Python 3.12.1 (64-bit)", "python", Some(Arch::X64)),
            ("PowerShell 7-x64", "powershell7", Some(Arch::X64)),
            ("Node.js", "nodejs", None),
            ("WinRAR 7.01 (64-bit)", "winrar", Some(Arch::X64)),
            (
                "Windows Driver Package - Contoso (usbser) Ports  (01/01/2020 1.0.0.0)",
                "windowsdriverpackagecontosoports",
                None,
            ),
        ] {
            assert_eq!(full(display), (expected.to_owned(), arch), "{display}");
        }
    }

    #[test]
    fn architecture_is_recognised_in_its_many_spellings() {
        assert_eq!(full("Tool x64"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(full("Tool AMD64"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(full("Tool x86-64"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(full("Tool x86_64"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(full("Tool 64 bit"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(
            full("Tool 64-bit edition"),
            ("tool".into(), Some(Arch::X64))
        );
        assert_eq!(full("Tool 64bits"), ("tool".into(), Some(Arch::X64)));
        assert_eq!(full("Tool x86"), ("tool".into(), Some(Arch::X86)));
        assert_eq!(full("Tool (32-bit)"), ("tool".into(), Some(Arch::X86)));
        assert_eq!(full("Tool ARM64"), ("tool".into(), Some(Arch::Arm64)));
        assert_eq!(full("Tool aarch64"), ("tool".into(), Some(Arch::Arm64)));
        assert_eq!(full("Tool arm32"), ("tool".into(), Some(Arch::Arm)));
        // Both architectures in one installer: none is claimed.
        assert_eq!(full("Tool 32/64 bit"), ("tool".into(), None));
        // Part of a word or of a longer number: not an architecture.
        assert_eq!(full("Maxx64"), ("maxx64".into(), None));
        assert_eq!(full("Tool x640"), ("toolx640".into(), None));
        assert_eq!(
            normalize_name("Tool x64").with_arch().as_deref(),
            Some("tool(X64)")
        );
        assert_eq!(normalize_name("Tool").with_arch(), None);
    }

    #[test]
    fn versions_are_removed_in_every_form() {
        assert_eq!(name("Tool 1.2.3"), "tool");
        assert_eq!(name("Tool 1.2.3-beta4"), "tool");
        assert_eq!(name("Tool v2"), "tool");
        assert_eq!(name("Tool Version 10"), "tool");
        assert_eq!(name("Tool SP1"), "tool");
        assert_eq!(name("Tool R2019.1"), "tool");
        assert_eq!(name("Tool build 1234.5"), "tool");
        assert_eq!(name("Tool 2.0rc1"), "tool");
        assert_eq!(name("Tool for v3"), "tool");
        assert_eq!(name("Tool 2015-2022 Runtime"), "toolruntime");
        // A lone number is part of the name, not a version.
        assert_eq!(name("Windows 11 Helper"), "windows11helper");
        assert_eq!(
            name("Visual Studio Community 2022"),
            "visualstudiocommunity2022"
        );
        assert_eq!(name("7-Zip"), "7zip");
        // Letters before a digit do not start a version word.
        assert_eq!(name("MP3 Tagger"), "mp3tagger");
    }

    #[test]
    fn brackets_languages_and_clutter_are_removed() {
        assert_eq!(name("Tool (remove only)"), "tool");
        assert_eq!(name("Tool [beta]"), "tool");
        assert_eq!(name("(Trial) Tool"), "tool");
        assert_eq!(name("\"Tool\""), "tool");
        assert_eq!(name("Tool en-US"), "tool");
        assert_eq!(name("Tool sr-Latn-RS edition"), "tooledition");
        assert_eq!(name("Tool xx-YY"), "toolxxyy", "unknown tags stay");
        assert_eq!(name("Tool EN"), "tool");
        assert_eq!(
            name("Security Update (KB5031234)"),
            "securityupdatekb5031234"
        );
        assert_eq!(name("Tool installed at C:\\Program Files\\Tool"), "tool");
        assert_eq!(name("https://example.org Tool"), "exampleorgtool");
        assert_eq!(name("  -- Tool ++  "), "tool");
        assert_eq!(name("Tool@@internal-tag"), "tool");
        assert_eq!(name("Rock & Roll + Co Tools"), "rockrolltools");
        assert_eq!(name(""), "");
        assert_eq!(name("()"), "");
    }

    #[test]
    fn compatibility_characters_read_as_plain_ones() {
        // The trade mark sign reads "TM", in capitals: the catalog composes
        // after lower-casing and so keeps them as well.
        assert_eq!(name("Tool\u{2122} Pro"), "toolTMpro");
        assert_eq!(name("\u{ff34}\u{ff4f}\u{ff4f}\u{ff4c}"), "tool");
        assert_eq!(name("Intel\u{ae} Graphics"), "intelgraphics");
        assert_eq!(
            name("\u{7f51}\u{6613}\u{4e91}\u{97f3}\u{4e50}"),
            "\u{7f51}\u{6613}\u{4e91}\u{97f3}\u{4e50}"
        );
    }

    #[test]
    fn publishers_lose_their_company_form() {
        for (publisher, expected) in [
            ("Mozilla", "mozilla"),
            ("Mozilla Corporation", "mozilla"),
            ("Google LLC", "google"),
            ("Igor Pavlov", "igorpavlov"),
            (
                "The Git Development Community",
                "thegitdevelopmentcommunity",
            ),
            ("Notepad++ Team", "notepadteam"),
            ("Microsoft Corporation", "microsoft"),
            ("Valve Corporation", "valve"),
            ("Docker Inc.", "docker"),
            ("JetBrains s.r.o.", "jetbrains"),
            ("Zoom Video Communications, Inc.", "zoomvideocommunications"),
            ("Example Software Ltd", "examplesoftware"),
            ("Example GmbH & Co. KG", "example"),
            ("Contoso S.A.", "contoso"),
            ("Contoso (Europe) Ltd.", "contoso"),
            ("Contoso 2000", "contoso"),
            ("Inc", "inc"),
            ("", ""),
        ] {
            assert_eq!(normalize_publisher(publisher), expected, "{publisher}");
        }
    }

    #[test]
    fn odd_input_never_panics() {
        for input in [
            "(",
            ")",
            "((",
            "\"",
            "[",
            "]]",
            "@@",
            "a@@",
            "abc@@d",
            "v",
            "v.",
            "1.",
            ".1",
            "-",
            "x64",
            "64-bit",
            "en-us",
            "c:\\",
            "in c:\\x",
            "for v",
            "(kb)",
            "(kb1",
            "\u{feff}",
            "1.2.3.4.5.6.7.8.9",
            "....",
            "////",
            "a.b.c.d",
            "s.r.o.",
        ] {
            let _ = normalize_name(input);
            let _ = normalize_publisher(input);
        }
    }
}
