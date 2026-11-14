//! Commands that change Windows itself: `registry fix`, `privacy apply` and
//! `privacy revert`, and `tools run`. Each one says what it is about to do
//! and asks first, unless `--yes` is given.

use clap::Subcommand;
use std::io::Write;
use zest_engine::Settings;
use zest_engine::core::Progress;
use zest_engine::core::fmt::plural;
use zest_engine::registry::{self, RegBackup, ScanOptions};
use zest_engine::tweaks::privacy::{ItemOutcome, Outcome, Privacy, State};
use zest_engine::tweaks::tools::{self, TOOLS, Tool, Verdict};

type CliResult = Result<(), String>;

#[derive(Subcommand)]
pub enum RegistryAction {
    /// List invalid registry entries (changes nothing).
    Scan,
    /// Remove the invalid entries a scan finds. A backup is saved first.
    Fix {
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum PrivacyAction {
    /// List the settings and where each one stands (changes nothing).
    List,
    /// Switch settings to private. A backup is saved first.
    Apply {
        /// Ids of the settings, as shown by `privacy list`.
        ids: Vec<String>,
        /// Apply the recommended set instead of naming settings.
        #[arg(long, conflicts_with = "ids")]
        recommended: bool,
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
    /// Put settings back to the Windows default. A backup is saved first.
    Revert {
        /// Ids of the settings, as shown by `privacy list`.
        ids: Vec<String>,
        /// Revert every setting that is not at its default.
        #[arg(long, conflicts_with = "ids")]
        all: bool,
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ToolsAction {
    /// List the repair and maintenance tools that ship with Windows (changes nothing).
    List,
    /// Run one tool and show what it reports.
    Run {
        /// Id of the tool, as shown by `tools list`.
        id: String,
        /// Do not ask for confirmation.
        #[arg(long, short)]
        yes: bool,
    },
}

fn print_json<T: serde::Serialize>(value: &T) -> CliResult {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(())
}

/// Asks a yes/no question on the terminal. Anything but "y" or "yes" is no.
fn confirm(question: &str) -> Result<bool, String> {
    print!("{question} [y/N] ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// `registry fix`: scans, then removes what the scan found.
pub fn registry_fix(json: bool, yes: bool) -> CliResult {
    let settings = Settings::load();
    let options = ScanOptions {
        excluded: settings.registry.excluded.clone(),
        ..Default::default()
    };
    let issues = registry::scan(&options, &Progress::new()).map_err(|e| e.to_string())?;
    if issues.is_empty() {
        if json {
            return print_json(&registry::FixReport::default());
        }
        println!("No invalid registry entries were found.");
        return Ok(());
    }
    let with_backup = settings.registry.backup_before_fix;
    if !yes {
        let safety = if with_backup {
            "A backup is saved first."
        } else {
            "Backups are switched off in your settings."
        };
        let question = format!(
            "Remove {}? {safety}",
            plural(issues.len() as u64, "invalid registry entry").replace("entrys", "entries")
        );
        if !confirm(&question)? {
            println!("Cancelled. Nothing was changed.");
            return Ok(());
        }
    }
    let mut backup = with_backup.then(|| RegBackup::new("registry-clean"));
    let report =
        registry::fix(&issues, backup.as_mut(), &Progress::new()).map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    println!(
        "{} removed.",
        plural(report.fixed as u64, "entry").replace("entrys", "entries")
    );
    if report.skipped_needs_admin > 0 {
        println!(
            "{} left alone: removing them needs administrator rights.",
            report.skipped_needs_admin
        );
    }
    if report.skipped_changed > 0 {
        println!(
            "{} left alone: they changed since the scan.",
            report.skipped_changed
        );
    }
    for (id, error) in &report.failed {
        println!("could not remove {id}: {error}");
    }
    if let Some(file) = &report.backup_file {
        println!("Backup: {}", file.display());
    }
    Ok(())
}

fn describe(outcome: &ItemOutcome) -> String {
    match outcome {
        ItemOutcome::Changed => "changed".to_owned(),
        ItemOutcome::Unchanged => "already set".to_owned(),
        ItemOutcome::NeedsAdmin => "skipped: needs administrator rights".to_owned(),
        ItemOutcome::NotApplicable => "skipped: not on this version of Windows".to_owned(),
        ItemOutcome::UnknownId => "no setting has this id".to_owned(),
        ItemOutcome::Failed(message) => format!("failed: {message}"),
    }
}

fn report_privacy(json: bool, outcome: &Outcome) -> CliResult {
    if json {
        print_json(outcome)?;
    } else {
        for item in &outcome.items {
            println!("  {:<28} {}", item.id, describe(&item.outcome));
        }
        println!("{} changed.", plural(outcome.changed() as u64, "setting"));
        if let Some(file) = &outcome.backup {
            println!("Backup: {}", file.display());
        }
    }
    let unknown = outcome
        .items
        .iter()
        .filter(|i| i.outcome == ItemOutcome::UnknownId)
        .count();
    if outcome.failed() > 0 || unknown > 0 {
        return Err(format!(
            "{} could not be changed",
            plural((outcome.failed() + unknown) as u64, "setting")
        ));
    }
    Ok(())
}

/// `privacy apply`: switches the named or the recommended settings to private.
pub fn privacy_apply(json: bool, ids: Vec<String>, recommended: bool, yes: bool) -> CliResult {
    let privacy = Privacy::system();
    let ids: Vec<String> = if recommended {
        privacy
            .recommended_pending()
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        ids
    };
    if ids.is_empty() {
        if recommended {
            println!("Every recommended setting that applies to this PC is already private.");
            return Ok(());
        }
        return Err(
            "name the settings to apply (see `privacy list`), or pass --recommended".into(),
        );
    }
    if !yes
        && !confirm(&format!(
            "Switch {} to private? A backup is saved first.",
            plural(ids.len() as u64, "privacy setting")
        ))?
    {
        println!("Cancelled. Nothing was changed.");
        return Ok(());
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let outcome = privacy.apply(&refs).map_err(|e| e.to_string())?;
    report_privacy(json, &outcome)
}

/// `privacy revert`: puts the named settings, or all of them, back to the
/// Windows default.
pub fn privacy_revert(json: bool, ids: Vec<String>, all: bool, yes: bool) -> CliResult {
    let privacy = Privacy::system();
    let ids: Vec<String> = if all {
        privacy
            .list()
            .into_iter()
            .filter(|s| matches!(s.state, State::Private | State::Custom))
            .map(|s| s.tweak.id.to_owned())
            .collect()
    } else {
        ids
    };
    if ids.is_empty() {
        if all {
            println!("Every setting is already at the Windows default.");
            return Ok(());
        }
        return Err("name the settings to revert (see `privacy list`), or pass --all".into());
    }
    if !yes
        && !confirm(&format!(
            "Put {} back to the Windows default? A backup is saved first.",
            plural(ids.len() as u64, "privacy setting")
        ))?
    {
        println!("Cancelled. Nothing was changed.");
        return Ok(());
    }
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let outcome = privacy.revert(&refs).map_err(|e| e.to_string())?;
    report_privacy(json, &outcome)
}

/// `tools list`.
pub fn tools_list(json: bool) -> CliResult {
    if json {
        return print_json(&TOOLS);
    }
    for tool in TOOLS {
        let mut notes = Vec::new();
        if !tool.is_command() {
            notes.push("opens a window");
        }
        if tool.needs_admin {
            notes.push("needs administrator");
        }
        if tool.needs_restart {
            notes.push("needs a restart");
        }
        println!("{:<26} {:<34} {}", tool.id, tool.title, notes.join(", "));
    }
    Ok(())
}

/// `tools run <id>`.
pub fn tools_run(json: bool, id: &str, yes: bool) -> CliResult {
    let tool =
        Tool::find(id).ok_or_else(|| format!("no tool is called '{id}' (see `tools list`)"))?;
    if !tool.is_command() {
        return Err(format!(
            "'{}' opens a window; start it from the app instead",
            tool.id
        ));
    }
    if !tool.is_available() {
        return Err(format!("{} is not present on this PC", tool.title));
    }
    if (tool.changes_system || tool.needs_restart)
        && !yes
        && !confirm(&format!("{}: {} Run it?", tool.title, tool.changes))?
    {
        println!("Cancelled. Nothing was changed.");
        return Ok(());
    }
    let run = tools::run(tool, &Progress::new(), &mut |_| {}).map_err(|e| e.to_string())?;
    if json {
        print_json(&run)?;
    } else {
        print!("{}", run.output);
        if !run.output.ends_with('\n') {
            println!();
        }
        println!("{}", run.summary);
    }
    match run.verdict {
        Verdict::Ok | Verdict::RestartNeeded | Verdict::Attention => Ok(()),
        Verdict::NeedsAdmin | Verdict::Failed | Verdict::Cancelled => Err(run.summary),
    }
}
