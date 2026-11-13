//! ActiveX / COM class issues.
//!
//! A class is reported only when **every** server it registers
//! (`InprocServer32`, `LocalServer32`) is confirmed missing. Classes are
//! skipped when
//!
//! * any server cannot be judged (bare names such as `mscoree.dll` resolve
//!   through the system and are therefore never "missing");
//! * the class is redirected (`TreatAs` / `AutoTreatAs`);
//! * it registers no server at all (AppID-only and interface-only classes);
//! * the missing file would live in a Windows-owned location – Windows
//!   keeps registrations for components that are installed on demand;
//! * the file exists for the other bitness (`Program Files (x86)` instead of
//!   `Program Files`, `SysWOW64` instead of `System32`).

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use rayon::prelude::*;
use std::io::ErrorKind;
use std::path::PathBuf;
use winreg::RegKey;
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::RegPath;

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for clsid_root in super::distinct_children(super::class_roots_with_wow(), "CLSID") {
        scan_root(&clsid_root, progress, out);
    }
}

enum Server {
    /// The sub-key does not exist.
    Absent,
    /// Present, but its target exists or cannot be judged.
    Unknown,
    /// Present and confirmed missing.
    Missing {
        sub_key: &'static str,
        raw: String,
        path: PathBuf,
    },
}

fn server(class: &RegKey, sub_key: &'static str, is_command: bool) -> Server {
    let key = match class.open_subkey_with_flags(sub_key, KEY_READ) {
        Ok(key) => key,
        Err(e) if e.kind() == ErrorKind::NotFound => return Server::Absent,
        Err(_) => return Server::Unknown,
    };
    let Some(raw) = check::raw_string_value(&key, "") else {
        return Server::Unknown;
    };
    let text = raw.trim();
    if text.is_empty() {
        return Server::Unknown;
    }
    let missing = if is_command {
        check::missing_command(text)
    } else {
        check::missing_file(text)
    };
    match check::outside_windows(missing) {
        Some(path) => Server::Missing { sub_key, raw, path },
        None => Server::Unknown,
    }
}

fn sub_key_exists(class: &RegKey, name: &str) -> bool {
    !matches!(
        class.open_subkey_with_flags(name, KEY_READ),
        Err(e) if e.kind() == ErrorKind::NotFound
    )
}

/// Number of classes one worker inspects before taking the next batch.
const BATCH: usize = 512;

/// Scans the class keys directly below a `CLSID`-shaped key.
///
/// A machine typically registers tens of thousands of classes, so the keys
/// are inspected in parallel batches.
pub(crate) fn scan_root(clsid_root: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = clsid_root.open() else {
        return;
    };
    let names: Vec<String> = root
        .enum_keys()
        .flatten()
        .filter(|name| super::looks_like_guid(name))
        .collect();
    drop(root);

    let found: Vec<Issue> = names
        .par_chunks(BATCH)
        .flat_map_iter(|batch| {
            let mut issues = Vec::new();
            // Registry handles are not shared between threads; every batch
            // opens its own.
            let Ok(root) = clsid_root.open() else {
                return issues;
            };
            for (index, name) in batch.iter().enumerate() {
                if index % 64 == 0 && progress.is_cancelled() {
                    break;
                }
                if let Some(issue) = inspect_class(&root, clsid_root, name) {
                    issues.push(issue);
                }
            }
            issues
        })
        .collect();
    out.extend(found);
}

