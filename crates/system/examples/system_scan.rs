//! Read-only tour of everything `zest-system` can enumerate.
//!
//! ```text
//! cargo run -p zest-system --example system_scan
//! cargo run -p zest-system --example system_scan -- --all     (full lists)
//! cargo run -p zest-system --example system_scan -- --json    (machine readable)
//! ```
//!
//! Nothing on the PC is changed.

use std::time::Instant;
use zest_core::Progress;
use zest_core::fmt::format_bytes;
use zest_system::apps::{self, AppSource, ListOptions};
use zest_system::restore;
use zest_system::startup::{self, StartupDetails, StartupItem, StartupKind};

fn flags(item: &StartupItem) -> String {
    let mut out = String::new();
    out.push_str(if item.enabled { "on " } else { "OFF" });
    out.push_str(if item.target_missing { " MISSING" } else { "" });
    out.push_str(if item.is_microsoft { " ms" } else { "" });
    out.push_str(if item.requires_admin { " admin" } else { "" });
    out
}

fn show(title: &str, items: &[&StartupItem], limit: usize) {
    let enabled = items.iter().filter(|i| i.enabled).count();
    let missing = items.iter().filter(|i| i.target_missing).count();
    let microsoft = items.iter().filter(|i| i.is_microsoft).count();
    println!(
        "\n== {title}: {} ({} enabled, {} disabled, {} missing target, {} Microsoft)",
        items.len(),
        enabled,
        items.len() - enabled,
        missing,
        microsoft
    );
    for item in items.iter().take(limit) {
        println!("  [{:<16}] {}", flags(item), item.name);
        if !item.command.is_empty() {
            println!("      {}", item.command);
        }
        match &item.details {
            StartupDetails::Task(task) => println!(
                "      {} | triggers: {} | state: {}",
                task.path,
                task.triggers.join(", "),
                task.state.label()
            ),
            StartupDetails::Service(service) => println!(
                "      {} | {} | {}",
                service.service_name,
                service.start_type.label(),
                service.state.label()
            ),
            StartupDetails::ContextMenu(entry) => {
                println!("      {} | {}", entry.applies_to, item.location);
            }
            _ => println!("      {}", item.location),
        }
    }
    if items.len() > limit {
        println!("  ... and {} more", items.len() - limit);
    }
}

/// `--json`: everything as one JSON document, for comparing against other tools.
fn dump_json() {
    let startup = startup::list_all(&Progress::new()).unwrap_or_default();
    let apps = apps::list_installed(&ListOptions::everything());
    let document = serde_json::json!({ "startup": startup, "apps": apps });
    println!("{document}");
}

/// `--timing`: how long each source takes on its own.
fn timing() {
    let time = |label: &str, count: &dyn Fn() -> usize| {
        let started = Instant::now();
        let found = count();
        println!("{label:<28} {found:>5} items in {:?}", started.elapsed());
    };
    time("startup programs", &|| {
        startup::list_startup_programs().len()
    });
    time("scheduled tasks", &|| {
        startup::list_scheduled_tasks().map_or(0, |v| v.len())
    });
    time("services", &|| {
        startup::list_services().map_or(0, |v| v.len())
    });
    time("context menu", &|| startup::list_context_menu().len());
    time("everything (one call)", &|| {
        startup::list_all(&Progress::new()).map_or(0, |v| v.len())
    });
    time("desktop apps", &|| {
        apps::list_desktop_apps(&ListOptions::default()).len()
    });
    time("store apps", &|| apps::list_store_apps().len());
}

fn main() {
    if std::env::args().any(|a| a == "--json") {
        dump_json();
        return;
    }
    if std::env::args().any(|a| a == "--timing") {
        timing();
        return;
    }
    let all = std::env::args().any(|a| a == "--all");
    let limit = if all { usize::MAX } else { 8 };

    let started = Instant::now();
    let items = match startup::list_all(&Progress::new()) {
        Ok(items) => items,
        Err(e) => {
            println!("startup enumeration failed: {e}");
            Vec::new()
        }
    };
    let startup_time = started.elapsed();
    for kind in StartupKind::ALL {
        let of_kind: Vec<&StartupItem> = items.iter().filter(|i| i.kind == kind).collect();
        show(kind.label(), &of_kind, limit);
    }
    let third_party_tasks = items
        .iter()
        .filter(|i| i.kind == StartupKind::ScheduledTask && !i.is_microsoft)
        .count();
    let third_party_services = items
        .iter()
        .filter(|i| i.kind == StartupKind::Service && !i.is_microsoft)
        .count();
    println!(
        "\nNon-Microsoft: {third_party_tasks} tasks, {third_party_services} services. Startup enumeration took {startup_time:?}."
    );

    let started = Instant::now();
    let desktop = apps::list_desktop_apps(&ListOptions::default());
    let desktop_time = started.elapsed();
    let started = Instant::now();
    let store = apps::list_store_apps();
    let store_time = started.elapsed();
    let everything = apps::list_desktop_apps(&ListOptions::everything());
    println!(
        "\n== Installed programs: {} desktop ({} incl. hidden components/updates) in {desktop_time:?}, {} Store in {store_time:?}",
        desktop.len(),
        everything.len(),
        store.len()
    );
    let sized: u64 = desktop.iter().filter_map(|a| a.size_bytes).sum();
    println!(
        "   declared size of desktop programs: {} | MSI: {} | 32-bit: {} | per-user: {}",
        format_bytes(sized),
        desktop.iter().filter(|a| a.is_msi).count(),
        desktop.iter().filter(|a| !a.is_64bit).count(),
        desktop
            .iter()
            .filter(|a| a.scope == zest_system::Scope::User)
            .count()
    );
    for app in desktop
        .iter()
        .chain(store.iter())
        .take(if all { usize::MAX } else { 12 })
    {
        println!(
            "  [{}] {} {} | {} | {} | {}",
            if app.source == AppSource::Store {
                "store"
            } else {
                "win32"
            },
            app.name,
            app.version.as_deref().unwrap_or("-"),
            app.publisher.as_deref().unwrap_or("-"),
            app.install_date
                .map(|d| d.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            app.size_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "-".to_owned()),
        );
    }
    if all {
        println!("\n   -- Store apps --");
        for app in &store {
            println!(
                "  {} {} | {}",
                app.name,
                app.version.as_deref().unwrap_or("-"),
                app.package_family_name.as_deref().unwrap_or("-")
            );
        }
    }

    println!("\n== System Restore");
    match restore::is_protection_enabled() {
        Ok(enabled) => println!("  protection enabled: {enabled}"),
        Err(e) => println!("  protection state: {e}"),
    }
    match restore::list_restore_points() {
        Ok(points) => {
            println!("  {} restore points", points.len());
            for point in points.iter().rev().take(5) {
                println!(
                    "  #{} {} | {} | {}",
                    point.sequence_number,
                    point
                        .created
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".to_owned()),
                    point.kind.label(),
                    point.description
                );
            }
        }
        Err(e) => println!("  restore points: {e}"),
    }
}
