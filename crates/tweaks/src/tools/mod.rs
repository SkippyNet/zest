//! The maintenance and repair tools that ship with Windows.
//!
//! Nothing here is a repair of Zest's own making. Each [`Tool`] is a program
//! in `System32`, started with the arguments Microsoft documents. Tools that
//! print a report run hidden and have their output captured ([`run`]); the
//! classic consoles are simply opened for the user ([`open`]).

mod decode;
mod run;

pub use decode::{StreamDecoder, Transcript};
pub use run::{ToolRun, Verdict, open, run};

use serde::Serialize;
use std::path::PathBuf;
use zest_core::{paths, process};

/// Section a tool is listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Group {
    /// Checking and repairing Windows itself.
    Repair,
    /// Reclaiming space and resetting caches.
    Cleanup,
    /// Fixing connections.
    Network,
    /// Windows' own consoles, opened as they are.
    Consoles,
}

impl Group {
    /// All groups in display order.
    pub const ALL: [Group; 4] = [
        Group::Repair,
        Group::Cleanup,
        Group::Network,
        Group::Consoles,
    ];

    /// Heading shown above the group.
    pub fn label(self) -> &'static str {
        match self {
            Group::Repair => "Check and repair Windows",
            Group::Cleanup => "Clean up and reset",
            Group::Network => "Network",
            Group::Consoles => "Windows consoles",
        }
    }
}

/// How the exit code of a tool is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ExitCodes {
    /// Zero is success, anything else a failure.
    Plain,
    /// `dism.exe`: Windows error codes, with 3010 meaning "restart to finish".
    Dism,
    /// `chkdsk.exe`: the four documented codes.
    Chkdsk,
    /// `sfc.exe`: zero is a clean run; the findings are in the output.
    Sfc,
}

/// What starting a tool does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Action {
    /// Runs hidden until it exits; the output is captured.
    Command {
        /// File name in `System32`.
        program: &'static str,
        /// Arguments. `{drive}` stands for the system drive, e.g. `C:`.
        args: &'static [&'static str],
        codes: ExitCodes,
    },
    /// Opens a window for the user and returns at once.
    Open {
        /// File name in `System32`.
        program: &'static str,
        args: &'static [&'static str],
    },
}

/// One built-in Windows tool.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Tool {
    /// Stable identifier.
    pub id: &'static str,
    pub title: &'static str,
    /// What the tool does, in one or two plain sentences.
    pub description: &'static str,
    /// What it changes on the PC, stated outright.
    pub changes: &'static str,
    /// How long it tends to take.
    pub duration: &'static str,
    pub group: Group,
    /// Must run with administrator rights.
    pub needs_admin: bool,
    /// The PC has to be restarted for the result to take effect.
    pub needs_restart: bool,
    /// Alters Windows in a way worth confirming first.
    pub changes_system: bool,
    pub action: Action,
}

impl Tool {
    /// Looks a tool up by id.
    pub fn find(id: &str) -> Option<&'static Tool> {
        TOOLS.iter().find(|t| t.id.eq_ignore_ascii_case(id))
    }

    /// Full path of the program in `System32`.
    pub fn program(&self) -> PathBuf {
        match self.action {
            Action::Command { program, .. } | Action::Open { program, .. } => {
                process::system_tool(program)
            }
        }
    }

    /// The arguments with placeholders filled in.
    pub fn arguments(&self) -> Vec<String> {
        let args = match self.action {
            Action::Command { args, .. } | Action::Open { args, .. } => args,
        };
        let drive = system_drive_letter();
        args.iter().map(|a| a.replace("{drive}", &drive)).collect()
    }

    /// The command as it would be typed, for display.
    pub fn command_line(&self) -> String {
        let name = match self.action {
            Action::Command { program, .. } | Action::Open { program, .. } => program,
        };
        let mut line = name.to_owned();
        for arg in self.arguments() {
            line.push(' ');
            line.push_str(&arg);
        }
        line
    }

    /// True when the program exists on this version of Windows.
    pub fn is_available(&self) -> bool {
        self.program().is_file()
    }

    /// True for tools whose output is captured, false for consoles.
    pub fn is_command(&self) -> bool {
        matches!(self.action, Action::Command { .. })
    }
}

