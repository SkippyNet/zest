//! Parser for the package manager's text tables.
//!
//! The Windows Package Manager has no machine-readable output for its
//! upgrade listing, so the table it prints is the interface. This parser is
//! built to survive everything that table is known to do:
//!
//! * **Locale independence** – header labels are translated, so nothing is
//!   matched by name. Columns are identified by *position* (name, id,
//!   installed version, available version, source) and located through the
//!   header's column offsets.
//! * **Display width** – columns are padded by terminal cells, not bytes or
//!   characters; East Asian wide characters occupy two cells.
//! * **Control noise** – VT/ANSI sequences are stripped and carriage-return
//!   progress spinners are replayed the way a terminal would show them.
//! * **Width drift** – if a row's name contains characters whose width is
//!   measured differently than the package manager measured them, the row is
//!   re-synchronised by a small shift rather than mis-sliced.
//! * **Extra text** – footers ("N upgrades available."), notes about pinned
//!   or unknown-version packages and follow-up tables are told apart from
//!   rows structurally.
//! * **Truncation** – cells cut short with `…` on narrow consoles are
//!   detected so a shortened package id is never passed on as if complete.

use unicode_width::UnicodeWidthChar;

/// The character the package manager appends to a cell it had to shorten.
pub(crate) const ELLIPSIS: char = '\u{2026}';

/// Largest column shift tried when re-synchronising a row.
const MAX_RESYNC: isize = 4;

fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Removes VT/ANSI escape sequences and stray control characters, keeping
/// line structure (`\n`, `\r`, backspace and tab) for [`render_line`].
fn strip_control_sequences(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then one final byte in '@'..='~'.
                Some('[') => {
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: terminated by BEL or by ESC '\'.
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' {
                            break;
                        }
                        if n == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Character set selection carries one more byte.
                Some('(') | Some(')') => {
                    chars.next();
                }
                // Any other escape is a two character sequence.
                _ => {}
            },
            // 8-bit CSI.
            '\u{9b}' => {
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            '\n' | '\r' | '\t' | '\u{8}' => out.push(c),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Replays carriage returns and backspaces the way a terminal would, so a
/// line such as `"  - \r  \\ \r     \rName  Id"` becomes `"Name  Id"`.
fn render_line(raw: &str) -> String {
    let mut cells: Vec<char> = Vec::new();
    let mut cursor = 0usize;
    for c in raw.chars() {
        match c {
            '\r' => cursor = 0,
            '\u{8}' => cursor = cursor.saturating_sub(1),
            c => {
                if cursor < cells.len() {
                    cells[cursor] = c;
                } else {
                    cells.push(c);
                }
                cursor += 1;
            }
        }
    }
    let line: String = cells.into_iter().collect();
    line.trim_end().to_owned()
}

/// True for leftovers of progress indicators: a lone spinner glyph or a
/// download bar drawn with block characters.
fn is_progress_noise(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains(['\u{2588}', '\u{2592}', '\u{2591}', '\u{2593}']) {
        return true;
    }
    trimmed.chars().count() < 10
        && trimmed
            .chars()
            .all(|c| matches!(c, '-' | '\\' | '|' | '/' | ' '))
}

/// Turns raw process output into the lines a user would have seen.
pub(crate) fn clean_lines(raw: &str) -> Vec<String> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    strip_control_sequences(raw)
        .split('\n')
        .map(render_line)
        .map(|line| {
            if is_progress_noise(&line) {
                String::new()
            } else {
                line
            }
        })
        .collect()
}

/// A line laid out by terminal cell.
struct Layout {
    /// Each character with the display column it starts at.
    chars: Vec<(usize, char)>,
    /// For every display column: is it covered by a non-space character?
    occupied: Vec<bool>,
}

impl Layout {
    fn new(line: &str) -> Self {
        let mut chars = Vec::with_capacity(line.len());
        let mut occupied = Vec::with_capacity(line.len());
        let mut col = 0usize;
        for c in line.chars() {
            chars.push((col, c));
            let width = char_width(c);
            for _ in 0..width {
                occupied.push(!c.is_whitespace());
            }
            col += width;
        }
        Layout { chars, occupied }
    }

