//! Type libraries.
//!
//! Layout: `TypeLib\{GUID}\<version>\<lcid>\win32|win64` with the library
//! path as default value. A library is reported only when it registers at
//! least one path and **all** of its paths, across every version and
//! language, are confirmed missing. The whole `{GUID}` key is then removed.
//!
//! Paths may carry a resource index (`C:\App\app.exe\3`); the index is
//! stripped before the file is looked up. Files in Windows-owned locations
//! are never judged, and a library that exists for the other bitness
//! (`Program Files (x86)` instead of `Program Files`) is not missing.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use std::path::PathBuf;
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::cmdline::{self, Target};
use zest_core::reg::RegPath;

const PLATFORMS: [&str; 3] = ["win32", "win64", "win16"];

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    for typelib_root in super::distinct_children(super::class_roots_with_wow(), "TypeLib") {
        scan_root(&typelib_root, progress, out);
    }
}

/// Removes a trailing `\<number>` resource index, if present.
fn strip_resource_number(path: &str) -> Option<&str> {
    let (head, tail) = path.rsplit_once('\\')?;
    (!tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())).then_some(head)
}

/// `Some(Some(path))`: confirmed missing. `Some(None)`: exists.
/// `None`: cannot be judged.
fn judge(raw: &str) -> Option<Option<PathBuf>> {
    let text = raw.trim().trim_matches('"').trim();
    if text.is_empty() {
        return None;
    }
    let mut target = cmdline::check_path(text);
    if target.is_missing()
        && let Some(stripped) = strip_resource_number(text)
    {
        target = cmdline::check_path(stripped);
    }
    match target {
        Target::Exists(_) => Some(None),
        Target::Unknown => None,
        Target::Missing(path) => {
            check::outside_windows(check::confirmed(Target::Missing(path))).map(Some)
        }
    }
}

