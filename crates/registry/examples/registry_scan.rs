//! Read-only registry scan of this PC.
//!
//! ```text
//! cargo run -p zest-registry --example registry_scan            # summary + first 15 per category
//! cargo run -p zest-registry --example registry_scan -- --all   # every issue
//! cargo run -p zest-registry --example registry_scan -- fonts startup --limit 50
//! ```
//!
//! Nothing is modified: the example only calls `scan`.

use std::time::Instant;
use zest_core::Progress;
use zest_registry::{Category, Confidence, ScanOptions, count_by_category, scan};

fn main() {
    let mut limit = 15usize;
    let mut categories: Vec<Category> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--all" => limit = usize::MAX,
            "--limit" => {
                limit = args.next().and_then(|n| n.parse().ok()).unwrap_or(limit);
            }
            other => match Category::from_slug(other) {
                Some(category) => categories.push(category),
                None => {
                    eprintln!("unknown category '{other}'. Known categories:");
                    for category in Category::ALL {
                        eprintln!("  {}", category.slug());
                    }
                    std::process::exit(2);
                }
            },
        }
    }
    if categories.is_empty() {
        categories = Category::ALL.to_vec();
    }

    // Per-category timing (sequential), so slow scanners stand out.
    println!("Per-category scan (sequential):");
    let mut sequential_total = std::time::Duration::ZERO;
    for &category in &categories {
        let started = Instant::now();
        let found = scan(&ScanOptions::only(&[category]), &Progress::new()).unwrap_or_default();
        let elapsed = started.elapsed();
        sequential_total += elapsed;
        println!(
            "  {:<26} {:>6} issue(s)  {:>8.1} ms",
            category.label(),
            found.len(),
            elapsed.as_secs_f64() * 1000.0
        );
    }
    println!(
        "  {:<26} {:>24.1} ms",
        "total",
        sequential_total.as_secs_f64() * 1000.0
    );

    // The real thing: all selected categories in parallel.
    let options = ScanOptions::only(&categories);
    let progress = Progress::new();
    let started = Instant::now();
    let issues = match scan(&options, &progress) {
        Ok(issues) => issues,
        Err(e) => {
            eprintln!("scan failed: {e}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();

    let safe = issues
        .iter()
        .filter(|i| i.confidence == Confidence::Safe)
        .count();
    let admin = issues.iter().filter(|i| i.requires_admin).count();
    println!();
    println!(
        "Parallel scan: {} issue(s) in {:.1} ms ({} safe, {} to review, {} need administrator rights)",
        issues.len(),
        elapsed.as_secs_f64() * 1000.0,
        safe,
        issues.len() - safe,
        admin
    );

    for (category, count) in count_by_category(&issues) {
        if !categories.contains(&category) {
            continue;
        }
        println!();
        println!("== {} ({count}) ==", category.label());
        println!("   {}", category.description());
        for issue in issues.iter().filter(|i| i.category == category).take(limit) {
            let confidence = match issue.confidence {
                Confidence::Safe => "safe  ",
                Confidence::Review => "review",
            };
            let admin = if issue.requires_admin {
                "admin"
            } else {
                "user "
            };
            println!("   [{confidence}] [{admin}] {}", issue.title);
            println!("        {}", issue.detail);
            println!("        {}", issue.location());
        }
        if count > limit {
            println!("   ... and {} more", count - limit);
        }
    }
}
