//! `update`: Zest's own updates from its public repository, and the listing
//! behind `updates list`: other programs' updates on any PC.

use clap::Subcommand;
use serde_json::json;
use std::io::Write;
use zest_engine::core::fmt::{format_bytes, plural};
use zest_engine::core::{Progress, brand};
use zest_engine::updates::selfupdate::{self, Applied, CheckOutcome, CopyKind, Release};
use zest_engine::updates::software::{self, CheckOptions, Listing, Problem};
use zest_engine::{Settings, app_update};

type CliResult = Result<(), String>;

#[derive(Subcommand)]
pub enum UpdateAction {
    /// See whether a newer version of Zest has been published (changes nothing).
    Check,
    /// Download the newest version, verify it and install it.
    Install {
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

fn print_json(value: &serde_json::Value) -> CliResult {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

fn copy_kind_label(kind: CopyKind) -> &'static str {
    match kind {
        CopyKind::Installed => "installed",
        CopyKind::Portable => "portable",
        CopyKind::Unmanaged => "unmanaged",
    }
}

/// Runs the check and remembers the result the way the app does, so the
/// app does not ask again the same day.
fn check_and_record() -> Result<CheckOutcome, String> {
    let outcome = selfupdate::check();
    let mut settings = Settings::load();
    if app_update::record_check(&mut settings, &outcome, app_update::now())
        && let Err(e) = settings.save()
    {
        eprintln!("note: the result could not be saved: {e}");
    }
    outcome.map_err(|e| e.to_string())
}

fn describe_release(release: &Release, kind: CopyKind) {
    println!(
        "{} {} is available (this is {}).",
        brand::NAME,
        release.version,
        brand::VERSION
    );
    if let Some(published) = &release.published {
        println!(
            "Published {}",
            published.replace('T', " ").trim_end_matches('Z')
        );
    }
    match release.package_for(kind) {
        Some(package) => println!("Package: {} ({})", package.name, format_bytes(package.size)),
        None => println!("Download: {}", release.page_url),
    }
    let notes = release.notes_plain();
    if !notes.is_empty() {
        println!();
        for line in notes.lines().take(30) {
            println!("  {line}");
        }
    }
    println!("\n{}", release.page_url);
}

fn check(json: bool) -> CliResult {
    let outcome = check_and_record()?;
    let kind = CopyKind::current();
    if json {
        return print_json(&match &outcome {
            CheckOutcome::UpToDate { latest } => json!({
                "current": brand::VERSION,
                "latest": latest.as_ref().map(ToString::to_string),
                "update_available": false,
                "copy": copy_kind_label(kind),
            }),
            CheckOutcome::Available(release) => json!({
                "current": brand::VERSION,
                "latest": release.version.to_string(),
                "update_available": true,
                "copy": copy_kind_label(kind),
                "page": release.page_url,
                "published": release.published,
                "package": release.package_for(kind).map(|p| &p.name),
                "notes": release.notes,
            }),
        });
    }
    match &outcome {
        CheckOutcome::UpToDate {
            latest: Some(latest),
        } => {
            println!(
                "{} {} is up to date (latest release: {latest}).",
                brand::NAME,
                brand::VERSION
            );
        }
        CheckOutcome::UpToDate { latest: None } => println!(
            "{} {}: no release has been published yet.",
            brand::NAME,
            brand::VERSION
        ),
        CheckOutcome::Available(release) => {
            describe_release(release, kind);
            if kind == CopyKind::Unmanaged {
                println!(
                    "\nThis copy was not set up by the installer or the portable package; download the new version from the page above."
                );
            } else {
                println!("\nInstall it with: zest-cli update install");
            }
        }
    }
    Ok(())
}

fn confirm(question: &str) -> bool {
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

fn install(json: bool, yes: bool) -> CliResult {
    let release = match check_and_record()? {
        CheckOutcome::Available(release) => release,
        CheckOutcome::UpToDate { .. } => {
            if json {
                return print_json(&json!({ "updated": false, "current": brand::VERSION }));
            }
            println!("{} {} is up to date.", brand::NAME, brand::VERSION);
            return Ok(());
        }
    };
    let kind = CopyKind::current();
    if kind == CopyKind::Unmanaged {
        return Err(format!(
            "{} Download: {}",
            selfupdate::UpdateError::Unmanaged,
            release.page_url
        ));
    }
    if !json {
        describe_release(&release, kind);
        println!();
    }
    if !yes && !confirm(&format!("Install {} {} now?", brand::NAME, release.version)) {
        return Err("cancelled".to_owned());
    }

    let progress = Progress::new();
    let download = std::thread::scope(|scope| {
        let worker = scope.spawn(|| selfupdate::download(&release, &progress));
        // A plain progress line while the download runs.
        while !worker.is_finished() {
            if !json && progress.total() > 0 {
                eprint!(
                    "\r  {} of {}   ",
                    format_bytes(progress.done()),
                    format_bytes(progress.total())
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !json {
            eprintln!();
        }
        worker.join()
    })
    .map_err(|_| "the download stopped unexpectedly".to_owned())?
    .map_err(|e| e.to_string())?;

    match selfupdate::apply(&download).map_err(|e| e.to_string())? {
        Applied::InstallerStarted => {
            if json {
                return print_json(
                    &json!({ "updated": true, "version": release.version.to_string(), "how": "installer" }),
                );
            }
            println!(
                "The installer for {} {} is running. Close {} if it is open; it restarts when the installer is done.",
                brand::NAME,
                release.version,
                brand::NAME
            );
        }
        Applied::Replaced { .. } => {
            if json {
                return print_json(
                    &json!({ "updated": true, "version": release.version.to_string(), "how": "portable" }),
                );
            }
            println!(
                "{} is now version {}. If the app is open, restart it to use the new version.",
                brand::NAME,
                release.version
            );
        }
    }
    Ok(())
}

/// Entry point of `zest-cli update …`.
pub fn run(json: bool, action: UpdateAction) -> CliResult {
    match action {
        UpdateAction::Check => check(json),
        UpdateAction::Install { yes } => install(json, yes),
    }
}

fn print_problem(problem: &Problem) {
    eprintln!("{}.", problem.title());
    eprintln!("{}", problem.explanation());
    if let Some(detail) = problem.detail() {
        for line in detail.lines() {
            eprintln!("  {line}");
        }
    }
}

/// `zest-cli updates list`: programs with a newer version, on any PC.
pub fn software_list(json: bool) -> CliResult {
    let settings = Settings::load();
    let options = CheckOptions {
        ignore_ids: settings.general.ignored_updates.clone(),
        catalog_fallback: settings.general.catalog_fallback,
        ..Default::default()
    };
    let listing = software::check(&options, &Progress::new()).map_err(|e| e.to_string())?;
    match listing {
        Listing::PackageManager(updates) => {
            if json {
                return print_json(&json!({ "source": "package-manager", "updates": updates }));
            }
            for update in &updates {
                println!(
                    "{:<44} {:<20} -> {}",
                    truncate(&update.name, 44),
                    truncate(&update.installed_version, 20),
                    update.available_version
                );
            }
            println!("\n{} available.", plural(updates.len() as u64, "update"));
            Ok(())
        }
        Listing::Catalog {
            problem,
            updates,
            packages_in_catalog,
        } => {
            if json {
                return print_json(&json!({
                    "source": "catalog",
                    "problem": problem,
                    "packages_in_catalog": packages_in_catalog,
                    "updates": updates,
                }));
            }
            print_problem(&problem);
            eprintln!(
                "\nCompared your programs with the public software catalog ({packages_in_catalog} packages) instead:\n"
            );
            for update in &updates {
                println!(
                    "{:<44} {:<20} -> {:<16} {}",
                    truncate(&update.name, 44),
                    truncate(&update.installed_version, 20),
                    truncate(&update.available_version, 16),
                    update.info_url
                );
            }
            println!(
                "\n{} found. Installing them needs the Windows Package Manager.",
                plural(updates.len() as u64, "newer version")
            );
            Ok(())
        }
        Listing::Unavailable {
            problem,
            catalog_error,
        } => {
            if json {
                print_json(&json!({
                    "source": "none",
                    "problem": problem,
                    "catalog_error": catalog_error,
                }))?;
            } else {
                print_problem(&problem);
                if let Some(error) = &catalog_error {
                    eprintln!("\n{error}");
                }
            }
            Err("software updates could not be checked".to_owned())
        }
    }
}
