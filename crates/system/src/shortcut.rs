//! Reading and writing Windows shortcuts (`.lnk` files).

use crate::com::ComGuard;
use crate::util::{from_wide, wide, wide_os, win_err};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{Interface, PCWSTR};
use zest_core::{Result, paths};

/// `SLGP_RAWPATH`: return the stored path without resolving it.
const SLGP_RAWPATH: u32 = 0x4;

/// The contents of a shortcut file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    /// File or folder the shortcut opens. `None` for shortcuts without a file
    /// target (installer-advertised shortcuts, shell namespace items).
    pub target: Option<PathBuf>,
    /// Command line arguments passed to the target.
    pub arguments: String,
    /// "Start in" folder.
    pub working_dir: Option<PathBuf>,
    /// Comment shown as the tooltip.
    pub description: String,
}

impl Shortcut {
    /// The command line the shortcut effectively runs (target plus arguments),
    /// or an empty string when it has no file target.
    pub fn command_line(&self) -> String {
        let Some(target) = &self.target else {
            return String::new();
        };
        let target = target.display().to_string();
        let quoted = if target.contains(' ') {
            format!("\"{target}\"")
        } else {
            target
        };
        if self.arguments.trim().is_empty() {
            quoted
        } else {
            format!("{quoted} {}", self.arguments.trim())
        }
    }
}

fn non_empty_path(text: String) -> Option<PathBuf> {
    let expanded = paths::expand_env(text.trim());
    (!expanded.is_empty()).then(|| PathBuf::from(expanded))
}

/// Reads a `.lnk` file. The target is not required to exist.
pub fn read_shortcut(path: &Path) -> Result<Shortcut> {
    let _com = ComGuard::new();
    let file_name = wide_os(path.as_os_str());
    let action = "read shortcut";
    // SAFETY: standard COM usage; every buffer passed is valid for its stated
    // length and `file_name` is NUL terminated and outlives the call.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| win_err(e, action))?;
        let persist: IPersistFile = link.cast().map_err(|e| win_err(e, action))?;
        persist
            .Load(PCWSTR(file_name.as_ptr()), STGM_READ)
            .map_err(|e| win_err(e, &format!("read shortcut {}", path.display())))?;

        let mut buf = vec![0u16; 4096];
        let mut target = None;
        if link
            .GetPath(&mut buf, std::ptr::null_mut(), SLGP_RAWPATH)
            .is_ok()
        {
            target = non_empty_path(from_wide(&buf));
        }
        if target.is_none() {
            buf.fill(0);
            if link.GetPath(&mut buf, std::ptr::null_mut(), 0).is_ok() {
                target = non_empty_path(from_wide(&buf));
            }
        }

        buf.fill(0);
        let arguments = if link.GetArguments(&mut buf).is_ok() {
            from_wide(&buf)
        } else {
            String::new()
        };

        buf.fill(0);
        let working_dir = if link.GetWorkingDirectory(&mut buf).is_ok() {
            non_empty_path(from_wide(&buf))
        } else {
            None
        };

        buf.fill(0);
        let description = if link.GetDescription(&mut buf[..1024]).is_ok() {
            from_wide(&buf)
        } else {
            String::new()
        };

        Ok(Shortcut {
            target,
            arguments,
            working_dir,
            description,
        })
    }
}

/// Creates (or overwrites) a `.lnk` file at `path`.
pub fn write_shortcut(path: &Path, shortcut: &Shortcut) -> Result<()> {
    let _com = ComGuard::new();
    let action = "create shortcut";
    let file_name = wide_os(path.as_os_str());
    // SAFETY: standard COM usage; all strings are NUL terminated and outlive
    // the calls they are passed to.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| win_err(e, action))?;
        if let Some(target) = &shortcut.target {
            let target = wide_os(target.as_os_str());
            link.SetPath(PCWSTR(target.as_ptr()))
                .map_err(|e| win_err(e, action))?;
        }
        if !shortcut.arguments.is_empty() {
            let args = wide(&shortcut.arguments);
            link.SetArguments(PCWSTR(args.as_ptr()))
                .map_err(|e| win_err(e, action))?;
        }
        if let Some(dir) = &shortcut.working_dir {
            let dir = wide_os(dir.as_os_str());
            link.SetWorkingDirectory(PCWSTR(dir.as_ptr()))
                .map_err(|e| win_err(e, action))?;
        }
        if !shortcut.description.is_empty() {
            let text = wide(&shortcut.description);
            link.SetDescription(PCWSTR(text.as_ptr()))
                .map_err(|e| win_err(e, action))?;
        }
        let persist: IPersistFile = link.cast().map_err(|e| win_err(e, action))?;
        persist
            .Save(PCWSTR(file_name.as_ptr()), true)
            .map_err(|e| win_err(e, &format!("create shortcut {}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let lnk = tmp.path().join("My Tool.lnk");
        let target = paths::system32_dir().join("cmd.exe");
        let written = Shortcut {
            target: Some(target.clone()),
            arguments: "/c echo \"hello world\"".to_owned(),
            working_dir: Some(tmp.path().to_path_buf()),
            description: "A test shortcut".to_owned(),
        };
        write_shortcut(&lnk, &written).unwrap();
        assert!(lnk.is_file());

        let read = read_shortcut(&lnk).unwrap();
        assert!(paths::same_path(read.target.as_deref().unwrap(), &target));
        assert_eq!(read.arguments, written.arguments);
        assert!(paths::same_path(
            read.working_dir.as_deref().unwrap(),
            tmp.path()
        ));
        assert_eq!(read.description, "A test shortcut");
        assert!(
            read.command_line()
                .ends_with("cmd.exe /c echo \"hello world\"")
        );
    }

    #[test]
    fn target_may_be_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let lnk = tmp.path().join("gone.lnk");
        let target = tmp.path().join("Removed App").join("app.exe");
        write_shortcut(
            &lnk,
            &Shortcut {
                target: Some(target.clone()),
                ..Shortcut::default()
            },
        )
        .unwrap();
        let read = read_shortcut(&lnk).unwrap();
        assert!(paths::same_path(read.target.as_deref().unwrap(), &target));
        assert_eq!(read.arguments, "");
        assert_eq!(
            read.command_line(),
            format!("\"{}\"", read.target.as_ref().unwrap().display()).replace("\"\"", "\"")
        );
    }

    #[test]
    fn unreadable_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let bogus = tmp.path().join("not-a-link.lnk");
        std::fs::write(&bogus, b"this is not a shortcut").unwrap();
        assert!(read_shortcut(&bogus).is_err());
        assert!(read_shortcut(&tmp.path().join("missing.lnk")).is_err());
    }

    #[test]
    fn command_line_quoting() {
        let s = Shortcut {
            target: Some(PathBuf::from(r"C:\Tools\run.exe")),
            arguments: " --fast ".to_owned(),
            ..Shortcut::default()
        };
        assert_eq!(s.command_line(), r"C:\Tools\run.exe --fast");
        assert_eq!(Shortcut::default().command_line(), "");
    }
}