    fn is_occupied(&self, col: usize) -> bool {
        self.occupied.get(col).copied().unwrap_or(false)
    }

    /// True when no non-space character lies in `[from, to)`.
    fn is_blank(&self, from: usize, to: usize) -> bool {
        (from..to.min(self.occupied.len())).all(|col| !self.occupied[col])
    }

    /// Text whose characters start within `[from, to)`, trimmed.
    fn slice(&self, from: usize, to: usize) -> String {
        self.chars
            .iter()
            .filter(|(col, _)| *col >= from && *col < to)
            .map(|(_, c)| *c)
            .collect::<String>()
            .trim()
            .to_owned()
    }

    fn width(&self) -> usize {
        self.occupied.len()
    }
}

/// Display columns at which a word of the header begins. Every real column
/// starts at one of these; a translated label made of several words
/// ("K dispozici") contributes extra candidates that [`choose_columns`]
/// weeds out.
fn header_word_starts(header: &str) -> Vec<usize> {
    let layout = Layout::new(header);
    let mut starts = Vec::new();
    let mut previous_blank = true;
    for col in 0..layout.width() {
        let blank = !layout.occupied[col];
        if !blank && previous_blank {
            starts.push(col);
        }
        previous_blank = blank;
    }
    starts
}

/// More header words than this and the subset search is skipped.
const MAX_HEADER_WORDS: usize = 12;

/// How well a body line reads as a row for the given column starts.
/// Zero means "not a row"; higher is better.
fn row_score(layout: &Layout, starts: &[usize]) -> usize {
    let Some(cells) = split_layout(layout, starts) else {
        return 0;
    };
    let has_space = |cell: &String| cell.chars().any(char::is_whitespace);
    // Every listing has a name and an identifier; identifiers of packages
    // that can be upgraded never contain spaces, sentences always do.
    if cells.len() < 2 || cells[0].is_empty() || cells[1].is_empty() || has_space(&cells[1]) {
        return 0;
    }
    let filled = cells.iter().filter(|cell| !cell.is_empty()).count();
    let last = cells.len() - 1;
    let mut penalty = 0;
    for (index, cell) in cells.iter().enumerate().skip(2) {
        if has_space(cell) {
            // A space in the last column almost certainly means two columns
            // were merged; in a middle column it is merely unusual.
            penalty += if index == last { 2 } else { 1 };
        }
    }
    (filled * 2).saturating_sub(penalty)
}

/// Calls `visit` with every way of picking `pick` items from `pool`, in
/// lexicographic order.
fn for_each_combination(pool: &[usize], pick: usize, visit: &mut dyn FnMut(&[usize])) {
    fn recurse(
        pool: &[usize],
        pick: usize,
        from: usize,
        chosen: &mut Vec<usize>,
        visit: &mut dyn FnMut(&[usize]),
    ) {
        if chosen.len() == pick {
            visit(chosen);
            return;
        }
        let needed = pick - chosen.len();
        for index in from..=pool.len().saturating_sub(needed) {
            if index >= pool.len() {
                break;
            }
            chosen.push(pool[index]);
            recurse(pool, pick, index + 1, chosen, visit);
            chosen.pop();
        }
    }
    if pick <= pool.len() {
        recurse(pool, pick, 0, &mut Vec::with_capacity(pick), visit);
    }
}

/// Decides where the columns of a table start.
///
/// Normally every word of the header is a column. When a header label
/// consists of several words there are more words than columns; the body
/// then decides: the subset of word starts under which the body lines read
/// best as rows wins. Listings have five columns (four without a source), so
/// those sizes are tried first.
fn choose_columns(header: &str, body: &[Layout]) -> Vec<usize> {
    let candidates = header_word_starts(header);
    let count = candidates.len();
    if !(2..=MAX_HEADER_WORDS).contains(&count) {
        return candidates;
    }
    let mut sizes = vec![count.min(5)];
    if count > 4 {
        sizes.push(4);
    }
    sizes.extend((2..=count).rev().filter(|size| *size != 5 && *size != 4));
    sizes.dedup();

    for size in sizes {
        let mut best: Option<(usize, Vec<usize>)> = None;
        for_each_combination(&candidates[1..], size - 1, &mut |rest| {
            let mut starts = Vec::with_capacity(size);
            starts.push(candidates[0]);
            starts.extend_from_slice(rest);
            let score: usize = body.iter().map(|line| row_score(line, &starts)).sum();
            if score > best.as_ref().map_or(0, |(top, _)| *top) {
                best = Some((score, starts));
            }
        });
        if let Some((_, starts)) = best {
            return starts;
        }
    }
    candidates
}