/// Judges one class key.
fn inspect_class(root: &RegKey, clsid_root: &RegPath, name: &str) -> Option<Issue> {
    let class = root.open_subkey_with_flags(name, KEY_READ).ok()?;
    if sub_key_exists(&class, "TreatAs") || sub_key_exists(&class, "AutoTreatAs") {
        return None;
    }

    let servers = [
        server(&class, "InprocServer32", false),
        server(&class, "LocalServer32", true),
    ];
    if servers.iter().any(|s| matches!(s, Server::Unknown)) {
        return None;
    }
    let class_path = clsid_root.child(name);
    let mut conditions = Vec::new();
    let mut first_missing: Option<PathBuf> = None;
    for found in servers {
        if let Server::Missing { sub_key, raw, path } = found {
            conditions.push(Condition::ValueEquals {
                key: class_path.child(sub_key),
                name: String::new(),
                data: raw,
            });
            conditions.push(Condition::PathMissing(path.clone()));
            first_missing.get_or_insert(path);
        }
    }
    // `None` here means the class registers no server at all.
    let missing = first_missing?;

    let detail = match check::string_value(&class, "") {
        Some(friendly) => format!("{} ({friendly})", missing.display()),
        None => missing.display().to_string(),
    };
    Some(Issue::for_key(
        Category::ComClasses,
        &class_path,
        "Component refers to a missing file",
        detail,
        Confidence::Safe,
        conditions,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    const DEAD: &str = "{11111111-1111-1111-1111-111111111111}";
    const DEAD_LOCAL: &str = "{22222222-2222-2222-2222-222222222222}";
    const ALIVE: &str = "{33333333-3333-3333-3333-333333333333}";
    const HALF: &str = "{44444444-4444-4444-4444-444444444444}";
    const DOTNET: &str = "{55555555-5555-5555-5555-555555555555}";
    const TREAT_AS: &str = "{66666666-6666-6666-6666-666666666666}";
    const APP_ID_ONLY: &str = "{77777777-7777-7777-7777-777777777777}";
    const WINDOWS: &str = "{88888888-8888-8888-8888-888888888888}";
    const BOTH_DEAD: &str = "{99999999-9999-9999-9999-999999999999}";
    const EMPTY_SERVER: &str = "{AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA}";

    #[test]
    fn only_fully_dead_classes_are_reported() {
        let scratch = Scratch::new("com");
        let root = scratch.key("CLSID");
        let class = |guid: &str| root.child(guid);

        scratch.set(&class(DEAD), "", "Dead Control");
        scratch.set(&class(DEAD).child("InprocServer32"), "", &gone("dead.dll"));

        scratch.set(
            &class(DEAD_LOCAL).child("LocalServer32"),
            "",
            &format!("\"{}\" -Embedding", gone("server.exe")),
        );

        scratch.set(&class(ALIVE).child("InprocServer32"), "", &existing_file());

        // One server alive, one dead: ambiguous, skipped.
        scratch.set(&class(HALF).child("InprocServer32"), "", &gone("half.dll"));
        scratch.set(&class(HALF).child("LocalServer32"), "", &existing_file());

        // Managed classes are hosted by a system DLL referenced by bare name.
        scratch.set(&class(DOTNET).child("InprocServer32"), "", "mscoree.dll");

        scratch.set(
            &class(TREAT_AS).child("InprocServer32"),
            "",
            &gone("old.dll"),
        );
        scratch.set(&class(TREAT_AS).child("TreatAs"), "", ALIVE);

        scratch.set(
            &class(APP_ID_ONLY),
            "AppID",
            "{00000000-0000-0000-0000-000000000001}",
        );

        let in_windows = zest_core::paths::windows_dir()
            .join(r"System32\zest-optional-feature.dll")
            .display()
            .to_string();
        scratch.set(&class(WINDOWS).child("InprocServer32"), "", &in_windows);

        scratch.set(
            &class(BOTH_DEAD).child("InprocServer32"),
            "",
            &gone("a.dll"),
        );
        scratch.set(&class(BOTH_DEAD).child("LocalServer32"), "", &gone("b.exe"));

        scratch.key(&format!(r"CLSID\{EMPTY_SERVER}\InprocServer32"));

        // Not a class id.
        scratch.set(
            &root.child("NotAGuid").child("InprocServer32"),
            "",
            &gone("x.dll"),
        );

        let mut out = Vec::new();
        scan_root(&root, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().map(|i| i.key.leaf()).collect();
        found.sort();
        assert_eq!(found, vec![DEAD, DEAD_LOCAL, BOTH_DEAD], "{out:#?}");

        let dead = out.iter().find(|i| i.key.leaf() == DEAD).unwrap();
        assert_eq!(dead.value, None);
        assert!(dead.detail.contains("dead.dll") && dead.detail.contains("Dead Control"));
        assert!(out.iter().all(Issue::still_valid));

        // Re-registering the class with a live file invalidates the issue.
        scratch.set(&class(DEAD).child("InprocServer32"), "", &existing_file());
        assert!(!dead.still_valid());
    }
}
