//! The analysis pass: works out exactly what a clean would remove.
//!
//! Scanning never modifies anything. Its result is the precise list of
//! files, registry entries and operations that [`crate::clean`] will act on,
//! so what the user reviews is what gets removed.

use crate::model::{
    Action, Catalog, CleanOptions, Finding, Group, Rule, RuleResult, ScanReport, Target,
};
use crate::pattern::NamePattern;
use crate::special::{self, RunContext};
use rayon::prelude::*;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zest_core::fastdir::{self, RawEntry};
use zest_core::walk;
use zest_core::{Progress, safety};

/// Analyses every enabled rule of `catalog`.
///
/// Rules run in parallel, and so do the folders inside a rule, all on the
/// shared walk pool of [`zest_core::fastdir`]. Cancelling through `progress`
/// returns the results gathered so far.
pub fn scan(catalog: &Catalog, opts: &CleanOptions, progress: &Progress) -> ScanReport {
    let started = Instant::now();
    let ctx = RunContext::new(opts);
    let jobs: Vec<(&Group, &Rule)> = catalog
        .rules()
        .filter(|(_, rule)| opts.enabled.contains(&rule.id))
        .collect();
    progress.set_total(jobs.len() as u64);

    let results: Vec<RuleResult> = fastdir::pool().install(|| {
        jobs.par_iter()
            .filter_map(|(group, rule)| {
                if progress.is_cancelled() {
                    return None;
                }
                progress.set_message(format!("{} – {}", group.name, rule.name));
                let result = scan_rule(group, rule, &ctx, progress);
                progress.inc(1);
                progress.add_bytes(result.bytes);
                (!result.is_empty()).then_some(result)
            })
            .collect()
    });

    let mut report = ScanReport {
        bytes: results.iter().map(|r| r.bytes).sum(),
        items: results.iter().map(|r| r.items).sum(),
        results,
        elapsed: started.elapsed(),
    };
    // Restore catalog order (parallel collection keeps it, but be explicit:
    // the UI relies on it).
    let order: Vec<&str> = catalog.rules().map(|(_, r)| r.id.as_str()).collect();
    report.results.sort_by_key(|r| {
        order
            .iter()
            .position(|id| *id == r.rule_id)
            .unwrap_or(usize::MAX)
    });
    report
}

/// Display name of the program that blocks `group`, if one is running.
pub(crate) fn blocker(group: &Group, ctx: &RunContext) -> Option<String> {
    group
        .processes
        .iter()
        .any(|p| ctx.running.contains(&p.to_lowercase()))
        .then(|| format!("{} is running", group.name))
}

fn scan_rule(group: &Group, rule: &Rule, ctx: &RunContext, progress: &Progress) -> RuleResult {
    let mut result = RuleResult {
        rule_id: rule.id.clone(),
        group_id: group.id.clone(),
        // When the caller chose to ignore running programs, nothing is blocked.
        blocked_by: if ctx.opts.ignore_running {
            None
        } else {
            blocker(group, ctx)
        },
        needs_admin: rule.requires_admin && !ctx.elevated,
        ..Default::default()
    };
    for action in &rule.actions {
        if progress.is_cancelled() {
            break;
        }
        scan_action(action, rule, ctx, progress, &mut result);
    }
    result
}

fn scan_action(
    action: &Action,
    rule: &Rule,
    ctx: &RunContext,
    progress: &Progress,
    out: &mut RuleResult,
) {
    match action {
        Action::Files {
            dir,
            pattern,
            recursive,
            min_age_days,
        } => {
            let mut min_age = Duration::from_secs(u64::from(*min_age_days) * 86_400);
            if rule.age_filtered {
                min_age = min_age.max(Duration::from_secs(
                    u64::from(ctx.opts.min_age_hours) * 3600,
                ));
            }
            scan_files(
                dir,
                &NamePattern::new(pattern),
                *recursive,
                min_age,
                ctx,
                progress,
                out,
            );
        }
        Action::File(path) => {
            if let Ok(meta) = std::fs::symlink_metadata(path)
                && !meta.is_dir()
                && allowed(path, ctx)
            {
                out.push(Finding::file(path.clone(), entry_size(&meta)));
            }
        }
        Action::Folder(path) => scan_folder(path, ctx, progress, out),
        Action::RegValues {
            key,
            include_subkeys,
        } => {
            if ctx.excluder.excludes_key(key) {
                return;
            }
            let Ok(opened) = key.open() else {
                return;
            };
            for (name, _) in opened.enum_values().flatten() {
                out.push(Finding {
                    target: Target::RegValue {
                        key: key.clone(),
                        name,
                    },
                    size: 0,
                    items: 1,
                    note: None,
                });
            }
            if *include_subkeys {
                for sub in opened.enum_keys().flatten() {
                    let child = key.child(&sub);
                    if !ctx.excluder.excludes_key(&child) {
                        out.push(Finding {
                            target: Target::RegKey(child),
                            size: 0,
                            items: 1,
                            note: None,
                        });
                    }
                }
            }
        }
        Action::RegValue { key, name } => {
            if !ctx.excluder.excludes_key(key)
                && key.open().is_ok_and(|k| k.get_raw_value(name).is_ok())
            {
                out.push(Finding {
                    target: Target::RegValue {
                        key: key.clone(),
                        name: name.clone(),
                    },
                    size: 0,
                    items: 1,
                    note: None,
                });
            }
        }
        Action::RegKey(key) => {
            if !ctx.excluder.excludes_key(key) && key.exists() {
                out.push(Finding {
                    target: Target::RegKey(key.clone()),
                    size: 0,
                    items: 1,
                    note: None,
                });
            }
        }
        Action::Special(special) => special::scan(special, ctx, out),
    }
}