/// Scans the library keys directly below a `TypeLib`-shaped key.
pub(crate) fn scan_root(typelib_root: &RegPath, progress: &Progress, out: &mut Vec<Issue>) {
    let Ok(root) = typelib_root.open() else {
        return;
    };
    'libraries: for (index, guid) in root.enum_keys().flatten().enumerate() {
        if index % 64 == 0 && progress.is_cancelled() {
            return;
        }
        if !super::looks_like_guid(&guid) {
            continue;
        }
        let Ok(library) = root.open_subkey_with_flags(&guid, KEY_READ) else {
            continue;
        };
        let library_path = typelib_root.child(&guid);
        let mut conditions = Vec::new();
        let mut first_missing: Option<PathBuf> = None;
        let mut friendly: Option<String> = None;

        for version in library.enum_keys().flatten() {
            let Ok(version_key) = library.open_subkey_with_flags(&version, KEY_READ) else {
                continue 'libraries;
            };
            if friendly.is_none() {
                friendly = check::string_value(&version_key, "");
            }
            for lcid in version_key.enum_keys().flatten() {
                // Language keys are hexadecimal numbers; FLAGS and HELPDIR are not.
                if lcid.is_empty() || !lcid.chars().all(|c| c.is_ascii_hexdigit()) {
                    continue;
                }
                let Ok(lcid_key) = version_key.open_subkey_with_flags(&lcid, KEY_READ) else {
                    continue 'libraries;
                };
                for platform in PLATFORMS {
                    let Ok(platform_key) = lcid_key.open_subkey_with_flags(platform, KEY_READ)
                    else {
                        continue;
                    };
                    let Some(raw) = check::raw_string_value(&platform_key, "") else {
                        continue 'libraries;
                    };
                    match judge(&raw) {
                        Some(Some(path)) => {
                            conditions.push(Condition::ValueEquals {
                                key: library_path.child(&version).child(&lcid).child(platform),
                                name: String::new(),
                                data: raw,
                            });
                            conditions.push(Condition::PathMissing(path.clone()));
                            first_missing.get_or_insert(path);
                        }
                        // One live or unjudgeable path keeps the whole library.
                        _ => continue 'libraries,
                    }
                }
            }
        }

        let Some(missing) = first_missing else {
            continue;
        };
        let detail = match friendly {
            Some(friendly) => format!("{} ({friendly})", missing.display()),
            None => missing.display().to_string(),
        };
        out.push(Issue::for_key(
            Category::TypeLibraries,
            &library_path,
            "Type library refers to a missing file",
            detail,
            Confidence::Safe,
            conditions,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Scratch, existing_file, gone};

    const DEAD: &str = "{11111111-1111-1111-1111-111111111111}";
    const ALIVE: &str = "{22222222-2222-2222-2222-222222222222}";
    const MIXED: &str = "{33333333-3333-3333-3333-333333333333}";
    const INDEXED_ALIVE: &str = "{44444444-4444-4444-4444-444444444444}";
    const INDEXED_DEAD: &str = "{55555555-5555-5555-5555-555555555555}";
    const NO_PATHS: &str = "{66666666-6666-6666-6666-666666666666}";
    const WINDOWS: &str = "{77777777-7777-7777-7777-777777777777}";

    #[test]
    fn resource_numbers() {
        assert_eq!(
            strip_resource_number(r"C:\App\app.exe\3"),
            Some(r"C:\App\app.exe")
        );
        assert_eq!(strip_resource_number(r"C:\App\app.tlb"), None);
        assert_eq!(strip_resource_number(r"C:\App\"), None);
        assert_eq!(strip_resource_number("nothing"), None);
    }

    #[test]
    fn library_must_be_entirely_dead() {
        let scratch = Scratch::new("typelib");
        let root = scratch.key("TypeLib");
        let lib = |guid: &str, rest: &str| root.child(guid).child(rest);

        scratch.set(&lib(DEAD, "1.0"), "", "Dead Library");
        scratch.set(&lib(DEAD, r"1.0\0\win32"), "", &gone("dead.tlb"));
        scratch.set(&lib(DEAD, r"1.0\0\win64"), "", &gone("dead64.tlb"));
        scratch.set(&lib(DEAD, r"1.0\FLAGS"), "", "0");
        scratch.set(&lib(DEAD, r"1.0\HELPDIR"), "", &gone(""));
        scratch.set(&lib(DEAD, r"2.0\409\win32"), "", &gone("dead2.tlb"));

        scratch.set(&lib(ALIVE, r"1.0\0\win32"), "", &existing_file());

        // One version dead, another alive: keep.
        scratch.set(&lib(MIXED, r"1.0\0\win32"), "", &gone("old.tlb"));
        scratch.set(&lib(MIXED, r"2.0\0\win32"), "", &existing_file());

        scratch.set(
            &lib(INDEXED_ALIVE, r"1.0\0\win32"),
            "",
            &format!(r"{}\2", existing_file()),
        );
        scratch.set(
            &lib(INDEXED_DEAD, r"1.0\0\win32"),
            "",
            &format!(r"{}\2", gone("res.dll")),
        );

        scratch.set(&lib(NO_PATHS, "1.0"), "", "Nothing registered");

        let in_windows = zest_core::paths::windows_dir()
            .join(r"System32\zest-optional.tlb")
            .display()
            .to_string();
        scratch.set(&lib(WINDOWS, r"1.0\0\win32"), "", &in_windows);

        let mut out = Vec::new();
        scan_root(&root, &Progress::new(), &mut out);
        let mut found: Vec<&str> = out.iter().map(|i| i.key.leaf()).collect();
        found.sort();
        assert_eq!(found, vec![DEAD, INDEXED_DEAD], "{out:#?}");

        let dead = out.iter().find(|i| i.key.leaf() == DEAD).unwrap();
        assert!(dead.detail.contains("Dead Library"), "{}", dead.detail);
        // Three paths, each with a data and a path condition.
        assert_eq!(dead.conditions.len(), 6);
        assert!(out.iter().all(Issue::still_valid));

        let indexed = out.iter().find(|i| i.key.leaf() == INDEXED_DEAD).unwrap();
        assert!(indexed.detail.ends_with("res.dll"), "{}", indexed.detail);
    }
}
