//! Physical key identity.
//!
//! The same registry key can be reachable under several names: 64-bit
//! Windows shares some keys between the 32-bit and 64-bit views and exposes
//! them below `WOW6432Node` through registry links. Scanning both names
//! would report every entry twice, so scanners de-duplicate their roots by
//! the kernel's own name for the opened key.

use winreg::RegKey;
use zest_core::reg::RegPath;

#[link(name = "ntdll")]
unsafe extern "system" {
    /// Queries information about an open registry key.
    fn NtQueryKey(
        key_handle: *mut core::ffi::c_void,
        key_information_class: u32,
        key_information: *mut core::ffi::c_void,
        length: u32,
        result_length: *mut u32,
    ) -> i32;
}

/// `KeyNameInformation`: the full kernel path of the key, links resolved.
const KEY_NAME_INFORMATION: u32 = 3;

/// The kernel's lower-cased name of an open key (for example
/// `\registry\machine\software\classes\typelib`), with links resolved.
///
/// Returns `None` when the name cannot be determined.
pub(crate) fn physical_name(key: &RegKey) -> Option<String> {
    let handle: *mut core::ffi::c_void = key.raw_handle();

    // First call: ask how much room the name needs.
    let mut needed = 0u32;
    // SAFETY: a null buffer of length zero is explicitly allowed; the call
    // only writes the required size into `needed`.
    unsafe {
        NtQueryKey(
            handle,
            KEY_NAME_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if !(4..=64 * 1024).contains(&needed) {
        return None;
    }

    // KEY_NAME_INFORMATION is { ULONG NameLength; WCHAR Name[]; }. A u32
    // buffer provides the alignment the structure requires.
    let mut buffer = vec![0u32; (needed as usize).div_ceil(4)];
    let capacity = (buffer.len() * 4) as u32;
    // SAFETY: `buffer` is valid for `capacity` bytes and outlives the call.
    let status = unsafe {
        NtQueryKey(
            handle,
            KEY_NAME_INFORMATION,
            buffer.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    };
    if status < 0 {
        return None;
    }

    let name_bytes = buffer[0] as usize;
    if name_bytes + 4 > buffer.len() * 4 {
        return None;
    }
    let units: Vec<u16> = buffer[1..]
        .iter()
        .flat_map(|word| [(*word & 0xFFFF) as u16, (*word >> 16) as u16])
        .take(name_bytes / 2)
        .collect();
    Some(String::from_utf16_lossy(&units).to_lowercase())
}

/// Keeps the first of every group of roots that are really the same key.
/// Roots that do not exist (or cannot be opened) are dropped.
pub(crate) fn distinct_roots(roots: impl IntoIterator<Item = RegPath>) -> Vec<RegPath> {
    let mut seen: Vec<String> = Vec::new();
    let mut distinct = Vec::new();
    for root in roots {
        let Ok(key) = root.open() else {
            continue;
        };
        // Without a physical name fall back to the spelled name, which can
        // only ever keep too many roots, never drop a distinct one.
        let identity = physical_name(&key).unwrap_or_else(|| root.full_name().to_lowercase());
        if !seen.contains(&identity) {
            seen.push(identity);
            distinct.push(root);
        }
    }
    distinct
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;

    #[test]
    fn names_are_kernel_paths() {
        let scratch = Scratch::new("physical");
        let key = scratch.path.open().unwrap();
        let name = physical_name(&key).expect("name of an open key");
        assert!(name.starts_with("\\registry\\user\\"), "{name}");
        assert!(
            name.ends_with(&scratch.path.path.to_lowercase()),
            "{name} should end with {}",
            scratch.path.path
        );
    }

    #[test]
    fn same_key_under_two_spellings_is_kept_once() {
        let scratch = Scratch::new("physical-distinct");
        let a = scratch.key("A");
        let b = scratch.key("B");
        let a_shouting = RegPath::hkcu(a.path.to_uppercase());
        let roots = distinct_roots([
            a.clone(),
            a_shouting,
            b.clone(),
            scratch.path.child("Missing"),
            a.clone(),
        ]);
        assert_eq!(roots, vec![a, b]);
    }

    #[test]
    fn current_user_classes_alias_is_detected() {
        // HKCU\Software\Classes is an alias of HKU\<sid>_Classes: the
        // physical name does not contain "\software\classes".
        let Ok(key) = RegPath::hkcu(r"Software\Classes").open() else {
            return;
        };
        let name = physical_name(&key).unwrap();
        assert!(name.ends_with("_classes"), "{name}");
    }
}
