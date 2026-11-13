//! Private lookup of the install folders of registered security products.
//!
//! Windows keeps a registry of the antivirus, firewall and antispyware
//! products that have announced themselves to the Security Center. Anything
//! running from one of their install folders is treated as security software
//! and is never throttled or suspended. Asking Windows keeps this accurate
//! for whatever product the user has installed, without a hard-coded list.

use std::sync::OnceLock;

static DIRS: OnceLock<Vec<String>> = OnceLock::new();

/// Lower-cased install folders (each ending in a backslash).
///
/// With `wait == false` this returns an empty slice until a previous call
/// (or [`warm_up`]) has finished the lookup, so hot paths never block on it.
pub(crate) fn product_dirs(wait: bool) -> &'static [String] {
    if wait {
        DIRS.get_or_init(query)
    } else {
        DIRS.get().map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Starts the lookup in the background.
pub(crate) fn warm_up() {
    if DIRS.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("zest-security-lookup".to_owned())
            .spawn(|| {
                let _ = DIRS.get_or_init(query);
            });
    }
}

/// True when `exe_path_lower` lies inside a registered security product folder.
pub(crate) fn is_security_path(exe_path_lower: &str, dirs: &[String]) -> bool {
    dirs.iter()
        .any(|dir| exe_path_lower.starts_with(dir.as_str()))
}

fn query() -> Vec<String> {
    // COM is initialised on a private thread so the caller's apartment model
    // is never affected.
    std::thread::Builder::new()
        .name("zest-security-query".to_owned())
        .spawn(query_on_this_thread)
        .ok()
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

fn query_on_this_thread() -> Vec<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };
    use windows::Win32::System::SecurityCenter::{
        IWSCProductList, WSC_SECURITY_PROVIDER_ANTISPYWARE, WSC_SECURITY_PROVIDER_ANTIVIRUS,
        WSC_SECURITY_PROVIDER_FIREWALL, WSCProductList,
    };

    let mut dirs: Vec<String> = Vec::new();
    // SAFETY: standard COM usage on a thread this function owns. Every
    // interface pointer is released (dropped) before CoUninitialize runs.
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return dirs;
        }
        for provider in [
            WSC_SECURITY_PROVIDER_ANTIVIRUS,
            WSC_SECURITY_PROVIDER_FIREWALL,
            WSC_SECURITY_PROVIDER_ANTISPYWARE,
        ] {
            let list: windows::core::Result<IWSCProductList> =
                CoCreateInstance(&WSCProductList, None, CLSCTX_INPROC_SERVER);
            let Ok(list) = list else {
                continue;
            };
            if list.Initialize(provider).is_err() {
                continue;
            }
            let count = list.Count().unwrap_or(0).max(0) as u32;
            for index in 0..count {
                let Ok(product) = list.get_Item(index) else {
                    continue;
                };
                if let Ok(path) = product.RemediationPath()
                    && let Some(dir) = install_dir(&path.to_string())
                    && !dirs.contains(&dir)
                {
                    dirs.push(dir);
                }
            }
        }
        CoUninitialize();
    }
    dirs
}

/// Derives a product's install folder from the program path it registered.
fn install_dir(registered: &str) -> Option<String> {
    let (program, _) = zest_core::cmdline::split_program(registered);
    let path = std::path::PathBuf::from(program.trim().trim_matches('"'));
    if !path.is_absolute() {
        return None;
    }
    let dir = path.parent()?.to_string_lossy().to_lowercase();
    let dir = dir.trim_end_matches('\\');
    // Refuse folders that are too generic to stand for a single product.
    let depth = dir.split('\\').filter(|c| !c.is_empty()).count();
    let windows = zest_core::paths::windows_dir()
        .to_string_lossy()
        .to_lowercase();
    if depth < 3 || dir == windows || dir.starts_with(&format!("{windows}\\")) {
        return None;
    }
    Some(format!("{dir}\\"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_dir_rules() {
        assert_eq!(
            install_dir(r#""C:\Program Files\Vendor\Shield\ui.exe" /open"#).as_deref(),
            Some(r"c:\program files\vendor\shield\")
        );
        assert_eq!(
            install_dir(r"C:\Program Files\Vendor Shield\ui.exe").as_deref(),
            Some(r"c:\program files\vendor shield\")
        );
        // Too generic, relative, or a URI: never treated as a product folder.
        assert_eq!(install_dir(r"C:\Tools\ui.exe"), None);
        assert_eq!(install_dir("windowsdefender://"), None);
        assert_eq!(install_dir("ui.exe"), None);
        let system = zest_core::paths::system32_dir().join("x.exe");
        assert_eq!(install_dir(&system.display().to_string()), None);
    }

    #[test]
    fn path_matching() {
        let dirs = vec![r"c:\program files\vendor\shield\".to_owned()];
        assert!(is_security_path(
            r"c:\program files\vendor\shield\bin\agent.exe",
            &dirs
        ));
        assert!(!is_security_path(
            r"c:\program files\vendor\shield-other\x.exe",
            &dirs
        ));
        assert!(!is_security_path(r"c:\program files\vendor\x.exe", &dirs));
    }

    #[test]
    fn lookup_completes() {
        // Must not hang or panic regardless of what is installed.
        let dirs = product_dirs(true);
        assert!(
            dirs.iter()
                .all(|d| d.ends_with('\\') && d == &d.to_lowercase())
        );
        assert_eq!(product_dirs(false).len(), dirs.len());
    }
}
