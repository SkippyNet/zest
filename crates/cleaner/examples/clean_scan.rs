//! Read-only analysis of this PC. Nothing is deleted.
//!
//! `cargo run -p zest-cleaner --example clean_scan [-- --all] [-- --verbose] [-- --timings]`
//!
//! `--all` analyses every rule (including privacy rules) instead of the
//! default selection; `--verbose` lists the largest findings per rule;
//! `--timings` analyses every selected rule on its own and lists the slowest.

use zest_cleaner::{Catalog, CleanOptions, Section, cookies, extensions, scan};
use zest_core::fmt::{format_bytes, format_count, format_duration};
use zest_core::paths::UserDirs;
use zest_core::{Progress, elevation};

fn main() {
    let all = std::env::args().any(|a| a == "--all");
    let verbose = std::env::args().any(|a| a == "--verbose");
    let users = [UserDirs::current()];

    let started = std::time::Instant::now();
    let catalog = Catalog::build(&users, &[]);
    println!(
        "Catalog: {} groups, {} rules (built in {}), elevated: {}",
        catalog.groups.len(),
        catalog.rules().count(),
        format_duration(started.elapsed()),
        elevation::is_elevated()
    );

    let opts = CleanOptions {
        enabled: if all {
            catalog.rules().map(|(_, r)| r.id.clone()).collect()
        } else {
            catalog.default_selection()
        },
        min_age_hours: 24,
        ..Default::default()
    };
    let report = scan(&catalog, &opts, &Progress::new());

    for section in Section::ALL {
        let groups: Vec<_> = catalog.section(section).collect();
        if groups.is_empty() {
            continue;
        }
        println!("\n== {} ==", section.label());
        for group in groups {
            let total: u64 = report
                .results
                .iter()
                .filter(|r| r.group_id == group.id)
                .map(|r| r.bytes)
                .sum();
            println!("  {:<34} {:>12}", group.name, format_bytes(total));
            for rule in &group.rules {
                let state = if opts.enabled.contains(&rule.id) {
                    "x"
                } else {
                    " "
                };
                match report.rule(&rule.id) {
                    Some(result) => {
                        let mut flags = String::new();
                        if let Some(blocker) = &result.blocked_by {
                            flags.push_str(&format!("  [{blocker}]"));
                        }
                        if result.needs_admin {
                            flags.push_str("  [needs admin]");
                        }
                        println!(
                            "    [{state}] {:<32} {:>12} {:>9} items{flags}",
                            rule.name,
                            format_bytes(result.bytes),
                            format_count(result.items)
                        );
                        if verbose {
                            let mut biggest: Vec<_> = result.findings.iter().collect();
                            biggest.sort_by_key(|f| std::cmp::Reverse(f.size));
                            for finding in biggest.iter().take(5) {
                                println!(
                                    "          {:>10}  {}{}",
                                    format_bytes(finding.size),
                                    finding.target.display(),
                                    finding
                                        .note
                                        .as_ref()
                                        .map(|n| format!("  ({n})"))
                                        .unwrap_or_default()
                                );
                            }
                        }
                    }
                    None => println!("    [{state}] {:<32} {:>12}", rule.name, "-"),
                }
            }
        }
    }
    println!(
        "\nTotal: {} in {} items, analysed in {}",
        format_bytes(report.bytes),
        format_count(report.items),
        format_duration(report.elapsed)
    );

    if std::env::args().any(|a| a == "--timings") {
        // Each rule on its own, to see which ones the analysis spends its time on.
        let mut timings: Vec<(std::time::Duration, String, u64)> = opts
            .enabled
            .iter()
            .map(|id| {
                let single = CleanOptions {
                    enabled: std::iter::once(id.clone()).collect(),
                    ..opts.clone()
                };
                let started = std::time::Instant::now();
                let report = scan(&catalog, &single, &Progress::new());
                (started.elapsed(), id.clone(), report.items)
            })
            .collect();
        timings.sort_by_key(|t| std::cmp::Reverse(t.0));
        println!("\nSlowest rules when analysed alone:");
        for (elapsed, id, items) in timings.iter().take(15) {
            println!(
                "  {:>8}  {:<40} {:>8} items",
                format_duration(*elapsed),
                id,
                format_count(*items)
            );
        }
        return;
    }

    let inventory = cookies::inventory(&users);
    println!(
        "\nCookies: {} hosts ({} unreadable browsers: {:?}); suggested keep list: {:?}",
        inventory.hosts.len(),
        inventory.unreadable.len(),
        inventory.unreadable,
        cookies::suggest_keep(&inventory.hosts)
    );

    let extensions = extensions::list_extensions(&users);
    println!("\nBrowser extensions: {}", extensions.len());
    for ext in &extensions {
        println!(
            "  {:<16} {:<12} {:<40} v{:<12} {:<7} access: {:<6} {}",
            ext.browser,
            ext.profile,
            ext.name.chars().take(40).collect::<String>(),
            ext.version,
            match ext.enabled {
                Some(true) => "on",
                Some(false) => "off",
                None => "?",
            },
            ext.access.label(),
            format_bytes(ext.size)
        );
    }
}
