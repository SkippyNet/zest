//! Meaning of the package manager's process exit codes.
//!
//! The codes are HRESULT-style values documented by the Windows Package
//! Manager project. Only the ones that change what the user should do next
//! get their own [`UpgradeStatus`]; everything else is a plain failure with
//! the code preserved for troubleshooting.

use super::UpgradeStatus;

/// Builds the signed exit code for a package manager error number.
const fn code(low: u32) -> i32 {
    (0x8A15_0000u32 | low) as i32
}

/// No package matched the query (also "nothing to upgrade" for listings).
pub(crate) const NO_APPLICATIONS_FOUND: i32 = code(0x0014);
/// The installed version is already the newest applicable one.
pub(crate) const UPDATE_NOT_APPLICABLE: i32 = code(0x002B);
/// The process was started through a console that was closed, or signalled.
const CTRL_SIGNAL_RECEIVED: i32 = code(0x0005);
const SHELLEXEC_INSTALL_FAILED: i32 = code(0x0006);
const DOWNLOAD_FAILED: i32 = code(0x0008);
const NO_APPLICABLE_INSTALLER: i32 = code(0x0010);
const INSTALLER_HASH_MISMATCH: i32 = code(0x0011);
const MULTIPLE_APPLICATIONS_FOUND: i32 = code(0x0016);
const COMMAND_REQUIRES_ADMIN: i32 = code(0x0019);
const MSSTORE_BLOCKED_BY_POLICY: i32 = code(0x001B);
const MSSTORE_APP_BLOCKED_BY_POLICY: i32 = code(0x001C);
const MSSTORE_INSTALL_FAILED: i32 = code(0x001E);
const UPDATE_ALL_HAS_FAILURE: i32 = code(0x002C);
const INSTALLER_SECURITY_CHECK_FAILED: i32 = code(0x002D);
const DOWNLOAD_SIZE_MISMATCH: i32 = code(0x002E);
const BLOCKED_BY_POLICY: i32 = code(0x003A);
const PACKAGE_AGREEMENTS_NOT_ACCEPTED: i32 = code(0x0041);
const SOURCE_AGREEMENTS_NOT_ACCEPTED: i32 = code(0x0046);
const MSI_INSTALL_FAILED: i32 = code(0x0049);
const UPGRADE_VERSION_NOT_NEWER: i32 = code(0x004F);
const UPGRADE_VERSION_UNKNOWN: i32 = code(0x0050);
const PORTABLE_INSTALL_FAILED: i32 = code(0x0052);
const INSTALLER_PROHIBITS_ELEVATION: i32 = code(0x0056);
const PACKAGE_ALREADY_INSTALLED: i32 = code(0x0061);
const PACKAGE_IS_PINNED: i32 = code(0x0068);
const SERVICE_UNAVAILABLE: i32 = code(0x006D);
const INSTALL_PACKAGE_IN_USE: i32 = code(0x0101);
const INSTALL_INSTALL_IN_PROGRESS: i32 = code(0x0102);
const INSTALL_FILE_IN_USE: i32 = code(0x0103);
const INSTALL_MISSING_DEPENDENCY: i32 = code(0x0104);
const INSTALL_DISK_FULL: i32 = code(0x0105);
const INSTALL_INSUFFICIENT_MEMORY: i32 = code(0x0106);
const INSTALL_NO_NETWORK: i32 = code(0x0107);
const INSTALL_CONTACT_SUPPORT: i32 = code(0x0108);
const INSTALL_REBOOT_REQUIRED_TO_FINISH: i32 = code(0x0109);
const INSTALL_REBOOT_REQUIRED_FOR_INSTALL: i32 = code(0x010A);
const INSTALL_REBOOT_INITIATED: i32 = code(0x010B);
const INSTALL_CANCELLED_BY_USER: i32 = code(0x010C);
const INSTALL_ALREADY_INSTALLED: i32 = code(0x010D);
const INSTALL_DOWNGRADE: i32 = code(0x010E);
const INSTALL_BLOCKED_BY_POLICY: i32 = code(0x010F);
const INSTALL_DEPENDENCIES: i32 = code(0x0110);
const INSTALL_PACKAGE_IN_USE_BY_APPLICATION: i32 = code(0x0111);
const INSTALL_INVALID_PARAMETER: i32 = code(0x0112);
const INSTALL_SYSTEM_NOT_SUPPORTED: i32 = code(0x0113);
const INSTALL_UPGRADE_NOT_SUPPORTED: i32 = code(0x0114);