/// Checks that `layout` is a row of left-aligned cells for these column
/// starts: every column is preceded by a blank cell and either begins
/// exactly at its start or is entirely empty.
fn fits(layout: &Layout, starts: &[usize]) -> bool {
    for (i, &start) in starts.iter().enumerate().skip(1) {
        if start == 0 || layout.is_occupied(start - 1) {
            return false;
        }
        let end = starts.get(i + 1).copied().unwrap_or(usize::MAX);
        if !layout.is_occupied(start) && !layout.is_blank(start, end) {
            return false;
        }
    }
    true
}

/// Slices one row into cells, shifting the columns after the first by a few
/// cells if that is what it takes for the row to line up.
fn split_layout(layout: &Layout, starts: &[usize]) -> Option<Vec<String>> {
    if layout.width() == 0 || starts.is_empty() {
        return None;
    }
    let mut deltas = vec![0isize];
    for d in 1..=MAX_RESYNC {
        deltas.push(-d);
        deltas.push(d);
    }
    for delta in deltas {
        let mut shifted = Vec::with_capacity(starts.len());
        shifted.push(starts[0]);
        let mut valid = true;
        for &start in &starts[1..] {
            let moved = start as isize + delta;
            if moved <= *shifted.last().unwrap_or(&0) as isize {
                valid = false;
                break;
            }
            shifted.push(moved as usize);
        }
        if !valid || !fits(layout, &shifted) {
            continue;
        }
        // A shifted match must actually have something in the second column;
        // otherwise any short line would "fit" at every shift.
        if delta != 0 && shifted.get(1).is_none_or(|&s| !layout.is_occupied(s)) {
            continue;
        }
        let cells = shifted
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                let end = shifted.get(i + 1).copied().unwrap_or(usize::MAX);
                layout.slice(start, end)
            })
            .collect();
        return Some(cells);
    }
    None
}

/// One table found in the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Table {
    /// Header labels as printed (translated).
    pub header: Vec<String>,
    /// Data rows; every row has `header.len()` cells.
    pub rows: Vec<Vec<String>>,
}

fn is_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= 10 && trimmed.chars().all(|c| c == '-')
}

/// Finds every table (header line, dashed separator, rows) in cleaned output.
pub(crate) fn parse_tables(lines: &[String]) -> Vec<Table> {
    // A separator only counts when the line above it can serve as a header.
    let separators: Vec<usize> = (1..lines.len())
        .filter(|&i| is_separator(&lines[i]) && !lines[i - 1].trim().is_empty())
        .collect();

    let mut tables = Vec::new();
    for (n, &sep) in separators.iter().enumerate() {
        let header_line = &lines[sep - 1];
        // The body runs until the header of the next table (or the end).
        let end = separators.get(n + 1).map_or(lines.len(), |&next| next - 1);
        let body: Vec<Layout> = lines[sep + 1..end.max(sep + 1)]
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| Layout::new(line))
            .collect();

        let starts = choose_columns(header_line, &body);
        if starts.len() < 2 {
            continue;
        }
        let header_layout = Layout::new(header_line);
        let header: Vec<String> = starts
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                header_layout.slice(start, starts.get(i + 1).copied().unwrap_or(usize::MAX))
            })
            .collect();

        let rows = body
            .iter()
            .filter_map(|line| split_layout(line, &starts))
            // Footers shorter than the first column parse as a lone "name".
            .filter(|cells| !cells[0].is_empty() && !cells[1].is_empty())
            .collect();
        tables.push(Table { header, rows });
    }
    tables
}

/// One row of an upgrade listing, before it becomes a public
/// [`super::SoftwareUpdate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UpgradeRow {
    pub name: String,
    pub id: String,
    pub installed: String,
    pub available: String,
    pub source: String,
    /// The row came from a follow-up table (packages that are only upgraded
    /// when named explicitly).
    pub secondary: bool,
}

