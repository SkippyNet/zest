//! Test helpers: scratch registry trees and paths that are known not to exist.
//!
//! Every test that touches the registry works inside its own key below
//! `HKCU\Software\ZestTests`, which is removed again when the helper is
//! dropped. Nothing outside that key is ever written.

use winreg::RegValue;
use winreg::enums::RegType;
use zest_core::paths;
use zest_core::reg::{RegPath, RegRoot};

/// A folder on the system drive that does not exist.
pub(crate) fn gone_dir() -> String {
    format!("{}Zest No Such Dir", paths::system_drive().display())
}

/// A file inside [`gone_dir`].
pub(crate) fn gone(name: &str) -> String {
    format!("{}\\{name}", gone_dir())
}

/// A file that certainly exists.
pub(crate) fn existing_file() -> String {
    paths::system32_dir()
        .join("shell32.dll")
        .display()
        .to_string()
}

/// A scratch key under `HKCU\Software\ZestTests`, deleted on drop.
pub(crate) struct Scratch {
    pub path: RegPath,
}

impl Scratch {
    pub(crate) fn new(tag: &str) -> Self {
        let path = RegPath::hkcu(format!(
            "Software\\ZestTests\\registry-{tag}-{}",
            std::process::id()
        ));
        // Start from a clean slate in case an earlier run was interrupted.
        let _ = RegRoot::Hkcu.key().delete_subkey_all(&path.path);
        RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
        Scratch { path }
    }

    /// Creates (if needed) and returns a key below the scratch root.
    pub(crate) fn key(&self, rel: &str) -> RegPath {
        let path = self.path.child(rel);
        RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
        path
    }

    /// Sets a `REG_SZ` value, creating the key if needed. `key` must lie
    /// inside the scratch tree.
    pub(crate) fn set(&self, key: &RegPath, name: &str, data: &str) {
        self.assert_inside(key);
        let (k, _) = RegRoot::Hkcu.key().create_subkey(&key.path).unwrap();
        k.set_value(name, &data).unwrap();
    }

    /// Sets a `REG_DWORD` value, creating the key if needed.
    pub(crate) fn set_dword(&self, key: &RegPath, name: &str, data: u32) {
        self.assert_inside(key);
        let (k, _) = RegRoot::Hkcu.key().create_subkey(&key.path).unwrap();
        k.set_value(name, &data).unwrap();
    }

    /// Sets a `REG_BINARY` value, creating the key if needed.
    pub(crate) fn set_binary(&self, key: &RegPath, name: &str, data: &[u8]) {
        self.assert_inside(key);
        let (k, _) = RegRoot::Hkcu.key().create_subkey(&key.path).unwrap();
        k.set_raw_value(
            name,
            &RegValue {
                bytes: data.to_vec().into(),
                vtype: RegType::REG_BINARY,
            },
        )
        .unwrap();
    }

    fn assert_inside(&self, key: &RegPath) {
        assert_eq!(key.root, RegRoot::Hkcu);
        assert!(
            key.path == self.path.path || key.path.starts_with(&format!("{}\\", self.path.path)),
            "{key} is outside the scratch tree"
        );
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = RegRoot::Hkcu.key().delete_subkey_all(&self.path.path);
        // Remove the shared parent when no other test is using it.
        let _ = RegRoot::Hkcu.key().delete_subkey("Software\\ZestTests");
    }
}
