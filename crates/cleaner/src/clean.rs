//! The cleaning pass: removes exactly what a scan reported.

use crate::model::{
    Catalog, CleanOptions, CleanReport, MAX_REPORTED_FAILURES, RuleCleanResult, RuleResult,
    ScanReport, Target,
};
use crate::scan::blocker;
use crate::special::{self, RunContext};
use rayon::prelude::*;
use std::time::Instant;
use zest_core::delete::{self, DeleteOptions, DeleteReport};
use zest_core::{Error, Progress, reg};

const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;
const ERROR_USER_MAPPED_FILE: i32 = 1224;

/// True when the failure means "another program is using this right now".
fn is_in_use(error: &Error) -> bool {
    match error {
        Error::Io { source, .. } => matches!(
            source.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_USER_MAPPED_FILE)
        ),
        _ => false,
    }
}

fn describe(error: &Error, elevated: bool) -> String {
    if error.is_access_denied() && !elevated {
        "Access denied – restart as administrator to remove this item".to_owned()
    } else {
        error.to_string()
    }
}

/// Removes everything listed in `scan`.
///
/// * Rules whose program is running are skipped unless
///   [`CleanOptions::ignore_running`] is set (the check is repeated now, so
///   closing the program after the scan is enough).
/// * Items that vanished since the scan are ignored.
/// * Items in use by another program are counted in `in_use`, not as failures.
/// * With `dry_run` nothing is touched; the report shows what would happen.
pub fn clean(
    catalog: &Catalog,
    scan: &ScanReport,
    opts: &CleanOptions,
    dry_run: bool,
    progress: &Progress,
) -> CleanReport {
    let started = Instant::now();
    let ctx = RunContext::new(opts);
    let total: u64 = scan.results.iter().map(|r| r.findings.len() as u64).sum();
    progress.set_total(total);

    let outcomes: Vec<(RuleCleanResult, Vec<(String, String)>)> = scan
        .results
        .par_iter()
        .map(|result| clean_rule(catalog, result, &ctx, dry_run, progress))
        .collect();

    let mut report = CleanReport {
        dry_run,
        ..Default::default()
    };
    for (rule, failures) in outcomes {
        report.bytes += rule.bytes;
        report.items += rule.items;
        report.in_use += rule.in_use;
        report.failed += rule.failed;
        for failure in failures {
            if report.failures.len() < MAX_REPORTED_FAILURES {
                report.failures.push(failure);
            }
        }
        report.rules.push(rule);
    }
    report.elapsed = started.elapsed();
    report
}

/// Books a failed item: in-use items are counted separately, items that are
/// already gone are ignored, everything else is a failure with a message.
fn record(
    out: &mut RuleCleanResult,
    failures: &mut Vec<(String, String)>,
    what: String,
    error: &Error,
    elevated: bool,
) {
    if is_in_use(error) {
        out.in_use += 1;
    } else if !error.is_not_found() {
        out.failed += 1;
        if failures.len() < MAX_REPORTED_FAILURES {
            failures.push((what, describe(error, elevated)));
        }
    }
}

fn clean_rule(
    catalog: &Catalog,
    result: &RuleResult,
    ctx: &RunContext,
    dry_run: bool,
    progress: &Progress,
) -> (RuleCleanResult, Vec<(String, String)>) {
    let mut out = RuleCleanResult {
        rule_id: result.rule_id.clone(),
        group_id: result.group_id.clone(),
        ..Default::default()
    };
    let mut failures: Vec<(String, String)> = Vec::new();

    let group = catalog.groups.iter().find(|g| g.id == result.group_id);
    if let Some(group) = group {
        progress.set_message(format!("Cleaning {}", group.name));
        if !ctx.opts.ignore_running
            && let Some(blocked) = blocker(group, ctx)
        {
            out.skipped = Some(blocked);
            progress.inc(result.findings.len() as u64);
            return (out, failures);
        }
    }

    let delete_opts = DeleteOptions::secure(ctx.opts.wipe).with_dry_run(dry_run);

    for finding in &result.findings {
        if progress.is_cancelled() {
            break;
        }
        match &finding.target {
            Target::File(path) => match delete::delete_file(path, &delete_opts) {
                Ok(bytes) => {
                    out.bytes += bytes;
                    out.items += finding.items;
                    progress.add_bytes(bytes);
                }
                Err(e) => record(
                    &mut out,
                    &mut failures,
                    path.display().to_string(),
                    &e,
                    ctx.elevated,
                ),
            },
            Target::Dir(path) => {
                let mut tree = DeleteReport::default();
                delete::delete_tree(path, false, &delete_opts, Some(progress), &mut tree);
                out.bytes += tree.bytes;
                out.items += tree.files;
                progress.add_bytes(tree.bytes);
                for (failed_path, message) in &tree.failures {
                    // Classify by message: the tree report carries text only.
                    let in_use = message.contains("os error 32") || message.contains("os error 33");
                    if in_use {
                        out.in_use += 1;
                    } else {
                        out.failed += 1;
                        if failures.len() < MAX_REPORTED_FAILURES {
                            failures.push((failed_path.display().to_string(), message.clone()));
                        }
                    }
                }
                let recorded = tree.failures.len() as u64;
                out.failed += tree.failed.saturating_sub(recorded);
            }
            Target::RegValue { key, name } => {
                if dry_run {
                    out.items += 1;
                } else {
                    match reg::delete_value(key, name, None) {
                        Ok(()) => out.items += 1,
                        Err(e) => record(
                            &mut out,
                            &mut failures,
                            finding.target.display(),
                            &e,
                            ctx.elevated,
                        ),
                    }
                }
            }
            Target::RegKey(key) => {
                if dry_run {
                    out.items += 1;
                } else {
                    match reg::delete_key(key, None) {
                        Ok(()) => out.items += 1,
                        Err(e) => record(
                            &mut out,
                            &mut failures,
                            finding.target.display(),
                            &e,
                            ctx.elevated,
                        ),
                    }
                }
            }
            Target::Special(special_op) => {
                if dry_run {
                    out.bytes += finding.size;
                    out.items += finding.items;
                } else {
                    match special::clean(special_op, ctx) {
                        Ok((bytes, items)) => {
                            out.bytes += bytes;
                            out.items += items;
                            progress.add_bytes(bytes);
                        }
                        Err(e) => record(
                            &mut out,
                            &mut failures,
                            finding.target.display(),
                            &e,
                            ctx.elevated,
                        ),
                    }
                }
            }
        }
        progress.inc(1);
    }

    if !dry_run {
        for root in &result.prune_roots {
            delete::remove_empty_dirs(root);
        }
    }
    (out, failures)
}
