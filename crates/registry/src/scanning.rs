//! Running scanners and summarising their results.

use crate::scanners;
use crate::types::{Category, Issue, ScanOptions};
use rayon::prelude::*;
use std::collections::HashSet;
use zest_core::{Progress, Result};

/// Scans the registry for invalid entries. **Read-only.**
///
/// The selected categories run in parallel. `progress` counts finished
/// categories (`done` / `total`) and names the one that finished last.
/// Cancelling `progress` stops the scanners at their next checkpoint and
/// makes this function return [`zest_core::Error::Cancelled`].
///
/// The result is sorted by category (display order) and then by location,
/// contains each entry at most once, and already has
/// [`ScanOptions::excluded`] applied.
pub fn scan(options: &ScanOptions, progress: &Progress) -> Result<Vec<Issue>> {
    let mut seen = HashSet::new();
    let categories: Vec<Category> = options
        .categories
        .iter()
        .copied()
        .filter(|c| seen.insert(*c))
        .collect();

    progress.set_done(0);
    progress.set_total(categories.len() as u64);
    progress.set_message("Scanning the registry");

    let per_category: Vec<Vec<Issue>> = categories
        .par_iter()
        .map(|&category| {
            if progress.is_cancelled() {
                return Vec::new();
            }
            let started = std::time::Instant::now();
            let found = scanners::run(category, progress);
            log::debug!(
                "registry scan: {} -> {} issue(s) in {:?}",
                category.slug(),
                found.len(),
                started.elapsed()
            );
            progress.inc(1);
            progress.set_message(format!("Checked {}", category.label().to_lowercase()));
            found
        })
        .collect();
    progress.check()?;

    let mut ids = HashSet::new();
    let mut issues: Vec<Issue> = per_category
        .into_iter()
        .flatten()
        .filter(|issue| !options.is_excluded(issue))
        .filter(|issue| ids.insert(issue.id.clone()))
        .collect();
    issues.sort_by(|a, b| (a.category, &a.key, &a.value).cmp(&(b.category, &b.key, &b.value)));
    progress.set_message(format!("Found {} registry issue(s)", issues.len()));
    Ok(issues)
}

/// Number of issues per category, for every category in display order
/// (categories without issues are included with a count of zero).
pub fn count_by_category(issues: &[Issue]) -> Vec<(Category, usize)> {
    Category::ALL
        .into_iter()
        .map(|category| {
            (
                category,
                issues.iter().filter(|i| i.category == category).count(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Confidence;
    use zest_core::Error;
    use zest_core::reg::RegPath;

    fn issue(category: Category, key: &str) -> Issue {
        Issue::for_key(
            category,
            &RegPath::hkcu(key),
            "t",
            "d",
            Confidence::Safe,
            vec![],
        )
    }

    #[test]
    fn counts_cover_every_category() {
        let issues = vec![
            issue(Category::Fonts, r"Software\A"),
            issue(Category::Startup, r"Software\B"),
            issue(Category::Fonts, r"Software\C"),
        ];
        let counts = count_by_category(&issues);
        assert_eq!(counts.len(), Category::ALL.len());
        assert_eq!(
            counts.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            Category::ALL.to_vec()
        );
        let get = |c: Category| counts.iter().find(|(cat, _)| *cat == c).unwrap().1;
        assert_eq!(get(Category::Fonts), 2);
        assert_eq!(get(Category::Startup), 1);
        assert_eq!(get(Category::Services), 0);
        assert_eq!(counts.iter().map(|(_, n)| n).sum::<usize>(), 3);
    }

    #[test]
    fn empty_selection_scans_nothing() {
        let progress = Progress::new();
        let found = scan(&ScanOptions::only(&[]), &progress).unwrap();
        assert!(found.is_empty());
        assert_eq!(progress.total(), 0);
    }

    #[test]
    fn cancelled_scan_reports_cancellation() {
        let progress = Progress::new();
        progress.cancel();
        let result = scan(&ScanOptions::all(), &progress);
        assert!(matches!(result, Err(Error::Cancelled)));
    }

    /// Read-only smoke test against the real registry: the scan must finish,
    /// be internally consistent, and never offer a structural key.
    #[test]
    fn real_scan_is_consistent() {
        let progress = Progress::new();
        let options = ScanOptions::all();
        let issues = scan(&options, &progress).unwrap();
        assert_eq!(progress.done(), options.categories.len() as u64);

        let mut ids = HashSet::new();
        for issue in &issues {
            assert!(ids.insert(issue.id.clone()), "duplicate id {}", issue.id);
            assert!(options.categories.contains(&issue.category));
            assert!(
                !issue.title.is_empty() && !issue.detail.is_empty(),
                "{issue:?}"
            );
            assert!(!issue.conditions.is_empty(), "{issue:?}");
            assert!(issue.key.depth() >= 2, "{issue:?}");
            if issue.value.is_none() {
                assert!(
                    !zest_core::reg::is_protected_key(&issue.key),
                    "structural key offered: {}",
                    issue.key
                );
            }
            if issue.category == Category::Services {
                assert_eq!(issue.confidence, Confidence::Review);
            }
        }
        let sorted = issues.windows(2).all(|w| {
            (w[0].category, &w[0].key, &w[0].value) <= (w[1].category, &w[1].key, &w[1].value)
        });
        assert!(sorted, "issues are not sorted");

        // Excluding everything under both hives leaves nothing.
        let none = scan(
            &ScanOptions {
                categories: options.categories.clone(),
                excluded: vec!["HKLM\\".to_owned(), "HKCU\\".to_owned()],
            },
            &Progress::new(),
        )
        .unwrap();
        assert!(none.is_empty());
    }
}