/// `HRESULT_FROM_WIN32(ERROR_CANCELLED)`: the elevation prompt was declined.
const E_CANCELLED: i32 = 0x8007_04C7u32 as i32;
/// `E_ACCESSDENIED`.
const E_ACCESS_DENIED: i32 = 0x8007_0005u32 as i32;
/// `HRESULT_FROM_WIN32(ERROR_ELEVATION_REQUIRED)`.
const E_ELEVATION_REQUIRED: i32 = 0x8007_02E4u32 as i32;
/// `ERROR_SUCCESS_REBOOT_REQUIRED` as returned by Windows Installer.
const MSI_REBOOT_REQUIRED: i32 = 3010;
/// `ERROR_SUCCESS_REBOOT_INITIATED`.
const MSI_REBOOT_INITIATED: i32 = 1641;

/// Maps a process exit code to a status and a plain-language explanation.
pub(crate) fn classify(exit_code: Option<i32>) -> (UpgradeStatus, &'static str) {
    use UpgradeStatus as S;
    let Some(code) = exit_code else {
        return (S::Failed, "The package manager stopped unexpectedly.");
    };
    match code {
        0 => (S::Upgraded, "Updated successfully."),
        MSI_REBOOT_REQUIRED
        | MSI_REBOOT_INITIATED
        | INSTALL_REBOOT_REQUIRED_TO_FINISH
        | INSTALL_REBOOT_INITIATED => (
            S::RebootRequired,
            "Updated. Restart your PC to finish the installation.",
        ),
        INSTALL_REBOOT_REQUIRED_FOR_INSTALL => (
            S::Failed,
            "The update could not start. Restart your PC, then try again.",
        ),
        UPDATE_NOT_APPLICABLE
        | UPGRADE_VERSION_NOT_NEWER
        | PACKAGE_ALREADY_INSTALLED
        | INSTALL_ALREADY_INSTALLED => (S::NoUpdateAvailable, "Already up to date."),
        NO_APPLICABLE_INSTALLER | INSTALL_SYSTEM_NOT_SUPPORTED | INSTALL_UPGRADE_NOT_SUPPORTED => (
            S::NoUpdateAvailable,
            "No update suitable for this PC is available.",
        ),
        UPGRADE_VERSION_UNKNOWN => (
            S::NoUpdateAvailable,
            "The installed version could not be determined, so the update was skipped.",
        ),
        INSTALL_DOWNGRADE => (
            S::NoUpdateAvailable,
            "A newer version than the one offered is already installed.",
        ),
        INSTALL_PACKAGE_IN_USE | INSTALL_FILE_IN_USE | INSTALL_PACKAGE_IN_USE_BY_APPLICATION => (
            S::InUse,
            "The program is running. Close it, then try again.",
        ),
        INSTALL_INSTALL_IN_PROGRESS => (
            S::InstallerBusy,
            "Another installation is in progress. Try again when it has finished.",
        ),
        INSTALL_CANCELLED_BY_USER | E_CANCELLED | CTRL_SIGNAL_RECEIVED => {
            (S::Cancelled, "The update was cancelled.")
        }
        COMMAND_REQUIRES_ADMIN | E_ACCESS_DENIED | E_ELEVATION_REQUIRED => (
            S::NeedsAdmin,
            "Administrator rights are required to update this program.",
        ),
        INSTALLER_PROHIBITS_ELEVATION => (
            S::Failed,
            "This program must be updated without administrator rights. Start Zest normally and try again.",
        ),
        BLOCKED_BY_POLICY
        | INSTALL_BLOCKED_BY_POLICY
        | MSSTORE_BLOCKED_BY_POLICY
        | MSSTORE_APP_BLOCKED_BY_POLICY => (
            S::BlockedByPolicy,
            "Your organisation's policy blocks this update.",
        ),
        PACKAGE_IS_PINNED => (S::Pinned, "This program is pinned to its current version."),
        NO_APPLICATIONS_FOUND => (
            S::NotFound,
            "The package manager no longer recognises this program.",
        ),
        MULTIPLE_APPLICATIONS_FOUND => (
            S::NotFound,
            "More than one program matches; it must be updated manually.",
        ),
        DOWNLOAD_FAILED | INSTALL_NO_NETWORK | SERVICE_UNAVAILABLE => (
            S::DownloadFailed,
            "The update could not be downloaded. Check your internet connection.",
        ),
        INSTALLER_HASH_MISMATCH | DOWNLOAD_SIZE_MISMATCH | INSTALLER_SECURITY_CHECK_FAILED => (
            S::VerificationFailed,
            "The downloaded installer failed its integrity check and was not run.",
        ),
        PACKAGE_AGREEMENTS_NOT_ACCEPTED | SOURCE_AGREEMENTS_NOT_ACCEPTED => (
            S::Failed,
            "The licence agreement for this update was not accepted.",
        ),
        INSTALL_DISK_FULL => (
            S::Failed,
            "There is not enough free disk space for this update.",
        ),
        INSTALL_INSUFFICIENT_MEMORY => (
            S::Failed,
            "There is not enough memory to install this update.",
        ),
        INSTALL_MISSING_DEPENDENCY | INSTALL_DEPENDENCIES => (
            S::Failed,
            "A component this update depends on could not be installed.",
        ),
        UPDATE_ALL_HAS_FAILURE => (S::Failed, "Some updates could not be installed."),
        SHELLEXEC_INSTALL_FAILED
        | MSI_INSTALL_FAILED
        | MSSTORE_INSTALL_FAILED
        | PORTABLE_INSTALL_FAILED
        | INSTALL_CONTACT_SUPPORT
        | INSTALL_INVALID_PARAMETER => (S::Failed, "The program's installer reported an error."),
        _ => (S::Failed, "The update could not be installed."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_decimal_values() {
        // Cross-check the hexadecimal constants against the decimal values
        // people see in logs.
        assert_eq!(UPDATE_NOT_APPLICABLE, -1978335189);
        assert_eq!(NO_APPLICATIONS_FOUND, -1978335212);
        assert_eq!(INSTALL_REBOOT_REQUIRED_TO_FINISH, -1978334967);
        assert_eq!(INSTALL_PACKAGE_IN_USE, -1978334975);
        assert_eq!(COMMAND_REQUIRES_ADMIN, -1978335207);
        assert_eq!(E_CANCELLED, -2147023673);
    }

    #[test]
    fn classification() {
        use UpgradeStatus as S;
        assert_eq!(classify(Some(0)).0, S::Upgraded);
        assert_eq!(classify(Some(3010)).0, S::RebootRequired);
        assert_eq!(
            classify(Some(INSTALL_REBOOT_REQUIRED_TO_FINISH)).0,
            S::RebootRequired
        );
        assert_eq!(
            classify(Some(UPDATE_NOT_APPLICABLE)).0,
            S::NoUpdateAvailable
        );
        assert_eq!(
            classify(Some(NO_APPLICABLE_INSTALLER)).0,
            S::NoUpdateAvailable
        );
        assert_eq!(classify(Some(INSTALL_PACKAGE_IN_USE)).0, S::InUse);
        assert_eq!(classify(Some(INSTALL_FILE_IN_USE)).0, S::InUse);
        assert_eq!(
            classify(Some(INSTALL_INSTALL_IN_PROGRESS)).0,
            S::InstallerBusy
        );
        assert_eq!(classify(Some(INSTALL_CANCELLED_BY_USER)).0, S::Cancelled);
        assert_eq!(classify(Some(E_CANCELLED)).0, S::Cancelled);
        assert_eq!(classify(Some(COMMAND_REQUIRES_ADMIN)).0, S::NeedsAdmin);
        assert_eq!(classify(Some(E_ACCESS_DENIED)).0, S::NeedsAdmin);
        assert_eq!(classify(Some(BLOCKED_BY_POLICY)).0, S::BlockedByPolicy);
        assert_eq!(classify(Some(PACKAGE_IS_PINNED)).0, S::Pinned);
        assert_eq!(classify(Some(NO_APPLICATIONS_FOUND)).0, S::NotFound);
        assert_eq!(classify(Some(DOWNLOAD_FAILED)).0, S::DownloadFailed);
        assert_eq!(
            classify(Some(INSTALLER_HASH_MISMATCH)).0,
            S::VerificationFailed
        );
        assert_eq!(classify(Some(SHELLEXEC_INSTALL_FAILED)).0, S::Failed);
        assert_eq!(classify(Some(1)).0, S::Failed);
        assert_eq!(classify(Some(-1)).0, S::Failed);
        assert_eq!(classify(None).0, S::Failed);
        // Every status comes with a non-empty explanation.
        for code in [
            0,
            1,
            3010,
            UPDATE_NOT_APPLICABLE,
            INSTALL_DISK_FULL,
            E_CANCELLED,
        ] {
            assert!(!classify(Some(code)).1.is_empty());
        }
    }
}
