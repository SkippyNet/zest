//! Unused file extensions.
//!
//! Two shapes are reported, both only for extension keys **without any
//! sub-keys** (no "open with" lists, no shell-new data, no handlers):
//!
//! * the key holds no values at all – an empty shell;
//! * the key's only value is a default value naming a file type (ProgID)
//!   that is not registered anywhere. Per-user entries of this kind are
//!   considered safe; machine-wide ones are offered for review only.
//!
//! Extension keys carrying anything else (content type, perceived type, ...)
//! are left alone.

use crate::check;
use crate::types::{Category, Condition, Confidence, Issue};
use winreg::enums::KEY_READ;
use zest_core::Progress;
use zest_core::reg::{RegPath, RegRoot};

pub(crate) fn scan(progress: &Progress, out: &mut Vec<Issue>) {
    // File types are looked up in the merged view: a type registered for the
    // machine satisfies an extension registered for the user and vice versa.
    let lookup = [RegPath::new(RegRoot::Hkcr, "")];
    for classes in super::class_roots() {
        scan_root(&classes, &lookup, progress, out);
    }
}

/// Scans the extension keys directly below `classes`. File types named by an
/// extension are searched below each of `type_roots`.
pub(crate) fn scan_root(
    classes: &RegPath,
    type_roots: &[RegPath],
    progress: &Progress,
    out: &mut Vec<Issue>,
) {
    let Ok(root) = classes.open() else {
        return;
    };
    for name in root.enum_keys().flatten() {
        if progress.is_cancelled() {
            return;
        }
        if !name.starts_with('.') || name.len() < 2 {
            continue;
        }
        let Ok(ext) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        let Ok(info) = ext.query_info() else {
            continue;
        };
        if info.sub_keys != 0 {
            continue;
        }
        let ext_path = classes.child(&name);
        let shape = Condition::KeyShape {
            key: ext_path.clone(),
            sub_keys: 0,
            values: info.values,
        };

        if info.values == 0 {
            out.push(Issue::for_key(
                Category::FileExtensions,
                &ext_path,
                "File extension entry is empty",
                name.clone(),
                Confidence::Safe,
                vec![shape],
            ));
            continue;
        }

        if info.values != 1 {
            continue;
        }
        let Some(raw) = check::raw_string_value(&ext, "") else {
            continue;
        };
        let prog_id = raw.trim();
        // A ProgID is a plain name; anything path-like is not ours to judge.
        if prog_id.is_empty() || prog_id.contains(['\\', '/', '%']) {
            continue;
        }
        let candidates: Vec<RegPath> = type_roots.iter().map(|r| r.child(prog_id)).collect();
        if candidates.iter().all(check::key_definitely_absent) {
            let mut conditions = vec![
                shape,
                Condition::ValueEquals {
                    key: ext_path.clone(),
                    name: String::new(),
                    data: raw.clone(),
                },
            ];
            conditions.extend(candidates.into_iter().map(Condition::KeyMissing));
            // Machine-wide extensions may be shipped with Windows for file
            // types that an optional feature registers later, so those are
            // offered for review only.
            let confidence = if ext_path.root == RegRoot::Hkcu {
                Confidence::Safe
            } else {
                Confidence::Review
            };
            out.push(Issue::for_key(
                Category::FileExtensions,
                &ext_path,
                "File extension refers to a file type that does not exist",
                format!("{name} -> {prog_id}"),
                confidence,
                conditions,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    #[test]
    fn empty_and_dangling_extensions() {
        let scratch = Scratch::new("file-ext");
        let classes = scratch.key("Classes");
        // Empty shell.
        scratch.key(r"Classes\.emptyext");
        // Dangling file type.
        scratch.set(&classes.child(".dangling"), "", "Vendor.Gone.1");
        // Valid: the file type exists.
        scratch.set(&classes.child(".valid"), "", "Vendor.Present.1");
        scratch.key(r"Classes\Vendor.Present.1");
        // Dangling type but with extra data: left alone.
        scratch.set(&classes.child(".mime"), "", "Vendor.Gone.2");
        scratch.set(&classes.child(".mime"), "Content Type", "text/x-thing");
        // Empty but with a sub-key: left alone.
        scratch.key(r"Classes\.withsub\OpenWithProgids");
        // Only a non-default value: left alone.
        scratch.set(&classes.child(".perceived"), "PerceivedType", "text");
        // Not an extension at all.
        scratch.key(r"Classes\NotAnExtension");

        let mut out = Vec::new();
        scan_root(
            &classes,
            std::slice::from_ref(&classes),
            &Progress::new(),
            &mut out,
        );
        let mut found: Vec<(String, String)> = out
            .iter()
            .map(|i| (i.key.leaf().to_owned(), i.title.clone()))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                (
                    ".dangling".to_owned(),
                    "File extension refers to a file type that does not exist".to_owned()
                ),
                (
                    ".emptyext".to_owned(),
                    "File extension entry is empty".to_owned()
                ),
            ],
            "{out:#?}"
        );
        assert!(out.iter().all(|i| i.value.is_none() && i.still_valid()));
    }

    #[test]
    fn conditions_notice_later_changes() {
        let scratch = Scratch::new("file-ext-change");
        let classes = scratch.key("Classes");
        scratch.key(r"Classes\.emptyext");
        scratch.set(&classes.child(".dangling"), "", "Vendor.Gone.1");

        let mut out = Vec::new();
        scan_root(
            &classes,
            std::slice::from_ref(&classes),
            &Progress::new(),
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(Issue::still_valid));

        // The file type gets registered, and the empty key gains a value.
        scratch.key(r"Classes\Vendor.Gone.1");
        scratch.set(&classes.child(".emptyext"), "", "Something.New");
        assert!(out.iter().all(|i| !i.still_valid()), "{out:#?}");
    }
}
