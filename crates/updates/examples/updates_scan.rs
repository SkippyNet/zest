//! Read-only tour of the updater engines on the current PC.
//!
//! ```text
//! cargo run -p zest-updates --example updates_scan
//! cargo run -p zest-updates --example updates_scan -- --skip-windows-update
//! cargo run -p zest-updates --example updates_scan -- --skip-software
//! ```
//!
//! Nothing is downloaded, installed, exported or changed: the program lists
//! available software updates, lists installed drivers and *searches* Windows
//! Update for driver updates (the search can take a couple of minutes).

use std::time::Instant;
use zest_core::fmt::{format_bytes, format_duration};
use zest_core::{Progress, process};
use zest_updates::drivers::{self, DriverListOptions};
use zest_updates::software::{self, CheckOptions, SoftwareUpdate};

fn main() {
    let has_flag = |flag: &str| std::env::args().any(|arg| arg == flag);
    if !has_flag("--skip-software") {
        software_section();
    }
    driver_section();
    if has_flag("--skip-windows-update") {
        println!("\n== Driver updates ==\n  skipped (--skip-windows-update)");
    } else {
        driver_update_section();
    }
}

fn software_section() {
    println!("== Software updates ==");
    let started = Instant::now();
    let status = software::status();
    println!(
        "  package manager: {} (version {}, at {}) [{}]",
        if status.available {
            "available"
        } else {
            "MISSING"
        },
        status.version.as_deref().unwrap_or("?"),
        status
            .path
            .as_ref()
            .map_or("-".to_owned(), |p| p.display().to_string()),
        format_duration(started.elapsed())
    );
    if !status.available {
        println!("  {}", software::MISSING_MESSAGE);
        return;
    }

    let progress = Progress::new();
    let started = Instant::now();
    match software::check_updates(&CheckOptions::default(), &progress) {
        Ok(updates) => {
            println!(
                "  {} updates found in {} ({})",
                updates.len(),
                format_duration(started.elapsed()),
                progress.message()
            );
            print_updates(&updates);
        }
        Err(e) => println!(
            "  check failed after {}: {e}",
            format_duration(started.elapsed())
        ),
    }

    verify_against_raw_listing(&status);
}

fn print_updates(updates: &[SoftwareUpdate]) {
    let name_width = updates
        .iter()
        .map(|u| u.name.chars().count())
        .max()
        .unwrap_or(4)
        .min(48);
    let id_width = updates
        .iter()
        .map(|u| u.id.chars().count())
        .max()
        .unwrap_or(2)
        .min(44);
    for update in updates {
        let name: String = update.name.chars().take(name_width).collect();
        println!(
            "    {name:<name_width$}  {:<id_width$}  {} -> {}  [{}]{}{}",
            update.id,
            update.installed_version,
            update.available_version,
            update.source,
            if update.requires_explicit_upgrade {
                " (explicit)"
            } else {
                ""
            },
            if update.can_upgrade() {
                ""
            } else {
                " (CANNOT UPGRADE)"
            },
        );
    }
}

/// Runs the same read-only listing once more, parses it, and checks every
/// table line of the raw text against the parsed fields.
fn verify_against_raw_listing(status: &software::PackageManagerStatus) {
    let Some(exe) = status.path.as_ref() else {
        return;
    };
    let args = software::check_command_args(&CheckOptions::default());
    let raw = match process::run_capture(exe, &args) {
        Ok(output) => output.stdout,
        Err(e) => {
            println!("  verification skipped: {e}");
            return;
        }
    };
    let parsed = software::parse_upgrade_listing(&raw);
    let squash = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let expected: Vec<String> = parsed
        .iter()
        .map(|u| {
            squash(&format!(
                "{} {} {} {} {}",
                u.name, u.id, u.installed_version, u.available_version, u.source
            ))
        })
        .collect();

    // Raw table lines: everything after a dashed rule that is not a blank
    // line, another rule, a header (followed by a rule) or a sentence that
    // matches no parsed row.
    let lines: Vec<&str> = raw.lines().map(str::trim_end).collect();
    let is_rule = |line: &str| line.len() >= 10 && line.chars().all(|c| c == '-');
    let mut in_table = false;
    let mut raw_rows = 0usize;
    let mut matched = 0usize;
    let mut unmatched: Vec<&str> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if is_rule(line) {
            in_table = true;
            continue;
        }
        if !in_table || line.trim().is_empty() {
            continue;
        }
        if lines.get(index + 1).is_some_and(|next| is_rule(next)) {
            continue; // header of a following table
        }
        if expected.contains(&squash(line)) {
            raw_rows += 1;
            matched += 1;
        } else if line.split_whitespace().count() >= 4
            && !line.ends_with('.')
            && !line.ends_with(':')
        {
            raw_rows += 1;
            unmatched.push(line);
        }
    }
    println!(
        "  parser check: {matched}/{raw_rows} raw table lines reproduced exactly from {} parsed rows",
        parsed.len()
    );
    for line in unmatched {
        println!("    UNMATCHED RAW LINE: {line}");
    }
    if matched != parsed.len() {
        println!(
            "    WARNING: {} parsed rows have no identical raw line",
            parsed.len() - matched.min(parsed.len())
        );
    }
}

