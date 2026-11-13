//! Read-only smoke test for the performance optimizer.
//!
//! Samples the system for about three seconds and prints what the optimizer
//! sees. Nothing is throttled, suspended, trimmed, ended or reconfigured.
//!
//! ```text
//! cargo run -p zest-performance --example perf_scan
//! ```

use std::time::{Duration, Instant};
use zest_core::fmt::{format_bytes, format_duration};
use zest_performance::classify::AppKind;
use zest_performance::monitor::{AppUsage, Sampler, WindowState};
use zest_performance::power;

fn describe(app: &AppUsage) -> String {
    let role = match (app.protection, app.window) {
        (Some(reason), _) if reason.is_permanent() => format!("protected: {}", reason.label()),
        (_, WindowState::Foreground) => "foreground".to_owned(),
        (_, WindowState::Visible) => "app (on screen)".to_owned(),
        (_, WindowState::Background) => "app (minimized/hidden)".to_owned(),
        (_, WindowState::None) => "background".to_owned(),
    };
    let mut text = format!("{role}, impact {}", app.impact.label());
    if app.recommended {
        text.push_str(", RECOMMENDED");
    }
    if app.suspended {
        text.push_str(", suspended");
    }
    if app.requires_admin {
        text.push_str(", needs admin");
    }
    text
}

fn row(app: &AppUsage) {
    println!(
        "  {:<34} {:>5.1}% {:>5.1}% {:>10} {:>10}/s {:>3}p  {}",
        truncate(&app.name, 34),
        app.cpu_percent,
        app.avg_cpu_percent,
        format_bytes(app.memory),
        format_bytes(app.io_bytes_per_sec),
        app.process_count(),
        describe(app)
    );
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max - 1).collect();
        format!("{cut}…")
    }
}

fn main() -> Result<(), zest_core::Error> {
    let summary = power::system_summary();
    println!("== System ==");
    println!("  {} ({})", summary.computer_name, summary.os_name);
    println!(
        "  Version {} build {}",
        summary.os_version, summary.os_build
    );
    println!(
        "  {} - {} cores / {} threads",
        summary.cpu_name, summary.physical_cores, summary.logical_cores
    );
    println!(
        "  RAM {} total, {} available",
        format_bytes(summary.total_memory),
        format_bytes(summary.available_memory)
    );
    println!(
        "  {} {} free of {}",
        summary.system_drive,
        format_bytes(summary.system_drive_free),
        format_bytes(summary.system_drive_total)
    );
    println!("  Up for {}", format_duration(summary.uptime));
    println!("  Power: {:?}", summary.power);

    // `--burn` keeps one thread of this program busy, as a calibration aid:
    // this program must then show up at roughly 100 / <logical processors> %.
    if std::env::args().any(|a| a == "--burn") {
        std::thread::spawn(|| {
            let mut x = 0u64;
            loop {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                std::hint::black_box(x);
            }
        });
    }

    let mut sampler = Sampler::new();
    let started = Instant::now();
    sampler.sample()?;
    let first_tick = started.elapsed();

    let mut warm_ticks = Vec::new();
    let mut snapshot = None;
    for _ in 0..3 {
        std::thread::sleep(Duration::from_secs(1));
        let started = Instant::now();
        snapshot = Some(sampler.sample()?);
        warm_ticks.push(started.elapsed());
    }
    let Some(snapshot) = snapshot else {
        return Ok(());
    };

    println!("\n== Snapshot ==");
    println!(
        "  CPU {:.1}%   Memory {} of {} ({:.0}%)   Commit {} of {}",
        snapshot.cpu_percent,
        format_bytes(snapshot.memory.used),
        format_bytes(snapshot.memory.total),
        snapshot.memory.used_percent,
        format_bytes(snapshot.memory.commit_total),
        format_bytes(snapshot.memory.commit_limit)
    );
    println!(
        "  {} processes, {} threads, {} handles",
        snapshot.process_count, snapshot.thread_count, snapshot.handle_count
    );
    let cores: Vec<String> = snapshot
        .per_core
        .iter()
        .map(|c| format!("{c:.0}"))
        .collect();
    println!("  Per core %: {}", cores.join(" "));

    let apps = &snapshot.apps;
    let system = apps.iter().filter(|a| a.kind == AppKind::System).count();
    let windowed = apps.iter().filter(|a| a.kind == AppKind::Windowed).count();
    let background = apps
        .iter()
        .filter(|a| a.kind == AppKind::Background)
        .count();
    println!(
        "  {} apps: {} protected/system, {} with windows, {} background, {} recommended",
        apps.len(),
        system,
        windowed,
        background,
        apps.iter().filter(|a| a.recommended).count()
    );

    let header = || {
        println!(
            "  {:<34} {:>6} {:>6} {:>10} {:>12} {:>4}  classification",
            "name", "cpu", "avg", "memory", "i/o", "proc"
        )
    };

    println!("\n== Top by CPU ==");
    header();
    let mut by_cpu: Vec<&AppUsage> = apps.iter().collect();
    by_cpu.sort_by(|a, b| b.avg_cpu_percent.total_cmp(&a.avg_cpu_percent));
    by_cpu.iter().take(12).for_each(|a| row(a));

    println!("\n== Top by memory ==");
    header();
    apps.iter().take(15).for_each(row);

    println!("\n== Controllable apps (not protected) ==");
    header();
    apps.iter()
        .filter(|a| a.protection.is_none_or(|r| !r.is_permanent()))
        .take(25)
        .for_each(row);

    println!("\n== Recommended for sleep ==");
    let recommended = snapshot.recommended();
    if recommended.is_empty() {
        println!("  (none)");
    }
    for app in recommended {
        row(app);
        if let Some(path) = &app.exe_path {
            println!("      {}", path.display());
        }
    }

    println!("\n== Power plans ==");
    for plan in power::list_power_plans()? {
        println!(
            "  {} {} ({})",
            if plan.active { "*" } else { " " },
            plan.name,
            plan.guid
        );
    }
    println!("  Settings: {:?}", power::system_tweaks());

    println!(
        "  Security product folders: {:?}",
        zest_performance::classify::security_product_folders()
    );

    // `--verify` prints raw figures for cross-checking against other tools.
    if std::env::args().any(|a| a == "--verify") {
        println!("\n== Verify (name|pids|working set|private bytes|private working set) ==");
        for app in apps.iter().take(8) {
            let pids: Vec<String> = app.pids().iter().map(u32::to_string).collect();
            println!(
                "VERIFY|{}|{}|{}|{}|{}",
                app.exe_name,
                pids.join(","),
                app.working_set,
                app.private_bytes,
                app.memory
            );
        }
        println!(
            "VERIFYSYS|{}|{}|{}",
            snapshot.memory.total,
            snapshot.memory.available,
            snapshot
                .boot_time
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        );
    }

    println!("\n== Cost ==");
    println!("  first tick: {:.1} ms", first_tick.as_secs_f64() * 1000.0);
    for (i, tick) in warm_ticks.iter().enumerate() {
        println!(
            "  warm tick {}: {:.2} ms",
            i + 1,
            tick.as_secs_f64() * 1000.0
        );
    }
    Ok(())
}