fn allowed(path: &Path, ctx: &RunContext) -> bool {
    !ctx.excluder.excludes_path(path) && !safety::is_protected(path)
}

/// Logical size of a file; links take no space of their own.
fn entry_size(meta: &Metadata) -> u64 {
    if walk::is_link(meta) { 0 } else { meta.len() }
}

/// Decides which entries of a folder a rule takes.
struct FileFilter<'a> {
    pattern: &'a NamePattern,
    min_age: Duration,
    ctx: &'a RunContext<'a>,
}

impl FileFilter<'_> {
    fn old_enough(&self, entry: &RawEntry<'_>) -> bool {
        if self.min_age.is_zero() {
            return true;
        }
        match entry.modified_time() {
            Some(modified) => self
                .ctx
                .now
                .duration_since(modified)
                .is_ok_and(|age| age >= self.min_age),
            // Without a timestamp we cannot prove the file is old: leave it.
            None => false,
        }
    }

    /// The finding for `entry` (a file or a link inside `dir`), if the rule
    /// wants it. The path is only built for entries that pass the cheap
    /// name and age tests.
    fn accept(&self, dir: &Path, entry: &RawEntry<'_>) -> Option<Finding> {
        let name_matches = self.pattern.matches_all()
            || self.pattern.matches(&String::from_utf16_lossy(entry.name));
        if !name_matches || !self.old_enough(entry) {
            return None;
        }
        let path = entry.path_in(dir);
        // Links take no space of their own.
        let size = if entry.is_link() { 0 } else { entry.size };
        allowed(&path, self.ctx).then(|| Finding::file(path, size))
    }
}

/// Everything the filter accepts below `dir`: its own entries first, then
/// each sub-folder in listing order, so the result does not depend on how
/// the work was spread over threads. Links are offered to the filter as
/// entries and never descended into.
fn collect_tree(dir: &Path, filter: &FileFilter<'_>, progress: &Progress) -> Vec<Finding> {
    if progress.is_cancelled() {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    // Unreadable folders are skipped silently.
    let _ = fastdir::read_dir_raw(dir, &mut |entry| {
        if entry.is_dir() && !entry.is_link() {
            subdirs.push(entry.path_in(dir));
        } else if let Some(finding) = filter.accept(dir, entry) {
            found.push(finding);
        }
    });
    if subdirs.len() < 2 {
        for sub in &subdirs {
            found.extend(collect_tree(sub, filter, progress));
        }
    } else {
        let nested: Vec<Vec<Finding>> = subdirs
            .par_iter()
            .map(|sub| collect_tree(sub, filter, progress))
            .collect();
        found.reserve(nested.iter().map(Vec::len).sum());
        for part in nested {
            found.extend(part);
        }
    }
    found
}

fn scan_files(
    dir: &Path,
    pattern: &NamePattern,
    recursive: bool,
    min_age: Duration,
    ctx: &RunContext,
    progress: &Progress,
    out: &mut RuleResult,
) {
    // The folder itself must be a real folder: never descend through a link.
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() && !walk::is_link(&meta) => {}
        _ => return,
    }
    if ctx.excluder.excludes_path(dir) {
        return;
    }
    let filter = FileFilter {
        pattern,
        min_age,
        ctx,
    };

    if recursive {
        for finding in collect_tree(dir, &filter, progress) {
            out.push(finding);
        }
        if !out.prune_roots.iter().any(|p| p == dir) {
            out.prune_roots.push(dir.to_path_buf());
        }
    } else {
        let _ = fastdir::read_dir_raw(dir, &mut |entry| {
            if (!entry.is_dir() || entry.is_link())
                && let Some(finding) = filter.accept(dir, entry)
            {
                out.push(finding);
            }
        });
    }
}

fn scan_folder(path: &Path, ctx: &RunContext, progress: &Progress, out: &mut RuleResult) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !allowed(path, ctx) {
        return;
    }
    if !meta.is_dir() || walk::is_link(&meta) {
        out.push(Finding::file(path.to_path_buf(), entry_size(&meta)));
        return;
    }
    if ctx.excluder.touches_dir(path) {
        // Something inside is excluded: fall back to file-by-file cleaning.
        scan_files(
            path,
            &NamePattern::new("*"),
            true,
            Duration::ZERO,
            ctx,
            progress,
            out,
        );
        return;
    }
    let size = walk::tree_size(path, Some(progress));
    out.push(Finding {
        target: Target::Dir(path.to_path_buf()),
        size: size.bytes,
        items: size.files.max(1),
        note: None,
    });
}