fn driver_section() {
    println!("\n== Installed drivers ==");
    let progress = Progress::new();

    let started = Instant::now();
    let fast = drivers::list_drivers_with(
        &DriverListOptions {
            verify_signatures: false,
            ..DriverListOptions::default()
        },
        &progress,
    );
    println!(
        "  listing without signatures: {} devices in {}",
        fast.as_ref().map_or(0, Vec::len),
        format_duration(started.elapsed())
    );

    let started = Instant::now();
    let all = match drivers::list_drivers(&progress) {
        Ok(all) => all,
        Err(e) => {
            println!("  listing failed: {e}");
            return;
        }
    };
    let elapsed = started.elapsed();
    let packages = drivers::group_by_package(&all);
    let third_party = all.iter().filter(|d| d.is_third_party()).count();
    println!(
        "  full listing: {} devices, {} driver packages in {}",
        all.len(),
        packages.len(),
        format_duration(elapsed)
    );
    println!(
        "  third-party: {} devices / {} packages; Microsoft-provided: {} devices",
        third_party,
        packages.iter().filter(|p| !p.is_inbox).count(),
        all.iter().filter(|d| d.is_microsoft).count()
    );
    println!(
        "  signed: {}, unsigned: {}, with problems: {}, flagged old: {}",
        all.iter().filter(|d| d.is_signed == Some(true)).count(),
        all.iter().filter(|d| d.is_signed == Some(false)).count(),
        all.iter().filter(|d| d.has_problem()).count(),
        all.iter().filter(|d| d.is_outdated_hint).count()
    );

    println!("  third-party driver packages:");
    for package in packages.iter().filter(|p| !p.is_inbox).take(40) {
        println!(
            "    {:<11} {:<26} {:<18} {}  {:<12} {}{} | {}",
            package.inf_name,
            truncate(&package.provider, 26),
            truncate(&package.driver_version, 18),
            package
                .driver_date
                .map_or("????-??-??".to_owned(), |d| d.to_string()),
            truncate(&package.device_class, 12),
            match package.is_signed {
                Some(true) => "signed",
                Some(false) => "UNSIGNED",
                None => "unchecked",
            },
            if package.is_outdated_hint { " OLD" } else { "" },
            truncate(&package.devices.join("; "), 70),
        );
    }
    for driver in all.iter().filter(|d| d.has_problem()) {
        println!(
            "  problem: {} (code {}): {}",
            driver.device_name,
            driver.problem_code,
            driver.problem_description().unwrap_or("no description")
        );
    }
    for driver in all.iter().filter(|d| d.is_signed == Some(false)).take(10) {
        println!(
            "  unsigned: {} [{}] by {}",
            driver.device_name, driver.inf_name, driver.provider
        );
    }
    let mut signers: Vec<(String, usize)> = Vec::new();
    for package in &packages {
        let signer = package
            .signer
            .clone()
            .unwrap_or_else(|| "(none)".to_owned());
        match signers.iter_mut().find(|(name, _)| *name == signer) {
            Some((_, count)) => *count += 1,
            None => signers.push((signer, 1)),
        }
    }
    signers.sort_by(|a, b| b.1.cmp(&a.1));
    println!("  signers:");
    for (signer, count) in signers {
        println!("    {count:>4}  {signer}");
    }
}

fn driver_update_section() {
    println!("\n== Driver updates (Windows Update search, read-only) ==");
    let progress = Progress::new();
    let started = Instant::now();
    match drivers::check_driver_updates(&progress) {
        Ok(updates) => {
            println!(
                "  {} driver updates found in {} ({})",
                updates.len(),
                format_duration(started.elapsed()),
                progress.message()
            );
            for update in &updates {
                println!(
                    "    {} | {:?}, class {} | {} | {} | {}{}{}",
                    update.title,
                    update.kind(),
                    update.driver_class,
                    update
                        .driver_date
                        .map_or("no date".to_owned(), |d| d.to_string()),
                    format_bytes(update.size_bytes),
                    update.hardware_id,
                    if update.is_optional {
                        " | optional"
                    } else {
                        ""
                    },
                    if update.reboot_expected {
                        " | may restart"
                    } else {
                        ""
                    },
                );
                match &update.applies_to {
                    Some(current) => println!(
                        "        device: {} | current driver {} ({}) [{}]",
                        current.device_name,
                        current.driver_version,
                        current
                            .driver_date
                            .map_or("no date".to_owned(), |d| d.to_string()),
                        current.inf_name
                    ),
                    None => println!("        (no installed driver matched)"),
                }
            }
        }
        Err(e) => println!(
            "  search failed after {}: {e}",
            format_duration(started.elapsed())
        ),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
