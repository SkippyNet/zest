//! The pages of the application and what they share.

pub mod apps;
pub mod backup;
pub mod clean;
pub mod disk;
pub mod drivers;
pub mod duplicates;
pub mod empty;
pub mod extensions;
pub mod health;
pub mod options;
pub mod performance;
pub mod privacy;
pub mod registry;
pub mod restore;
pub mod startup;
pub mod sysinfo;
pub mod tools;
pub mod updates;
pub mod wiper;

use crate::theme::Palette;
use crate::widgets::Toasts;
use egui_phosphor::regular as icon;
use zest_engine::Settings;

/// Every destination in the navigation rail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Health,
    Clean,
    Registry,
    Privacy,
    Startup,
    Performance,
    Apps,
    Updates,
    Drivers,
    Extensions,
    Disk,
    Duplicates,
    Empty,
    Wiper,
    Backup,
    Restore,
    SysInfo,
    Tools,
    Options,
}

impl Page {
    /// All pages in navigation order.
    pub const ALL: [Page; 19] = [
        Page::Health,
        Page::Clean,
        Page::Registry,
        Page::Privacy,
        Page::Startup,
        Page::Performance,
        Page::Apps,
        Page::Updates,
        Page::Drivers,
        Page::Extensions,
        Page::Disk,
        Page::Duplicates,
        Page::Empty,
        Page::Wiper,
        Page::Backup,
        Page::Restore,
        Page::SysInfo,
        Page::Tools,
        Page::Options,
    ];

    /// Label in the navigation rail.
    pub fn label(self) -> &'static str {
        match self {
            Page::Health => "Health check",
            Page::Clean => "Custom clean",
            Page::Registry => "Registry",
            Page::Privacy => "Windows privacy",
            Page::Startup => "Startup",
            Page::Performance => "Performance",
            Page::Apps => "Uninstall",
            Page::Updates => "Software updates",
            Page::Drivers => "Drivers",
            Page::Extensions => "Browser extensions",
            Page::Disk => "Disk analyzer",
            Page::Duplicates => "Duplicate finder",
            Page::Empty => "Empty folders",
            Page::Wiper => "Drive wiper",
            Page::Backup => "Backup",
            Page::Restore => "System restore",
            Page::SysInfo => "System info",
            Page::Tools => "Windows tools",
            Page::Options => "Options",
        }
    }

    /// Icon in the navigation rail.
    pub fn icon(self) -> &'static str {
        match self {
            Page::Health => icon::HEARTBEAT,
            Page::Clean => icon::BROOM,
            Page::Registry => icon::TREE_STRUCTURE,
            Page::Privacy => icon::SHIELD_CHECK,
            Page::Startup => icon::ROCKET_LAUNCH,
            Page::Performance => icon::GAUGE,
            Page::Apps => icon::PACKAGE,
            Page::Updates => icon::ARROWS_CLOCKWISE,
            Page::Drivers => icon::CPU,
            Page::Extensions => icon::PUZZLE_PIECE,
            Page::Disk => icon::CHART_PIE_SLICE,
            Page::Duplicates => icon::COPY,
            Page::Empty => icon::FOLDER_DASHED,
            Page::Wiper => icon::ERASER,
            Page::Backup => icon::VAULT,
            Page::Restore => icon::CLOCK_COUNTER_CLOCKWISE,
            Page::SysInfo => icon::DESKTOP_TOWER,
            Page::Tools => icon::WRENCH,
            Page::Options => icon::GEAR_SIX,
        }
    }

    /// Stable name used on the command line (`--page clean`).
    pub fn slug(self) -> &'static str {
        match self {
            Page::Health => "health",
            Page::Clean => "clean",
            Page::Registry => "registry",
            Page::Privacy => "privacy",
            Page::Startup => "startup",
            Page::Performance => "performance",
            Page::Apps => "uninstall",
            Page::Updates => "updates",
            Page::Drivers => "drivers",
            Page::Extensions => "extensions",
            Page::Disk => "disk",
            Page::Duplicates => "duplicates",
            Page::Empty => "empty",
            Page::Wiper => "wiper",
            Page::Backup => "backup",
            Page::Restore => "restore",
            Page::SysInfo => "system",
            Page::Tools => "tools",
            Page::Options => "options",
        }
    }

    /// Parses a [`Page::slug`].
    pub fn from_slug(slug: &str) -> Option<Page> {
        Page::ALL
            .into_iter()
            .find(|p| p.slug().eq_ignore_ascii_case(slug))
    }
}

/// Navigation groups: `(heading, pages)`.
pub const NAV_GROUPS: [(&str, &[Page]); 6] = [
    (
        "Clean",
        &[Page::Health, Page::Clean, Page::Registry, Page::Privacy],
    ),
    ("Speed", &[Page::Startup, Page::Performance]),
    (
        "Software",
        &[Page::Apps, Page::Updates, Page::Drivers, Page::Extensions],
    ),
    (
        "Storage",
        &[Page::Disk, Page::Duplicates, Page::Empty, Page::Wiper],
    ),
    ("Protect", &[Page::Backup, Page::Restore]),
    ("System", &[Page::SysInfo, Page::Tools]),
];

/// What a page gets from, and reports back to, the application shell.
pub struct Cx<'a> {
    /// Current colours.
    pub pal: &'a Palette,
    /// User settings. Set [`Cx::settings_changed`] after modifying them.
    pub settings: &'a mut Settings,
    /// Corner notifications.
    pub toasts: &'a mut Toasts,
    /// Zest's own update check (shown on the About tab of the options).
    pub updater: &'a mut crate::updater::Updater,
    /// Whether the app runs with administrator rights.
    pub elevated: bool,
    /// Set to navigate to another page.
    pub goto: Option<Page>,
    /// Set after changing `settings` so they are saved and re-applied.
    pub settings_changed: bool,
    /// Set to restart the app with administrator rights.
    pub request_elevation: bool,
}