/// The system drive without a trailing backslash, e.g. `C:`.
fn system_drive_letter() -> String {
    let drive = paths::system_drive();
    drive
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_owned()
}

const DISM: &str = "Dism.exe";

/// Every tool, in the order it is shown.
pub static TOOLS: &[Tool] = &[
    // ── Check and repair Windows ─────────────────────────────────────
    Tool {
        id: "sfc-verify",
        title: "Check system files",
        description: "Checks that the protected files of Windows are intact and reports any that are damaged.",
        changes: "Changes nothing. It only reports.",
        duration: "5 to 15 minutes",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: "sfc.exe",
            args: &["/verifyonly"],
            codes: ExitCodes::Sfc,
        },
    },
    Tool {
        id: "sfc-repair",
        title: "Repair system files",
        description: "Checks the protected files of Windows and replaces damaged ones with good copies from the component store.",
        changes: "Replaces damaged Windows system files.",
        duration: "5 to 20 minutes",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: true,
        action: Action::Command {
            program: "sfc.exe",
            args: &["/scannow"],
            codes: ExitCodes::Sfc,
        },
    },
    Tool {
        id: "image-check",
        title: "Check the component store",
        description: "Reads whether Windows has already marked its component store as damaged. The component store is where system file repairs come from.",
        changes: "Changes nothing. It only reports.",
        duration: "A few seconds",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: DISM,
            args: &["/Online", "/Cleanup-Image", "/CheckHealth"],
            codes: ExitCodes::Dism,
        },
    },
    Tool {
        id: "image-scan",
        title: "Scan the component store",
        description: "Scans the whole component store for damage and records what it finds.",
        changes: "Changes nothing apart from recording the result.",
        duration: "5 to 10 minutes",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: DISM,
            args: &["/Online", "/Cleanup-Image", "/ScanHealth"],
            codes: ExitCodes::Dism,
        },
    },
    Tool {
        id: "image-repair",
        title: "Repair the component store",
        description: "Repairs the component store, downloading good files from Windows Update when needed. Run this when repairing system files fails.",
        changes: "Replaces damaged files in the Windows component store.",
        duration: "10 to 30 minutes",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: true,
        action: Action::Command {
            program: DISM,
            args: &["/Online", "/Cleanup-Image", "/RestoreHealth"],
            codes: ExitCodes::Dism,
        },
    },
    Tool {
        id: "disk-scan",
        title: "Scan the system drive for errors",
        description: "Checks the file system of the Windows drive while it stays in use.",
        changes: "Repairs nothing. Problems it finds are reported.",
        duration: "1 to 10 minutes",
        group: Group::Repair,
        needs_admin: true,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: "chkdsk.exe",
            args: &["{drive}", "/scan"],
            codes: ExitCodes::Chkdsk,
        },
    },
    // ── Clean up and reset ───────────────────────────────────────────
    Tool {
        id: "component-store-size",
        title: "Measure the component store",
        description: "Reports how much space older versions of Windows components take and whether a clean-up is worth it.",
        changes: "Changes nothing. It only reports.",
        duration: "1 to 3 minutes",
        group: Group::Cleanup,
        needs_admin: true,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: DISM,
            args: &["/Online", "/Cleanup-Image", "/AnalyzeComponentStore"],
            codes: ExitCodes::Dism,
        },
    },
    Tool {
        id: "component-store-cleanup",
        title: "Clean up the component store",
        description: "Removes older versions of Windows components that updates have replaced. Windows does this by itself over time; this does it now.",
        changes: "Deletes replaced component versions for good.",
        duration: "5 to 20 minutes",
        group: Group::Cleanup,
        needs_admin: true,
        needs_restart: false,
        changes_system: true,
        action: Action::Command {
            program: DISM,
            args: &["/Online", "/Cleanup-Image", "/StartComponentCleanup"],
            codes: ExitCodes::Dism,
        },
    },
    Tool {
        id: "store-cache",
        title: "Reset the Microsoft Store cache",
        description: "Clears the cache of the Microsoft Store, which fixes downloads and updates that are stuck. The Store opens when it is done.",
        changes: "Empties the Store cache. Installed apps stay.",
        duration: "Under a minute",
        group: Group::Cleanup,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: "WSReset.exe",
            args: &[],
            codes: ExitCodes::Plain,
        },
    },
    // ── Network ──────────────────────────────────────────────────────
    Tool {
        id: "dns-flush",
        title: "Clear the DNS cache",
        description: "Forgets the website addresses Windows has remembered, so they are looked up again. Helps when a site has moved and no longer loads.",
        changes: "Empties the DNS cache. It fills again as you browse.",
        duration: "A few seconds",
        group: Group::Network,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Command {
            program: "ipconfig.exe",
            args: &["/flushdns"],
            codes: ExitCodes::Plain,
        },
    },
    Tool {
        id: "winsock-reset",
        title: "Reset Winsock",
        description: "Puts the list of network providers Windows uses for connections back to its original state. Fixes connections broken by leftovers of network software.",
        changes: "Removes add-ons that VPN or security software put into the network stack. Such software may need to be reinstalled.",
        duration: "A few seconds",
        group: Group::Network,
        needs_admin: true,
        needs_restart: true,
        changes_system: true,
        action: Action::Command {
            program: "netsh.exe",
            args: &["winsock", "reset"],
            codes: ExitCodes::Plain,
        },
    },
    Tool {
        id: "tcpip-reset",
        title: "Reset TCP/IP",
        description: "Rewrites the TCP/IP settings of Windows to what they were when it was installed.",
        changes: "Resets network settings. Manually entered IP addresses and DNS servers are lost.",
        duration: "A few seconds",
        group: Group::Network,
        needs_admin: true,
        needs_restart: true,
        changes_system: true,
        action: Action::Command {
            program: "netsh.exe",
            args: &["int", "ip", "reset"],
            codes: ExitCodes::Plain,
        },
    },
    // ── Windows consoles ─────────────────────────────────────────────
    Tool {
        id: "disk-cleanup",
        title: "Disk Cleanup",
        description: "The classic Windows clean-up dialog.",
        changes: "Opens a window. Nothing changes until you choose something there.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "cleanmgr.exe",
            args: &[],
        },
    },
    Tool {
        id: "reliability-monitor",
        title: "Reliability Monitor",
        description: "A day-by-day history of crashes, failed updates and installs.",
        changes: "Opens a window. It only shows information.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "perfmon.exe",
            args: &["/rel"],
        },
    },
    Tool {
        id: "resource-monitor",
        title: "Resource Monitor",
        description: "Live processor, memory, disk and network use per process.",
        changes: "Opens a window. It only shows information.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "resmon.exe",
            args: &[],
        },
    },
    Tool {
        id: "event-viewer",
        title: "Event Viewer",
        description: "The logs Windows and apps write about errors and warnings.",
        changes: "Opens a window. It only shows information.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "eventvwr.msc",
            args: &[],
        },
    },
    Tool {
        id: "services",
        title: "Services",
        description: "The background services of Windows and installed software.",
        changes: "Opens a window. Nothing changes until you change a service there.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "services.msc",
            args: &[],
        },
    },
    Tool {
        id: "system-configuration",
        title: "System Configuration",
        description: "Start-up modes and boot options for troubleshooting.",
        changes: "Opens a window. Nothing changes until you apply something there.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "msconfig.exe",
            args: &[],
        },
    },
    Tool {
        id: "memory-diagnostic",
        title: "Windows Memory Diagnostic",
        description: "Tests the memory of this PC for faults. Windows asks whether to restart now or check at the next start.",
        changes: "Opens a window. The test itself runs during a restart you agree to.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "MdSched.exe",
            args: &[],
        },
    },
    Tool {
        id: "optimize-drives",
        title: "Optimize Drives",
        description: "Defragments hard disks and trims solid-state drives, and shows when that last happened.",
        changes: "Opens a window. Nothing changes until you choose a drive there.",
        duration: "",
        group: Group::Consoles,
        needs_admin: false,
        needs_restart: false,
        changes_system: false,
        action: Action::Open {
            program: "dfrgui.exe",
            args: &[],
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_entries_complete() {
        let mut seen = HashSet::new();
        for tool in TOOLS {
            assert!(seen.insert(tool.id), "duplicate id {}", tool.id);
            assert!(!tool.title.is_empty(), "{}", tool.id);
            assert!(tool.description.ends_with('.'), "{}", tool.id);
            assert!(tool.changes.ends_with('.'), "{}", tool.id);
            assert_eq!(
                tool.duration.is_empty(),
                !tool.is_command(),
                "{}: commands say how long they take, consoles do not",
                tool.id
            );
        }
        assert!(Tool::find("SFC-Repair").is_some(), "ids ignore case");
        assert!(Tool::find("format-c").is_none());
    }

    #[test]
    fn tools_are_started_from_system32_and_the_core_ones_exist() {
        for tool in TOOLS {
            assert!(
                tool.program().starts_with(paths::system32_dir()),
                "{} must be started from System32",
                tool.id
            );
        }
        // Server editions leave out a few desktop programs (the Store reset,
        // for one), so only the tools every Windows has are required.
        for id in [
            "sfc-repair",
            "image-repair",
            "disk-scan",
            "dns-flush",
            "winsock-reset",
        ] {
            let tool = Tool::find(id).unwrap();
            assert!(
                tool.is_available(),
                "{id} not found at {}",
                tool.program().display()
            );
        }
    }

    #[test]
    fn commands_are_built_as_documented() {
        let line = |id: &str| Tool::find(id).unwrap().command_line();
        assert_eq!(line("sfc-repair"), "sfc.exe /scannow");
        assert_eq!(line("sfc-verify"), "sfc.exe /verifyonly");
        assert_eq!(
            line("image-check"),
            "Dism.exe /Online /Cleanup-Image /CheckHealth"
        );
        assert_eq!(
            line("image-scan"),
            "Dism.exe /Online /Cleanup-Image /ScanHealth"
        );
        assert_eq!(
            line("image-repair"),
            "Dism.exe /Online /Cleanup-Image /RestoreHealth"
        );
        assert_eq!(
            line("component-store-size"),
            "Dism.exe /Online /Cleanup-Image /AnalyzeComponentStore"
        );
        assert_eq!(
            line("component-store-cleanup"),
            "Dism.exe /Online /Cleanup-Image /StartComponentCleanup"
        );
        assert_eq!(line("dns-flush"), "ipconfig.exe /flushdns");
        assert_eq!(line("winsock-reset"), "netsh.exe winsock reset");
        assert_eq!(line("tcpip-reset"), "netsh.exe int ip reset");
        assert_eq!(line("reliability-monitor"), "perfmon.exe /rel");
    }

    #[test]
    fn the_disk_scan_targets_the_system_drive_and_only_scans() {
        let tool = Tool::find("disk-scan").unwrap();
        let args = tool.arguments();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].len(), 2, "a drive like C: — {:?}", args[0]);
        assert!(args[0].ends_with(':'));
        assert_eq!(args[1], "/scan");
        assert!(!tool.changes_system);
    }

    #[test]
    fn nothing_destructive_hides_in_the_arguments() {
        for tool in TOOLS {
            let line = tool.command_line().to_ascii_lowercase();
            for word in [
                "/resetbase",
                "/f",
                "/r ",
                "/x",
                "format",
                "delete",
                "/spotfix",
            ] {
                assert!(
                    !line.split(' ').any(|part| part == word.trim()),
                    "{} contains {word}",
                    tool.id
                );
            }
        }
    }

    #[test]
    fn flags_match_what_the_tools_do() {
        for tool in TOOLS {
            if tool.needs_restart {
                assert!(tool.changes_system, "{}", tool.id);
            }
            if !tool.is_command() {
                assert!(!tool.needs_admin, "{}: Windows asks by itself", tool.id);
                assert!(!tool.changes_system, "{}", tool.id);
                assert_eq!(tool.group, Group::Consoles, "{}", tool.id);
            }
        }
        for id in [
            "sfc-repair",
            "image-repair",
            "component-store-cleanup",
            "winsock-reset",
            "tcpip-reset",
        ] {
            let tool = Tool::find(id).unwrap();
            assert!(tool.changes_system && tool.needs_admin, "{id}");
        }
        for id in [
            "sfc-verify",
            "image-check",
            "image-scan",
            "disk-scan",
            "component-store-size",
        ] {
            assert!(!Tool::find(id).unwrap().changes_system, "{id}");
        }
    }

    #[test]
    fn every_group_has_tools() {
        for group in Group::ALL {
            assert!(TOOLS.iter().any(|t| t.group == group), "{group:?}");
        }
    }
}
