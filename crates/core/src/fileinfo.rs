//! Version resource lookup (publisher, description, product version).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

/// Strings from an executable's version resource.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionInfo {
    /// `CompanyName`, e.g. the publisher.
    pub company: Option<String>,
    /// `FileDescription`, usually the friendly program name.
    pub description: Option<String>,
    /// `ProductName`.
    pub product: Option<String>,
    /// `ProductVersion` (falls back to `FileVersion`).
    pub version: Option<String>,
}

/// Reads the version resource of an executable or DLL.
/// Returns `None` when the file has no version resource or cannot be read.
pub fn version_info(path: &Path) -> Option<VersionInfo> {
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let file = PCWSTR(wide.as_ptr());

    // SAFETY: `file` is NUL terminated; `data` is sized by the API itself and
    // every pointer returned by VerQueryValueW points into `data`, which
    // outlives all uses below.
    unsafe {
        let size = GetFileVersionInfoSizeW(file, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(file, None, size, data.as_mut_ptr().cast()).ok()?;

        let query = |sub_block: &str| -> Option<(*const u16, usize)> {
            let block: Vec<u16> = sub_block.encode_utf16().chain(Some(0)).collect();
            let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut len = 0u32;
            let ok = VerQueryValueW(
                data.as_ptr().cast(),
                PCWSTR(block.as_ptr()),
                &mut ptr,
                &mut len,
            );
            if ok.as_bool() && !ptr.is_null() && len > 0 {
                Some((ptr as *const u16, len as usize))
            } else {
                None
            }
        };

        // Language/code page pairs present in the file, then common fallbacks.
        let mut translations: Vec<(u16, u16)> = Vec::new();
        if let Some((ptr, bytes)) = query("\\VarFileInfo\\Translation") {
            let pairs = std::slice::from_raw_parts(ptr, bytes / 2);
            translations.extend(pairs.chunks_exact(2).map(|p| (p[0], p[1])));
        }
        translations.extend([
            (0x0409, 0x04B0),
            (0x0409, 0x04E4),
            (0x0409, 0x0000),
            (0x0000, 0x04B0),
        ]);

        let string = |name: &str| -> Option<String> {
            translations.iter().find_map(|(lang, cp)| {
                let (ptr, chars) = query(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{name}"))?;
                let slice = std::slice::from_raw_parts(ptr, chars);
                let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
                let text = String::from_utf16_lossy(&slice[..end]).trim().to_owned();
                (!text.is_empty()).then_some(text)
            })
        };

        Some(VersionInfo {
            company: string("CompanyName"),
            description: string("FileDescription"),
            product: string("ProductName"),
            version: string("ProductVersion").or_else(|| string("FileVersion")),
        })
    }
}

/// Convenience: the publisher of an executable, if it declares one.
pub fn publisher(path: &Path) -> Option<String> {
    version_info(path)?.company
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_system_binary() {
        let info = version_info(&crate::paths::system32_dir().join("notepad.exe"))
            .or_else(|| version_info(&crate::paths::system32_dir().join("cmd.exe")))
            .expect("a system binary should have version info");
        assert!(
            info.company.as_deref().unwrap_or("").contains("Microsoft"),
            "{info:?}"
        );
        assert!(info.version.is_some());
    }

    #[test]
    fn missing_or_plain_files_yield_none() {
        assert_eq!(version_info(Path::new(r"C:\definitely\not\here.exe")), None);
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("plain.txt");
        std::fs::write(&plain, b"no resources here").unwrap();
        assert_eq!(version_info(&plain), None);
    }
}
