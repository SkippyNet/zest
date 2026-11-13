//! Starting the background agent when the user signs in.
//!
//! Uses the per-user `Run` key, the same place Task Manager's Startup tab
//! shows, so the user can always see and disable it there.

use std::path::Path;
use zest_core::reg::RegPath;
use zest_core::{Error, Result, brand, elevation};

/// Command line argument that starts the app as a background agent.
pub const AGENT_ARG: &str = "--agent";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn run_key() -> RegPath {
    RegPath::hkcu(RUN_KEY)
}

fn command_for(exe: &Path) -> String {
    format!(
        "{} {AGENT_ARG}",
        elevation::quote_arg(&exe.to_string_lossy())
    )
}

fn is_enabled_in(key: &RegPath) -> bool {
    key.read_string(brand::NAME).is_some()
}

fn set_enabled_in(key: &RegPath, exe: &Path, enabled: bool) -> Result<()> {
    let map = |e: std::io::Error| Error::Io {
        context: format!("updating {key}"),
        source: e,
    };
    if enabled {
        let (opened, _) = key.root.key().create_subkey(&key.path).map_err(map)?;
        opened
            .set_value(brand::NAME, &command_for(exe))
            .map_err(map)
    } else {
        match key.open_write() {
            Ok(opened) => match opened.delete_value(brand::NAME) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(map(e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(map(e)),
        }
    }
}

/// True when the agent is registered to start at sign-in.
pub fn is_enabled() -> bool {
    is_enabled_in(&run_key())
}

/// Registers or unregisters the agent for start at sign-in.
pub fn set_enabled(exe: &Path, enabled: bool) -> Result<()> {
    set_enabled_in(&run_key(), exe, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::reg::RegRoot;

    #[test]
    fn enable_disable_round_trip_on_scratch_key() {
        let key = RegPath::hkcu(format!(
            "Software\\ZestTests\\autostart-{}\\Run",
            std::process::id()
        ));
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = RegRoot::Hkcu.key().delete_subkey_all(&self.0);
                let _ = RegRoot::Hkcu.key().delete_subkey("Software\\ZestTests");
            }
        }
        let _cleanup = Cleanup(key.parent().unwrap().path);

        let exe = Path::new(r"C:\Program Files\Zest\zest.exe");
        assert!(!is_enabled_in(&key));
        // Disabling when nothing is registered (key missing) is fine.
        set_enabled_in(&key, exe, false).unwrap();

        set_enabled_in(&key, exe, true).unwrap();
        assert!(is_enabled_in(&key));
        assert_eq!(
            key.read_string(brand::NAME).as_deref(),
            Some(r#""C:\Program Files\Zest\zest.exe" --agent"#)
        );
        set_enabled_in(&key, exe, false).unwrap();
        assert!(!is_enabled_in(&key));
        set_enabled_in(&key, exe, false).unwrap();
    }
}
