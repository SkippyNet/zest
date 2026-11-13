//! Read-only smoke test for the disk analyzer.
//!
//! ```text
//! cargo run --release -p zest-disk --example disk_analyze -- C:\Users\me
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};
use zest_core::Progress;
use zest_core::fmt::{format_bytes, format_count, format_duration};
use zest_disk::{AnalyzeOptions, analyze, list_drives};

fn main() {
    let path: PathBuf = match std::env::args_os().nth(1) {
        Some(arg) => arg.into(),
        None => {
            eprintln!("usage: disk_analyze <folder or drive>");
            std::process::exit(2);
        }
    };

    println!("Drives");
    for drive in list_drives() {
        println!(
            "  {:<22} {:<6} {:<16} {:>10} free of {:>10}  ssd: {:<7} system: {}",
            drive.display_name(),
            drive.file_system,
            drive.kind.label(),
            format_bytes(drive.free_bytes),
            format_bytes(drive.total_bytes),
            drive.is_ssd.map_or("unknown".to_owned(), |s| s.to_string()),
            drive.is_system,
        );
    }

    println!("\nAnalyzing {} ...", path.display());
    let progress = Progress::new();
    let started = Instant::now();
    let analysis = match analyze(&AnalyzeOptions::for_path(&path), &progress) {
        Ok(analysis) => analysis,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();
    let secs = elapsed.as_secs_f64().max(0.001);

    println!(
        "\n{} in {} files and {} folders",
        format_bytes(analysis.total_bytes),
        format_count(analysis.total_files),
        format_count(analysis.total_dirs),
    );
    println!(
        "scanned in {}  ({} files/s, {} entries/s)",
        format_duration(Duration::from_millis(analysis.elapsed_ms)),
        format_count((analysis.total_files as f64 / secs) as u64),
        format_count(((analysis.total_files + analysis.total_dirs) as f64 / secs) as u64),
    );
    println!(
        "links skipped: {}   unreadable folders: {}   cloud-only: {} in {} files",
        format_count(analysis.links_skipped),
        format_count(analysis.inaccessible_dirs),
        format_bytes(analysis.cloud_bytes),
        format_count(analysis.cloud_files),
    );

    println!("\nCategories");
    for stat in &analysis.categories {
        let share = if analysis.total_bytes > 0 {
            stat.bytes as f64 * 100.0 / analysis.total_bytes as f64
        } else {
            0.0
        };
        println!(
            "  {:<13} {:>10}  {:>5.1}%  {:>10} files",
            stat.category.label(),
            format_bytes(stat.bytes),
            share,
            format_count(stat.files),
        );
    }

    println!("\nTop extensions");
    for stat in analysis.extensions.iter().take(12) {
        let name = if stat.extension.is_empty() {
            "(none)".to_owned()
        } else {
            format!(".{}", stat.extension)
        };
        println!(
            "  {:<14} {:<13} {:>10}  {:>10} files",
            name,
            stat.category.label(),
            format_bytes(stat.bytes),
            format_count(stat.files),
        );
    }

    println!("\nLargest files");
    for file in analysis.largest_files.iter().take(12) {
        println!("  {:>10}  {}", format_bytes(file.size), file.path.display());
    }

    let mut nodes = 0u64;
    fn count(node: &zest_disk::FolderNode, nodes: &mut u64) {
        *nodes += 1;
        for child in &node.children {
            count(child, nodes);
        }
    }
    for root in &analysis.tree {
        count(root, &mut nodes);
        println!("\nLargest folders in {}", root.path.display());
        for child in root.children.iter().take(15) {
            println!(
                "  {:>10}  {:>9} files  {}",
                format_bytes(child.bytes),
                format_count(child.files),
                child.name
            );
        }
        if root.pruned_children > 0 {
            println!(
                "  {:>10}  in {} smaller folders",
                format_bytes(root.pruned_bytes),
                format_count(root.pruned_children)
            );
        }
        println!(
            "  {:>10}  in {} files directly in this folder",
            format_bytes(root.own_bytes),
            format_count(root.own_files)
        );
    }
    println!(
        "\nresult tree: {} nodes kept for {} folders on disk",
        format_count(nodes),
        format_count(analysis.total_dirs)
    );
}
