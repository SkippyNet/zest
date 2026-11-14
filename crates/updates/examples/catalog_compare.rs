//! Compares the catalog fallback with the Windows Package Manager's own
//! answers on this PC. Read-only: it lists, exports a package list to a
//! temporary file and downloads the public catalog; nothing is installed.
//!
//! `cargo run -p zest-updates --example catalog_compare`
//!
//! Set `ZEST_DATA_DIR` to keep the downloaded catalog out of the real data
//! folder.

use std::collections::{BTreeMap, BTreeSet};
use zest_core::Progress;
use zest_system::apps::{self, ListOptions};
use zest_updates::software::{self, Catalog, CheckOptions, InstalledProgram};

/// Package ids the package manager itself ties to installed programs.
fn exported_ids() -> BTreeSet<String> {
    let Some(exe) = software::winget_path() else {
        return BTreeSet::new();
    };
    let file = std::env::temp_dir().join(format!("zest-compare-{}.json", std::process::id()));
    let _ = zest_core::process::hidden_command(&exe)
        .args(["export", "--output"])
        .arg(&file)
        .args(["--accept-source-agreements", "--disable-interactivity"])
        .output();
    let json = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    let root: serde_json::Value =
        serde_json::from_str(json.trim_start_matches('\u{feff}')).unwrap_or_default();
    let mut ids = BTreeSet::new();
    for source in root["Sources"].as_array().into_iter().flatten() {
        if source["SourceDetails"]["Name"].as_str() != Some("winget") {
            continue;
        }
        for package in source["Packages"].as_array().into_iter().flatten() {
            if let Some(id) = package["PackageIdentifier"].as_str() {
                ids.insert(id.to_owned());
            }
        }
    }
    ids
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let progress = Progress::new();
    let installed = apps::list_installed(&ListOptions::default());
    let programs: Vec<InstalledProgram> = installed.iter().map(InstalledProgram::from).collect();
    println!("installed programs: {}", programs.len());

    let started = std::time::Instant::now();
    let catalog = Catalog::load(&progress)?;
    println!(
        "catalog: {} packages, loaded in {:.1} s (copy is {} min old)",
        catalog.package_count(),
        started.elapsed().as_secs_f32(),
        catalog.age().map_or(0, |age| age.as_secs() / 60)
    );

    // Which package is which program?
    let started = std::time::Instant::now();
    let mut ours: BTreeMap<String, String> = BTreeMap::new();
    for program in &programs {
        if let Some(id) = catalog.package_id(program) {
            ours.entry(id).or_insert_with(|| program.name.clone());
        }
    }
    println!(
        "identified by the catalog reader: {} packages in {:.2} s",
        ours.len(),
        started.elapsed().as_secs_f32()
    );
    let theirs = exported_ids();
    if theirs.is_empty() {
        println!("the package manager is not available for comparison");
    } else {
        let our_ids: BTreeSet<String> = ours.keys().cloned().collect();
        let both = our_ids.intersection(&theirs).count();
        println!(
            "identified by the package manager: {} packages; in common: {both}",
            theirs.len()
        );
        println!("only the package manager:");
        for id in theirs.difference(&our_ids) {
            println!("    {id}");
        }
        println!("only the catalog reader:");
        for id in our_ids.difference(&theirs) {
            println!("    {id}  <- {}", ours[id]);
        }
    }

    // Which programs are out of date?
    let catalog_updates = catalog.updates_for(&programs);
    let listed = software::check_updates(&CheckOptions::default(), &progress);
    println!(
        "\nupdates according to the catalog reader: {}",
        catalog_updates.len()
    );
    match listed {
        Ok(listed) => {
            let winget_only: Vec<_> = listed.iter().filter(|u| u.source != "msstore").collect();
            println!(
                "updates according to the package manager: {} ({} from its community source)",
                listed.len(),
                winget_only.len()
            );
            let theirs: BTreeMap<&str, &software::SoftwareUpdate> =
                winget_only.iter().map(|u| (u.id.as_str(), *u)).collect();
            let ours: BTreeMap<&str, &software::CatalogUpdate> =
                catalog_updates.iter().map(|u| (u.id.as_str(), u)).collect();
            let mut agree = 0;
            for (id, update) in &ours {
                match theirs.get(id) {
                    Some(same) if same.available_version == update.available_version => agree += 1,
                    Some(other) => println!(
                        "    differs  {id}: catalog {} -> {}, package manager {} -> {}",
                        update.installed_version,
                        update.available_version,
                        other.installed_version,
                        other.available_version
                    ),
                    None => println!(
                        "    only catalog  {id}: {} {} -> {} ({:?})",
                        update.name,
                        update.installed_version,
                        update.available_version,
                        update.matched_by
                    ),
                }
            }
            for (id, update) in &theirs {
                if !ours.contains_key(id) {
                    println!(
                        "    only package manager  {id}: {} {} -> {}",
                        update.name, update.installed_version, update.available_version
                    );
                }
            }
            println!("in agreement: {agree}");
        }
        Err(error) => println!("the package manager could not list updates: {error}"),
    }
    Ok(())
}
