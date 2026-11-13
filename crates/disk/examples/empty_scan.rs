//! Read-only smoke test for the empty folder finder.
//!
//! ```text
//! cargo run --release -p zest-disk --example empty_scan -- C:\Users\me\Documents
//! ```
//!
//! Nothing is deleted: the example only prints what was found.

use std::path::PathBuf;
use std::time::Instant;
use zest_core::Progress;
use zest_core::fmt::format_count;
use zest_disk::{EmptyFolderOptions, find_empty_folders};

fn main() {
    let path: PathBuf = match std::env::args_os().nth(1) {
        Some(arg) => arg.into(),
        None => {
            eprintln!("usage: empty_scan <folder>");
            std::process::exit(2);
        }
    };
    let progress = Progress::new();
    let started = Instant::now();
    let found = match find_empty_folders(
        &EmptyFolderOptions::for_roots(vec![path.clone()]),
        &progress,
    ) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "{} empty folders below {} ({} folders examined in {:.2?})",
        format_count(found.len() as u64),
        path.display(),
        format_count(progress.done()),
        started.elapsed()
    );
    for folder in found.iter().take(25) {
        println!("  {}", folder.display());
    }
    if found.len() > 25 {
        println!("  ... and {} more", found.len() - 25);
    }
}
