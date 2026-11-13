//! Helpers shared by the unit tests of this crate.

use zest_core::reg::{RegPath, RegRoot};

/// A scratch key under `HKCU\Software\ZestTests` that is removed again
/// when dropped. Every mutating registry test works inside one of these and
/// never touches a real location.
pub(crate) struct Scratch(pub RegPath);

impl Scratch {
    /// Creates `HKCU\Software\ZestTests\system-<tag>-<pid>`.
    pub(crate) fn new(tag: &str) -> Self {
        let path = RegPath::hkcu(format!(
            "Software\\ZestTests\\system-{tag}-{}",
            std::process::id()
        ));
        RegRoot::Hkcu.key().create_subkey(&path.path).unwrap();
        Scratch(path)
    }

    /// Creates a sub-key and returns its path.
    pub(crate) fn child(&self, name: &str) -> RegPath {
        let child = self.0.child(name);
        RegRoot::Hkcu.key().create_subkey(&child.path).unwrap();
        child
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = RegRoot::Hkcu.key().delete_subkey_all(&self.0.path);
        // Remove the shared parent when no other test is using it.
        let _ = RegRoot::Hkcu.key().delete_subkey("Software\\ZestTests");
    }
}
