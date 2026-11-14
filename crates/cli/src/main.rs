//! Command line interface for Zest.
//!
//! Every command that only looks is safe to run at any time. Commands that
//! remove or change something (`clean`, `registry fix`, `privacy apply`,
//! `tools run`, `backup restore`, `update install`) ask for confirmation
//! unless `--yes` is given; `clean` also supports `--dry-run`.

#![cfg(windows)]

mod backup_cmd;
mod maintain_cmd;
mod update_cmd;

use clap::{Parser, Subcommand};
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use zest_engine::cleaner::{Catalog, CleanOptions, Section};
use zest_engine::core::Progress;
use zest_engine::core::fmt::{format_bytes, format_count, format_duration, plural};
use zest_engine::disk::{self, AnalyzeOptions, DuplicateOptions};
use zest_engine::registry::{self, ScanOptions};
use zest_engine::system::apps::{self, ListOptions};
use zest_engine::system::startup;
use zest_engine::updates::drivers;
use zest_engine::{RunKind, Settings, health, ops};

#[derive(Parser)]
#[command(
    name = "zest-cli",
    version,
    about = "Zest: a fast, private PC cleaner and optimizer for Windows."
)]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check privacy, space, speed and security (changes nothing).
    Health,
    /// Show what a clean would remove (changes nothing).
    Analyze {
        /// Analyze every rule, not just your selection.
        #[arg(long)]
        all: bool,
    },
    /// Clean using your saved selection.
    Clean {
        /// Show what would be removed without removing anything.
        #[arg(long)]
        dry_run: bool,
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// List every cleaning rule and whether it is selected.
    Rules,
    /// Registry cleaner.
    Registry {
        #[command(subcommand)]
        action: maintain_cmd::RegistryAction,
    },
    /// Startup apps, scheduled tasks, services and context-menu entries.
    Startup {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Installed programs.
    Apps {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Programs with updates available.
    Updates {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Installed third-party drivers.
    Drivers {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Disk analyzer.
    Disk {
        #[command(subcommand)]
        action: DiskAction,
    },
    /// Find duplicate files in one or more folders (changes nothing).
    Duplicates {
        /// Folders to compare.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Ignore files smaller than this many bytes.
        #[arg(long, default_value_t = 8 * 1024)]
        min_size: u64,
    },
    /// Windows privacy settings: where each one stands, apply and revert.
    Privacy {
        #[command(subcommand)]
        action: maintain_cmd::PrivacyAction,
    },
    /// The repair and maintenance tools that ship with Windows.
    Tools {
        #[command(subcommand)]
        action: maintain_cmd::ToolsAction,
    },
    /// Hardware, drive health and battery of this PC (changes nothing).
    System,
    /// Versioned file backups: create, list, restore and verify.
    Backup {
        #[command(subcommand)]
        action: backup_cmd::BackupAction,
    },
    /// Zest's own updates: check for a newer version and install it.
    Update {
        #[command(subcommand)]
        action: update_cmd::UpdateAction,
    },
}

#[derive(Subcommand)]
enum ListAction {
    /// List the entries (changes nothing).
    List,
}

#[derive(Subcommand)]
enum DiskAction {
    /// Break down what uses space under a folder (changes nothing).
    Analyze {
        /// Folder or drive to analyze.
        path: PathBuf,
    },
}

type CliResult = Result<(), String>;

fn print_json<T: Serialize>(value: &T) -> CliResult {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn selection(settings: &Settings, catalog: &Catalog, all: bool) -> CleanOptions {
    let mut options = ops::clean_options(settings, catalog);
    if all {
        options.enabled = catalog.rules().map(|(_, r)| r.id.clone()).collect();
    }
    options
}

#[derive(Serialize)]
struct RuleRow<'a> {
    id: &'a str,
    group: &'a str,
    name: &'a str,
    selected: bool,
    bytes: u64,
    items: u64,
    blocked_by: Option<&'a str>,
    needs_admin: bool,
}

fn analyze(json: bool, all: bool) -> CliResult {
    let settings = Settings::load();
    let catalog = ops::catalog(&settings);
    let options = selection(&settings, &catalog, all);
    let report = zest_engine::cleaner::scan(&catalog, &options, &Progress::new());
    if json {
        return print_json(&report);
    }
    for section in Section::ALL {
        for group in catalog.section(section) {
            let results: Vec<_> = report
                .results
                .iter()
                .filter(|r| r.group_id == group.id)
                .collect();
            if results.is_empty() {
                continue;
            }
            let total: u64 = results.iter().map(|r| r.bytes).sum();
            println!("{:<44} {:>12}", group.name, format_bytes(total));
            for result in results {
                let name = group
                    .rules
                    .iter()
                    .find(|r| r.id == result.rule_id)
                    .map_or(result.rule_id.as_str(), |r| r.name.as_str());
                let mut note = String::new();
                if let Some(blocker) = &result.blocked_by {
                    note = format!("  [{blocker}]");
                } else if result.needs_admin {
                    note = "  [needs administrator]".to_owned();
                }
                println!(
                    "  {:<42} {:>12} {:>10}{note}",
                    truncate(name, 42),
                    format_bytes(result.bytes),
                    plural(result.items, "item")
                );
            }
        }
    }
    println!(
        "\n{} in {} can be removed (analyzed in {}). Nothing was changed.",
        format_bytes(report.bytes),
        plural(report.items, "item"),
        format_duration(report.elapsed)
    );
    Ok(())
}

fn clean(json: bool, dry_run: bool, yes: bool) -> CliResult {
    let settings = Settings::load();
    let analysis = ops::analyze(&settings, None, &Progress::new());
    if analysis.scan.items == 0 {
        println!("Nothing to clean.");
        return Ok(());
    }
    if !dry_run && !yes {
        print!(
            "Remove {} ({})? This cannot be undone. [y/N] ",
            plural(analysis.scan.items, "item"),
            format_bytes(analysis.scan.bytes)
        );
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            println!("Cancelled. Nothing was removed.");
            return Ok(());
        }
    }
    let report = ops::run_clean(&analysis, RunKind::Manual, dry_run, &Progress::new());
    if json {
        return print_json(&report);
    }
    let verb = if dry_run { "would be freed" } else { "freed" };
    println!(
        "{} {verb}, {} removed in {}.",
        format_bytes(report.bytes),
        plural(report.items, "item"),
        format_duration(report.elapsed)
    );
    if report.in_use > 0 {
        println!(
            "{} in use and left in place.",
            plural(report.in_use, "item")
        );
    }
    for rule in report.rules.iter().filter(|r| r.skipped.is_some()) {
        println!(
            "Skipped {}: {}.",
            rule.rule_id,
            rule.skipped.as_deref().unwrap_or_default()
        );
    }
    if report.failed > 0 {
        println!("{} could not be removed:", plural(report.failed, "item"));
        for (what, why) in report.failures.iter().take(20) {
            println!("  {what}: {why}");
        }
    }
    Ok(())
}

fn rules(json: bool) -> CliResult {
    let settings = Settings::load();
    let catalog = ops::catalog(&settings);
    let enabled = ops::clean_options(&settings, &catalog).enabled;
    if json {
        let rows: Vec<RuleRow> = catalog
            .rules()
            .map(|(group, rule)| RuleRow {
                id: &rule.id,
                group: &group.name,
                name: &rule.name,
                selected: enabled.contains(&rule.id),
                bytes: 0,
                items: 0,
                blocked_by: None,
                needs_admin: rule.requires_admin,
            })
            .collect();
        return print_json(&rows);
    }
    for (group, rule) in catalog.rules() {
        let mark = if enabled.contains(&rule.id) { "x" } else { " " };
        println!(
            "[{mark}] {:<30} {:<24} {}",
            rule.id,
            truncate(&group.name, 24),
            rule.name
        );
    }
    Ok(())
}

#[derive(Serialize)]
struct HealthSummary {
    trackers: u64,
    privacy_items: u64,
    junk_bytes: u64,
    junk_items: u64,
    startup_programs: Vec<String>,
    updates_available: Option<Vec<String>>,
    blocked_browsers: Vec<String>,
    system_drive_total: u64,
    system_drive_free: u64,
}

fn health_check(json: bool) -> CliResult {
    let settings = Settings::load();
    let report = health::check(&settings, &Progress::new());
    if json {
        return print_json(&HealthSummary {
            trackers: report.trackers,
            privacy_items: report.privacy_items,
            junk_bytes: report.junk_bytes,
            junk_items: report.junk_items,
            startup_programs: report.startup.iter().map(|s| s.name.clone()).collect(),
            updates_available: report
                .updates
                .as_ref()
                .map(|u| u.iter().map(|u| u.name.clone()).collect()),
            blocked_browsers: report.blocked.clone(),
            system_drive_total: report.drive_total,
            system_drive_free: report.drive_free,
        });
    }
    println!(
        "Privacy   {} can be removed",
        plural(report.trackers + report.privacy_items, "tracker")
    );
    println!(
        "Space     {} of junk in {}",
        format_bytes(report.junk_bytes),
        plural(report.junk_items, "item")
    );
    println!(
        "Speed     {} start with Windows",
        plural(report.startup.len() as u64, "program")
    );
    match &report.updates {
        Some(updates) => println!(
            "Security  {} can be updated",
            plural(updates.len() as u64, "program")
        ),
        None => println!("Security  software updates could not be checked"),
    }
    println!(
        "Drive     {} free of {}",
        format_bytes(report.drive_free),
        format_bytes(report.drive_total)
    );
    if !report.blocked.is_empty() {
        println!(
            "Note      {} running: its data was not checked. Close it and run this again.",
            report.blocked.join(" and ")
        );
    }
    Ok(())
}

fn registry_scan(json: bool) -> CliResult {
    let settings = Settings::load();
    let options = ScanOptions {
        excluded: settings.registry.excluded.clone(),
        ..Default::default()
    };
    let issues = registry::scan(&options, &Progress::new()).map_err(|e| e.to_string())?;
    if json {
        return print_json(&issues);
    }
    for (category, count) in registry::count_by_category(&issues) {
        if count > 0 {
            println!("{:<28} {:>5}", category.label(), count);
        }
    }
    println!();
    for issue in &issues {
        println!(
            "{:<34} {}",
            truncate(&issue.title, 34),
            truncate(&issue.location(), 90)
        );
    }
    println!(
        "\n{} found. Nothing was changed.",
        plural(issues.len() as u64, "issue")
    );
    Ok(())
}

fn startup_list(json: bool) -> CliResult {
    let items = startup::list_all(&Progress::new()).map_err(|e| e.to_string())?;
    if json {
        return print_json(&items);
    }
    for item in items.iter().filter(|i| !i.is_microsoft) {
        println!(
            "{:<4} {:<16} {:<38} {}",
            if item.enabled { "on" } else { "off" },
            item.kind.label(),
            truncate(&item.name, 38),
            truncate(&item.command, 70)
        );
    }
    println!(
        "\n{} shown ({} Windows entries hidden; use --json for everything).",
        format_count(items.iter().filter(|i| !i.is_microsoft).count() as u64),
        format_count(items.iter().filter(|i| i.is_microsoft).count() as u64)
    );
    Ok(())
}

fn apps_list(json: bool) -> CliResult {
    let list = apps::list_installed(&ListOptions::default());
    if json {
        return print_json(&list);
    }
    for app in &list {
        println!(
            "{:<48} {:<22} {:>10}",
            truncate(&app.name, 48),
            truncate(app.version.as_deref().unwrap_or(""), 22),
            app.size_bytes.map(format_bytes).unwrap_or_default()
        );
    }
    println!("\n{}.", plural(list.len() as u64, "program"));
    Ok(())
}

fn updates_list(json: bool) -> CliResult {
    update_cmd::software_list(json)
}

fn drivers_list(json: bool) -> CliResult {
    let all = drivers::list_drivers(&Progress::new()).map_err(|e| e.to_string())?;
    let packages: Vec<_> = drivers::group_by_package(&all)
        .into_iter()
        .filter(|p| !(p.is_inbox || p.is_microsoft))
        .collect();
    if json {
        return print_json(&packages);
    }
    for package in &packages {
        println!(
            "{:<50} {:<24} {:<18} {}",
            truncate(
                package
                    .devices
                    .first()
                    .map_or(package.inf_name.as_str(), String::as_str),
                50
            ),
            truncate(&package.provider, 24),
            package.driver_version,
            package
                .driver_date
                .map(|d| d.to_string())
                .unwrap_or_default()
        );
    }
    println!(
        "\n{}.",
        plural(packages.len() as u64, "third-party driver package")
    );
    Ok(())
}

fn disk_analyze(json: bool, path: PathBuf) -> CliResult {
    let analysis = disk::analyze(&AnalyzeOptions::for_path(path), &Progress::new())
        .map_err(|e| e.to_string())?;
    if json {
        return print_json(&analysis);
    }
    println!(
        "{} in {} and {} (analyzed in {})\n",
        format_bytes(analysis.total_bytes),
        plural(analysis.total_files, "file"),
        plural(analysis.total_dirs, "folder"),
        format_duration(Duration::from_millis(analysis.elapsed_ms))
    );
    for stat in analysis.categories.iter().filter(|c| c.bytes > 0) {
        println!(
            "{:<14} {:>12} {:>12}",
            stat.category.label(),
            format_bytes(stat.bytes),
            plural(stat.files, "file")
        );
    }
    if let Some(root) = analysis.tree.first() {
        println!("\nLargest folders");
        for child in root.children.iter().take(15) {
            println!("{:>12}  {}", format_bytes(child.bytes), child.name);
        }
    }
    println!("\nLargest files");
    for file in analysis.largest_files.iter().take(15) {
        println!("{:>12}  {}", format_bytes(file.size), file.path.display());
    }
    Ok(())
}

fn duplicates(json: bool, paths: Vec<PathBuf>, min_size: u64) -> CliResult {
    let mut options = DuplicateOptions::for_roots(paths);
    options.min_size = min_size.max(1);
    let report = disk::find_duplicates(&options, &Progress::new()).map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    for group in report.groups.iter().take(50) {
        println!(
            "{} x {} ({} wasted)",
            group.files.len(),
            format_bytes(group.size),
            format_bytes(group.wasted_bytes())
        );
        for file in &group.files {
            println!("    {}", file.path.display());
        }
    }
    if report.groups.len() > 50 {
        println!(
            "… and {} more sets (use --json for all)",
            report.groups.len() - 50
        );
    }
    println!(
        "\n{} wasted by {} ({} compared in {}). Nothing was changed.",
        format_bytes(report.wasted_bytes),
        plural(report.groups.len() as u64, "set"),
        plural(report.files_scanned, "file"),
        format_duration(Duration::from_millis(report.elapsed_ms))
    );
    Ok(())
}

/// Prints the hardware inventory. The report names no serial numbers,
/// network addresses, computer or user.
fn system_info(json: bool) -> CliResult {
    let inventory = zest_engine::hardware::inventory();
    if json {
        println!("{}", zest_engine::hardware::to_json(&inventory));
    } else {
        print!("{}", zest_engine::hardware::to_text(&inventory));
    }
    Ok(())
}

fn privacy_list(json: bool) -> CliResult {
    use zest_engine::tweaks::privacy::{Category, Privacy, State};

    let privacy = Privacy::system();
    let list = privacy.list();
    if json {
        return print_json(&list);
    }
    for category in Category::ALL {
        println!("{}", category.label());
        for status in list.iter().filter(|s| s.tweak.category == category) {
            let state = match status.state {
                State::Private => "private",
                State::Default => "default",
                State::Custom => "custom",
                State::NotApplicable => "n/a",
            };
            let mut flags = Vec::new();
            if status.tweak.recommended {
                flags.push("recommended");
            }
            if status.needs_admin {
                flags.push("needs administrator");
            }
            println!(
                "  {:<8} {:<26} {:<48} {}",
                state,
                status.tweak.id,
                truncate(status.tweak.title, 48),
                flags.join(", ")
            );
        }
        println!();
    }
    let applicable = list
        .iter()
        .filter(|s| s.state != State::NotApplicable)
        .count();
    let private = list.iter().filter(|s| s.state == State::Private).count();
    println!(
        "{private} of {applicable} settings are private. Change them on the Windows privacy page of the app."
    );
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let result = match cli.command {
        Command::Health => health_check(json),
        Command::Analyze { all } => analyze(json, all),
        Command::Clean { dry_run, yes } => clean(json, dry_run, yes),
        Command::Rules => rules(json),
        Command::Registry { action } => match action {
            maintain_cmd::RegistryAction::Scan => registry_scan(json),
            maintain_cmd::RegistryAction::Fix { yes } => maintain_cmd::registry_fix(json, yes),
        },
        Command::Startup {
            action: ListAction::List,
        } => startup_list(json),
        Command::Apps {
            action: ListAction::List,
        } => apps_list(json),
        Command::Updates {
            action: ListAction::List,
        } => updates_list(json),
        Command::Drivers {
            action: ListAction::List,
        } => drivers_list(json),
        Command::Disk {
            action: DiskAction::Analyze { path },
        } => disk_analyze(json, path),
        Command::Duplicates { paths, min_size } => duplicates(json, paths, min_size),
        Command::Privacy { action } => match action {
            maintain_cmd::PrivacyAction::List => privacy_list(json),
            maintain_cmd::PrivacyAction::Apply {
                ids,
                recommended,
                yes,
            } => maintain_cmd::privacy_apply(json, ids, recommended, yes),
            maintain_cmd::PrivacyAction::Revert { ids, all, yes } => {
                maintain_cmd::privacy_revert(json, ids, all, yes)
            }
        },
        Command::Tools { action } => match action {
            maintain_cmd::ToolsAction::List => maintain_cmd::tools_list(json),
            maintain_cmd::ToolsAction::Run { id, yes } => maintain_cmd::tools_run(json, &id, yes),
        },
        Command::System => system_info(json),
        Command::Backup { action } => backup_cmd::run(json, action),
        Command::Update { action } => update_cmd::run(json, action),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