/// Extracts upgrade rows from raw output of the upgrade listing.
pub(crate) fn parse_upgrade_rows(raw: &str) -> Vec<UpgradeRow> {
    let lines = clean_lines(raw);
    let mut rows: Vec<UpgradeRow> = Vec::new();
    for (index, table) in parse_tables(&lines).into_iter().enumerate() {
        // Name, Id, Version, Available[, Source] - anything else is not an
        // upgrade table.
        if !(4..=5).contains(&table.header.len()) {
            continue;
        }
        for cells in table.rows {
            let id = cells[1].clone();
            let available = cells[3].clone();
            // Package identifiers never contain spaces; sentences do.
            if id.chars().any(char::is_whitespace) || available.is_empty() {
                continue;
            }
            let row = UpgradeRow {
                name: cells[0].clone(),
                id,
                installed: cells[2].clone(),
                available,
                source: cells.get(4).cloned().unwrap_or_default(),
                secondary: index > 0,
            };
            if !rows.iter().any(|r| {
                r.id == row.id && r.installed == row.installed && r.available == row.available
            }) {
                rows.push(row);
            }
        }
    }
    rows
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::char_width;

    fn width(text: &str) -> usize {
        text.chars().map(char_width).sum()
    }

    /// Lays a table out exactly like the package manager does: every column
    /// as wide as its widest cell (measured in display cells), one space
    /// between columns, a dashed rule as long as the table.
    pub(crate) fn render(header: &[&str], rows: &[Vec<&str>]) -> String {
        let columns = header.len();
        let mut widths: Vec<usize> = header.iter().map(|h| width(h)).collect();
        for row in rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width(cell));
            }
        }
        let line = |cells: &[&str]| -> String {
            let mut out = String::new();
            for (i, cell) in cells.iter().enumerate() {
                out.push_str(cell);
                if i + 1 < columns {
                    out.push_str(&" ".repeat(widths[i] - width(cell) + 1));
                }
            }
            out.trim_end().to_owned()
        };
        let total: usize = widths.iter().sum::<usize>() + columns - 1;
        let mut out = String::new();
        out.push_str(&line(header));
        out.push_str("\r\n");
        out.push_str(&"-".repeat(total));
        out.push_str("\r\n");
        for row in rows {
            out.push_str(&line(row));
            out.push_str("\r\n");
        }
        out
    }

    pub(crate) const HEADER: [&str; 5] = ["Name", "Id", "Version", "Available", "Source"];

    /// A listing in the exact shape captured from a real run (clean output,
    /// version cell containing spaces, ids containing `+` and `-`).
    pub(crate) fn typical() -> String {
        let mut out = render(
            &HEADER,
            &[
                vec!["7-Zip 23.01 (x64)", "7zip.7zip", "23.01", "24.09", "winget"],
                vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
                vec![
                    "Microsoft Visual C++ v14 Redistributable (x64) - 14.51.36247",
                    "Microsoft.VCRedist.2015+.x64",
                    "14.51.36247.0",
                    "14.52.37029.0",
                    "winget",
                ],
                vec![
                    "Notepad++ (64-bit x64)",
                    "Notepad++.Notepad++",
                    "8.8.4",
                    "8.9.9.1",
                    "winget",
                ],
                vec![
                    "Visual Studio Community 2022",
                    "Microsoft.VisualStudio.2022.Community",
                    "17.14.13 (August 2025)",
                    "17.14.45",
                    "winget",
                ],
                vec![
                    "Podman Desktop 1.19.2",
                    "RedHat.Podman-Desktop",
                    "1.19.2",
                    "1.30.5",
                    "winget",
                ],
                vec![
                    "Spotify - Music and Podcasts",
                    "9NCBCSZSJRSB",
                    "1.2.50.335",
                    "1.2.74.477",
                    "msstore",
                ],
            ],
        );
        out.push_str("7 upgrades available.\r\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{HEADER, render, typical};
    use super::*;

    fn row<'a>(rows: &'a [UpgradeRow], id: &str) -> &'a UpgradeRow {
        rows.iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("no row with id {id}: {rows:#?}"))
    }

    #[test]
    fn typical_listing() {
        let rows = parse_upgrade_rows(&typical());
        assert_eq!(rows.len(), 7, "{rows:#?}");
        let git = row(&rows, "Git.Git");
        assert_eq!(
            (
                git.name.as_str(),
                git.installed.as_str(),
                git.available.as_str()
            ),
            ("Git", "2.53.0", "2.56.0.4")
        );
        assert_eq!(git.source, "winget");
        assert!(!git.secondary);
        // A version containing spaces stays in its own column.
        let vs = row(&rows, "Microsoft.VisualStudio.2022.Community");
        assert_eq!(vs.installed, "17.14.13 (August 2025)");
        assert_eq!(vs.available, "17.14.45");
        // Punctuation in ids and long names.
        let vc = row(&rows, "Microsoft.VCRedist.2015+.x64");
        assert_eq!(
            vc.name,
            "Microsoft Visual C++ v14 Redistributable (x64) - 14.51.36247"
        );
        assert_eq!(row(&rows, "Notepad++.Notepad++").available, "8.9.9.1");
        assert_eq!(row(&rows, "9NCBCSZSJRSB").source, "msstore");
    }

    #[test]
    fn literal_capture_shape() {
        // Verbatim layout of a real listing (trimmed to two rows).
        let raw = "Name                 Id                    Version Available Source\r\n\
                   -------------------------------------------------------------------\r\n\
                   Git                  Git.Git               2.53.0  2.56.0.4  winget\r\n\
                   Rustup: the Rust ... Rustlang.Rustup       1.28.2  1.29.3    winget\r\n\
                   2 upgrades available.\r\n";
        let rows = parse_upgrade_rows(raw);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(row(&rows, "Rustlang.Rustup").name, "Rustup: the Rust ...");
    }

    #[test]
    fn footers_and_notes_are_not_rows() {
        let mut raw = typical();
        raw.push_str("3 package(s) have version numbers that cannot be determined. Use --include-unknown to see all results.\r\n");
        raw.push_str("1 package(s) have pins that prevent upgrade. Use the 'winget pin' command to view and edit pins. Using the --include-pinned argument may show more results.\r\n");
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 7, "{rows:#?}");
    }

    #[test]
    fn nothing_to_upgrade() {
        assert!(parse_upgrade_rows("").is_empty());
        assert!(
            parse_upgrade_rows("No installed package found matching input criteria.\r\n")
                .is_empty()
        );
        // A header without rows.
        assert!(parse_upgrade_rows(&render(&HEADER, &[])).is_empty());
    }

    #[test]
    fn second_table_is_marked_secondary() {
        let mut raw = typical();
        raw.push_str("The following packages have an upgrade available, but require explicit targeting for upgrade:\r\n");
        raw.push_str(&render(
            &HEADER,
            &[vec![
                "Discord",
                "Discord.Discord",
                "1.0.9035",
                "1.0.9172",
                "winget",
            ]],
        ));
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 8, "{rows:#?}");
        assert!(row(&rows, "Discord.Discord").secondary);
        assert!(!row(&rows, "Git.Git").secondary);
        // The second header line must not have been read as a row of the first table.
        assert!(rows.iter().all(|r| r.id != "Id"));
    }

    #[test]
    fn spinner_and_vt_noise_is_removed() {
        let table = typical();
        let noisy = format!(
            "\u{1b}[?25l\r   - \r   \\ \r   | \r   / \r      \r{}\u{1b}[?25h",
            table.replace("Git.Git", "\u{1b}[1mGit.Git\u{1b}[0m")
        );
        let rows = parse_upgrade_rows(&noisy);
        assert_eq!(rows.len(), 7, "{rows:#?}");
        assert_eq!(row(&rows, "Git.Git").available, "2.56.0.4");

        // Spinner frames and a download bar on lines of their own.
        let noisy = format!(
            "   -\r\n   \\\r\n  \u{2588}\u{2588}\u{2588}\u{2592}\u{2592}  1.00 MB / 5.00 MB\r\n{table}"
        );
        assert_eq!(parse_upgrade_rows(&noisy).len(), 7);

        // OSC title sequences and a byte order mark.
        let noisy = format!("\u{feff}\u{1b}]0;winget\u{7}{table}");
        assert_eq!(parse_upgrade_rows(&noisy).len(), 7);
    }

    #[test]
    fn carriage_returns_are_replayed() {
        assert_eq!(render_line("abc\rX"), "Xbc");
        assert_eq!(render_line("  - \r  \\ \r     \rName  Id"), "Name  Id");
        assert_eq!(render_line("abc\u{8}\u{8}Z"), "aZc");
        assert_eq!(render_line("trailing   \r"), "trailing");
    }

    #[test]
    fn wide_characters_are_measured_in_cells() {
        // CJK names are two cells per character; padding is by cell.
        let raw = render(
            &HEADER,
            &[
                vec!["微信", "Tencent.WeChat", "3.9.10.19", "3.9.12.51", "winget"],
                vec![
                    "网易云音乐",
                    "NetEase.CloudMusic",
                    "2.10.13",
                    "3.0.9",
                    "winget",
                ],
                vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
                vec!["LINE（ライン）", "LINE.LINE", "8.4.0", "9.5.0", "winget"],
            ],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 4, "{rows:#?}");
        assert_eq!(row(&rows, "Tencent.WeChat").name, "微信");
        assert_eq!(row(&rows, "NetEase.CloudMusic").installed, "2.10.13");
        assert_eq!(row(&rows, "LINE.LINE").name, "LINE（ライン）");
        assert_eq!(row(&rows, "Git.Git").available, "2.56.0.4");
    }

    #[test]
    fn translated_headers_work_by_position() {
        let german = render(
            &["Name", "ID", "Version", "Verfügbar", "Quelle"],
            &[vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"]],
        );
        let japanese = render(
            &["名前", "ID", "バージョン", "利用可能", "ソース"],
            &[vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"]],
        );
        let russian = render(
            &["Имя", "ИД", "Версия", "Доступно", "Источник"],
            &[vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"]],
        );
        for raw in [german, japanese, russian] {
            let rows = parse_upgrade_rows(&raw);
            assert_eq!(rows.len(), 1, "{raw}");
            assert_eq!(rows[0].id, "Git.Git");
            assert_eq!(rows[0].available, "2.56.0.4");
            assert_eq!(rows[0].source, "winget");
        }
    }

    #[test]
    fn header_label_containing_a_space() {
        let raw = render(
            &["Nom du paquet", "ID", "Version", "Disponible", "Source"],
            &[
                vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
                vec![
                    "Mozilla Firefox (x64 fr)",
                    "Mozilla.Firefox.fr",
                    "131.0",
                    "133.0",
                    "winget",
                ],
            ],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(
            row(&rows, "Mozilla.Firefox.fr").name,
            "Mozilla Firefox (x64 fr)"
        );
    }

    #[test]
    fn multi_word_labels_in_any_column() {
        let rows_data = vec![
            vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
            vec![
                "Visual Studio Community 2022",
                "Microsoft.VisualStudio.2022.Community",
                "17.14.13 (August 2025)",
                "17.14.45",
                "winget",
            ],
        ];
        let headers: [[&str; 5]; 4] = [
            ["Název", "ID", "Verze", "K dispozici", "Zdroj"],
            ["이름", "ID", "버전", "사용 가능", "원본"],
            ["Nom du paquet", "ID", "Version", "Disponible", "Source"],
            [
                "Name",
                "Package Id",
                "Installed Version",
                "Available Version",
                "Package Source",
            ],
        ];
        for header in headers {
            // Many rows, and each row on its own (the hardest case: nothing
            // but one line to tell label words from columns).
            let mut variants = vec![rows_data.clone()];
            variants.extend(rows_data.iter().map(|row| vec![row.clone()]));
            for data in variants {
                let mut raw = render(&header, &data);
                raw.push_str("2 upgrades available.\r\n");
                let rows = parse_upgrade_rows(&raw);
                assert_eq!(
                    rows.len(),
                    data.len(),
                    "header {header:?}\n{raw}\n{rows:#?}"
                );
                for (parsed, expected) in rows.iter().zip(&data) {
                    assert_eq!(
                        [
                            parsed.name.as_str(),
                            parsed.id.as_str(),
                            parsed.installed.as_str(),
                            parsed.available.as_str(),
                            parsed.source.as_str()
                        ],
                        expected[..],
                        "header {header:?}\n{raw}"
                    );
                }
                let tables = parse_tables(&clean_lines(&raw));
                assert_eq!(tables[0].header, header, "{raw}");
            }
        }
    }

    #[test]
    fn one_character_cells_under_a_multi_word_label() {
        let raw = render(
            &["Název", "ID", "Verze", "K dispozici", "Zdroj"],
            &[vec!["Tool", "Vendor.Tool", "4", "5", "winget"]],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 1, "{raw}\n{rows:#?}");
        assert_eq!(
            (rows[0].installed.as_str(), rows[0].available.as_str()),
            ("4", "5")
        );
        assert_eq!(rows[0].source, "winget");
    }

    #[test]
    fn name_words_aligned_with_label_words_do_not_confuse_columns() {
        // "Git Extensions" breaks exactly where "Nom du paquet" does.
        let raw = render(
            &["Nom du paquet", "ID", "Version", "Disponible", "Source"],
            &[vec![
                "Git Extensions",
                "GitExtensionsTeam.GitExtensions",
                "4.2.1",
                "5.1.1",
                "winget",
            ]],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 1, "{raw}\n{rows:#?}");
        assert_eq!(rows[0].name, "Git Extensions");
        assert_eq!(rows[0].id, "GitExtensionsTeam.GitExtensions");
        assert_eq!(rows[0].available, "5.1.1");
    }

    #[test]
    fn combinations_are_complete_and_ordered() {
        let mut seen = Vec::new();
        for_each_combination(&[1, 2, 3, 4], 2, &mut |c| seen.push(c.to_vec()));
        assert_eq!(seen, [[1, 2], [1, 3], [1, 4], [2, 3], [2, 4], [3, 4]]);
        let mut count = 0;
        for_each_combination(&[1, 2, 3], 3, &mut |_| count += 1);
        for_each_combination(&[1, 2, 3], 4, &mut |_| count += 100);
        for_each_combination(&[], 0, &mut |c| count += 10 + c.len());
        assert_eq!(count, 11);
    }

    #[test]
    fn narrow_columns_separated_by_one_space() {
        // Every cell exactly as wide as its header: single spaces only.
        let raw = render(
            &HEADER,
            &[vec!["Tool", "ab", "1.0.0.0", "1.0.0.100", "winget"]],
        );
        assert!(raw.starts_with("Name Id Version Available Source"));
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(rows[0].id, "ab");
        assert_eq!(rows[0].available, "1.0.0.100");
    }

    #[test]
    fn missing_source_column() {
        let raw = render(
            &["Name", "Id", "Version", "Available"],
            &[vec!["Git", "Git.Git", "2.53.0", "2.56.0.4"]],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "");
        assert_eq!(rows[0].available, "2.56.0.4");
    }

    #[test]
    fn unknown_and_lower_than_versions() {
        let raw = render(
            &HEADER,
            &[
                vec!["Some Tool", "Vendor.SomeTool", "Unknown", "4.2.0", "winget"],
                vec![
                    "Other Tool",
                    "Vendor.OtherTool",
                    "< 5.0.1",
                    "5.0.1",
                    "winget",
                ],
            ],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(row(&rows, "Vendor.SomeTool").installed, "Unknown");
        assert_eq!(row(&rows, "Vendor.OtherTool").installed, "< 5.0.1");
    }

    #[test]
    fn truncated_cells_keep_their_ellipsis() {
        // What a narrow console produces: cells cut short with an ellipsis.
        let raw = render(
            &HEADER,
            &[
                vec![
                    "Microsoft Visual C++ 2015-202\u{2026}",
                    "Microsoft.VCRedist.2015+.x64",
                    "14.36.32532\u{2026}",
                    "14.38.33135\u{2026}",
                    "winget",
                ],
                vec![
                    "Visual Studio Community 2022",
                    "Microsoft.VisualStudio.2022\u{2026}",
                    "17.14.13",
                    "17.14.45",
                    "winget",
                ],
            ],
        );
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert!(rows[0].name.ends_with(ELLIPSIS));
        assert_eq!(rows[0].id, "Microsoft.VCRedist.2015+.x64");
        assert!(rows[0].installed.ends_with(ELLIPSIS));
        assert_eq!(rows[1].id, "Microsoft.VisualStudio.2022\u{2026}");
    }

    #[test]
    fn rows_resynchronise_after_width_disagreement() {
        // Lay the table out as if "®" were two cells wide (the package
        // manager's opinion), while this parser measures it as one.
        let mut raw = render(
            &HEADER,
            &[
                vec![
                    "Intel\u{ae}\u{ae} Graphics Command Center",
                    "Intel.GraphicsCommandCenter",
                    "1.100.5688.0",
                    "1.100.6002.0",
                    "winget",
                ],
                vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
            ],
        );
        // Remove one "®": that row's later columns now sit one cell to the left.
        raw = raw.replacen("Intel\u{ae}\u{ae}", "Intel\u{ae}", 1);
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        let intel = row(&rows, "Intel.GraphicsCommandCenter");
        assert_eq!(intel.name, "Intel\u{ae} Graphics Command Center");
        assert_eq!(intel.installed, "1.100.5688.0");
        assert_eq!(intel.available, "1.100.6002.0");
        assert_eq!(intel.source, "winget");
    }

    #[test]
    fn installed_listing_with_empty_cells_yields_only_upgradable_rows() {
        // The "list" command: Available and Source are often empty, and
        // locally discovered programs have ids containing spaces.
        let raw = render(
            &HEADER,
            &[
                vec![
                    "App Installer",
                    "Microsoft.AppInstaller",
                    "1.29.380.0",
                    "",
                    "winget",
                ],
                vec![
                    "Some Client Components (All Users)",
                    "ARP\\Machine\\X86\\Some Client Components",
                    "72.2024.1010.1953",
                    "",
                    "",
                ],
                vec![
                    "AV1 Video Extension",
                    "MSIX\\Microsoft.AV1VideoExtension_2.0.35.0_x64__8wekyb3d8bbwe",
                    "2.0.35.0",
                    "",
                    "",
                ],
                vec!["Git", "Git.Git", "2.53.0", "2.56.0.4", "winget"],
            ],
        );
        let tables = parse_tables(&clean_lines(&raw));
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows.len(), 4, "{:#?}", tables[0].rows);
        assert_eq!(
            tables[0].rows[1][1],
            "ARP\\Machine\\X86\\Some Client Components"
        );
        assert_eq!(tables[0].rows[0][3], "");
        let rows = parse_upgrade_rows(&raw);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "Git.Git");
    }

    #[test]
    fn duplicate_rows_collapse() {
        let raw = render(
            &HEADER,
            &[
                vec!["Tool", "Vendor.Tool", "1.0", "2.0", "winget"],
                vec!["Tool", "Vendor.Tool", "1.0", "2.0", "winget"],
                vec!["Tool (user)", "Vendor.Tool", "1.5", "2.0", "winget"],
            ],
        );
        assert_eq!(parse_upgrade_rows(&raw).len(), 2);
    }

    #[test]
    fn unrelated_dashes_and_tables_are_ignored() {
        // A dashed line with nothing above it, and a two-column table.
        let raw = "\r\n----------------------\r\nsome text\r\n\r\nName   Value\r\n-------------\r\nalpha  1\r\n";
        assert!(parse_upgrade_rows(raw).is_empty());
        let tables = parse_tables(&clean_lines(raw));
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].header, vec!["Name", "Value"]);
        assert_eq!(
            tables[0].rows,
            vec![vec!["alpha".to_owned(), "1".to_owned()]]
        );
    }

    #[test]
    fn garbage_never_panics() {
        for raw in [
            "\u{1b}",
            "\u{1b}[",
            "\u{1b}]unterminated",
            "\r\r\r",
            "-\n--\n---",
            "Name\n----------\n",
            "\u{8}\u{8}\u{8}x",
            "a\u{0}b\u{7f}c",
        ] {
            let _ = parse_upgrade_rows(raw);
        }
    }
}
