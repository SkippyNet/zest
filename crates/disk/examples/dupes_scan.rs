//! Read-only smoke test for the duplicate finder.
//!
//! ```text
//! cargo run --release -p zest-disk --example dupes_scan -- C:\Users\me\Documents [min-bytes] [max-bytes]
//! ```
//!
//! Nothing is deleted: the example only prints what was found.

use std::path::PathBuf;
use std::time::Duration;
use zest_core::Progress;
use zest_core::fmt::{format_bytes, format_count, format_duration};
use zest_disk::{DuplicateOptions, KeepStrategy, find_duplicates, select_for_removal};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path: PathBuf = match args.next() {
        Some(arg) => arg.into(),
        None => {
            eprintln!("usage: dupes_scan <folder> [min-bytes] [max-bytes]");
            std::process::exit(2);
        }
    };
    let number = |arg: Option<std::ffi::OsString>| -> Option<u64> {
        arg.and_then(|a| a.to_string_lossy().replace('_', "").parse().ok())
    };
    let mut options = DuplicateOptions::for_roots(vec![path.clone()]);
    if let Some(min) = number(args.next()) {
        options.min_size = min;
    }
    options.max_size = number(args.next());

    println!(
        "Searching {} (min {}, max {}) ...",
        path.display(),
        format_bytes(options.min_size),
        options.max_size.map_or("none".to_owned(), format_bytes),
    );
    let report = match find_duplicates(&options, &Progress::new()) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    println!(
        "\n{} duplicate groups, {} redundant files, {} wasted",
        format_count(report.groups.len() as u64),
        format_count(report.duplicate_files),
        format_bytes(report.wasted_bytes),
    );
    println!(
        "{} files seen, {} candidates, {} read for comparison, {} unreadable, {} hard links merged",
        format_count(report.files_scanned),
        format_count(report.candidates),
        format_bytes(report.bytes_hashed),
        format_count(report.unreadable),
        format_count(report.hard_links_merged),
    );
    println!(
        "finished in {}",
        format_duration(Duration::from_millis(report.elapsed_ms))
    );

    println!("\nBiggest groups");
    for group in report.groups.iter().take(10) {
        println!(
            "  {} x {}  ({} wasted)",
            group.files.len(),
            format_bytes(group.size),
            format_bytes(group.wasted_bytes())
        );
        for file in group.files.iter().take(4) {
            println!("      {}", file.path.display());
        }
        if group.files.len() > 4 {
            println!("      ... and {} more", group.files.len() - 4);
        }
    }

    let would_remove = select_for_removal(&report.groups, &KeepStrategy::Newest);
    println!(
        "\nKeeping the newest copy of each group would remove {} files (nothing was deleted).",
        format_count(would_remove.len() as u64)
    );
}
