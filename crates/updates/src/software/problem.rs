//! Why the Windows Package Manager cannot be used on a particular PC, and
//! what the person in front of it can do about that.
//!
//! PCs differ: the package manager may be absent (long-term servicing
//! editions, server editions, stripped-down installations), years old, never
//! started before, switched off by an organisation, or simply offline. Each
//! of those is a different situation with a different remedy, and none of
//! them may ever look like "everything is up to date".

use super::codes;
use serde::{Deserialize, Serialize};

/// What stands between this PC and a list of software updates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Problem {
    /// The package manager is not installed.
    NotInstalled,
    /// It is installed but cannot be started (damaged or not registered for
    /// this user account).
    NotWorking {
        /// What Windows reported.
        detail: String,
    },
    /// It is too old to answer the questions Zest asks.
    TooOld {
        /// Its version, when it told.
        version: Option<String>,
    },
    /// The organisation that manages this PC has switched it off.
    BlockedByPolicy,
    /// Its package sources cannot be opened: never set up, damaged, or the
    /// PC is offline and no earlier copy exists.
    SourcesUnavailable {
        /// The last lines it printed (in the system language).
        detail: String,
    },
    /// The terms of a package source have not been accepted.
    AgreementsNotAccepted,
    /// It did not answer in time.
    TimedOut,
    /// It failed for a reason that is not one of the above.
    Failed {
        /// Its exit code, when it exited.
        code: Option<i32>,
        /// The last lines it printed (in the system language).
        detail: String,
    },
}

/// What can be done about a [`Problem`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Remedy {
    /// Install "App Installer" from the Microsoft Store.
    InstallPackageManager,
    /// Update "App Installer" from the Microsoft Store.
    UpdatePackageManager,
    /// Try again (after going online, or in a moment).
    Retry,
    /// Reset the package sources, then try again.
    ResetSources,
    /// Only the organisation's administrator can change this.
    AskAdministrator,
}

impl Remedy {
    /// Label for a button that carries out the remedy, when Zest can help
    /// with it.
    pub fn action_label(self) -> Option<&'static str> {
        match self {
            Remedy::InstallPackageManager | Remedy::UpdatePackageManager => {
                Some("Open the Store page")
            }
            Remedy::Retry => Some("Try again"),
            Remedy::ResetSources | Remedy::AskAdministrator => None,
        }
    }
}

impl Problem {
    /// One short line naming the situation.
    pub fn title(&self) -> &'static str {
        match self {
            Problem::NotInstalled => "Windows Package Manager is not installed",
            Problem::NotWorking { .. } => "Windows Package Manager does not start",
            Problem::TooOld { .. } => "Windows Package Manager is out of date",
            Problem::BlockedByPolicy => "Software updates are switched off by your organisation",
            Problem::SourcesUnavailable { .. } => "The package sources cannot be opened",
            Problem::AgreementsNotAccepted => "The package source terms were not accepted",
            Problem::TimedOut => "Windows Package Manager did not answer",
            Problem::Failed { .. } => "Windows Package Manager reported an error",
        }
    }

    /// What it means and what to do, in plain language.
    pub fn explanation(&self) -> String {
        match self {
            Problem::NotInstalled => "Software updates are installed by the package manager that ships with Windows 10 and 11. This PC does not have it. Install \u{201c}App Installer\u{201d} from the Microsoft Store, then check again.".to_owned(),
            Problem::NotWorking { .. } => "The package manager is present but Windows could not start it. Reinstalling or updating \u{201c}App Installer\u{201d} from the Microsoft Store usually repairs it.".to_owned(),
            Problem::TooOld { version } => {
                let which = version
                    .as_deref()
                    .map(|v| format!(" (version {v})"))
                    .unwrap_or_default();
                format!("The package manager on this PC{which} is too old to list updates. Update \u{201c}App Installer\u{201d} from the Microsoft Store, then check again.")
            }
            Problem::BlockedByPolicy => "A policy set by the organisation that manages this PC prevents the package manager from running. Your administrator can tell you how software is kept up to date here.".to_owned(),
            Problem::SourcesUnavailable { .. } => "The package manager could not open its list of available software. Check the internet connection and try again. If it keeps failing, run \u{201c}winget source reset --force\u{201d} in a terminal started as administrator.".to_owned(),
            Problem::AgreementsNotAccepted => "The package manager asked for the terms of its software sources to be accepted and could not be answered automatically. Updating \u{201c}App Installer\u{201d} from the Microsoft Store resolves this.".to_owned(),
            Problem::TimedOut => "The package manager took too long to answer, usually because a package source is slow to respond. Try again in a moment.".to_owned(),
            Problem::Failed { code, .. } => {
                let code = code
                    .map(|c| format!(" (code 0x{:08X})", c as u32))
                    .unwrap_or_default();
                format!("The package manager stopped with an error{code}. Try again; if it keeps happening, updating \u{201c}App Installer\u{201d} from the Microsoft Store often helps.")
            }
        }
    }

    /// Technical detail worth showing below the explanation, if any.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Problem::NotWorking { detail }
            | Problem::SourcesUnavailable { detail }
            | Problem::Failed { detail, .. } => {
                Some(detail.as_str()).filter(|d| !d.trim().is_empty())
            }
            _ => None,
        }
    }

    /// The most useful next step.
    pub fn remedy(&self) -> Remedy {
        match self {
            Problem::NotInstalled => Remedy::InstallPackageManager,
            Problem::NotWorking { .. }
            | Problem::TooOld { .. }
            | Problem::AgreementsNotAccepted => Remedy::UpdatePackageManager,
            Problem::BlockedByPolicy => Remedy::AskAdministrator,
            Problem::SourcesUnavailable { .. } => Remedy::ResetSources,
            Problem::TimedOut | Problem::Failed { .. } => Remedy::Retry,
        }
    }
}

/// What a listing attempt's exit code says about the package manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExitMeaning {
    /// The listing ran; an empty result really means "nothing to update".
    Fine,
    /// An option on the command line is not known to this version.
    UnknownOption,
    /// A specific problem.
    Policy,
    Sources,
    Agreements,
    /// Anything else that is not success.
    Other,
}

/// Reads the exit code of the listing command.
pub(crate) fn exit_meaning(code: Option<i32>) -> ExitMeaning {
    let Some(code) = code else {
        return ExitMeaning::Other;
    };
    match code {
        0 | codes::UPDATE_NOT_APPLICABLE | codes::NO_APPLICATIONS_FOUND => ExitMeaning::Fine,
        codes::INVALID_CL_ARGUMENTS | codes::UNSUPPORTED_ARGUMENT => ExitMeaning::UnknownOption,
        codes::BLOCKED_BY_POLICY => ExitMeaning::Policy,
        codes::SOURCE_AGREEMENTS_NOT_ACCEPTED => ExitMeaning::Agreements,
        codes::INDEX_INTEGRITY_COMPROMISED
        | codes::SOURCES_INVALID
        | codes::SOURCE_DATA_MISSING
        | codes::NO_SOURCES_DEFINED
        | codes::SOURCE_DATA_INTEGRITY_FAILURE
        | codes::SOURCE_OPEN_FAILED
        | codes::FAILED_TO_OPEN_ALL_SOURCES => ExitMeaning::Sources,
        _ => ExitMeaning::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_map_to_situations() {
        assert_eq!(exit_meaning(Some(0)), ExitMeaning::Fine);
        assert_eq!(
            exit_meaning(Some(codes::NO_APPLICATIONS_FOUND)),
            ExitMeaning::Fine
        );
        assert_eq!(
            exit_meaning(Some(codes::UPDATE_NOT_APPLICABLE)),
            ExitMeaning::Fine
        );
        // Documented decimal values, as they appear in logs.
        assert_eq!(exit_meaning(Some(-1978335230)), ExitMeaning::UnknownOption);
        assert_eq!(exit_meaning(Some(-1978335143)), ExitMeaning::UnknownOption);
        assert_eq!(exit_meaning(Some(-1978335174)), ExitMeaning::Policy);
        assert_eq!(exit_meaning(Some(-1978335162)), ExitMeaning::Agreements);
        for sources in [
            -1978335222,
            -1978335221,
            -1978335217,
            -1978335211,
            -1978335169,
            -1978335163,
            -1978335157,
        ] {
            assert_eq!(
                exit_meaning(Some(sources)),
                ExitMeaning::Sources,
                "{sources}"
            );
        }
        assert_eq!(exit_meaning(Some(1)), ExitMeaning::Other);
        assert_eq!(exit_meaning(Some(-1978335231)), ExitMeaning::Other);
        assert_eq!(exit_meaning(None), ExitMeaning::Other);
    }

    #[test]
    fn every_problem_explains_itself_and_names_a_next_step() {
        let problems = [
            Problem::NotInstalled,
            Problem::NotWorking {
                detail: "The system cannot execute the specified program.".into(),
            },
            Problem::TooOld {
                version: Some("1.0.11692".into()),
            },
            Problem::TooOld { version: None },
            Problem::BlockedByPolicy,
            Problem::SourcesUnavailable {
                detail: String::new(),
            },
            Problem::AgreementsNotAccepted,
            Problem::TimedOut,
            Problem::Failed {
                code: Some(-1978335231),
                detail: "Internal Error".into(),
            },
        ];
        for problem in &problems {
            assert!(!problem.title().is_empty());
            assert!(problem.explanation().len() > 40, "{problem:?}");
            // None of them may read like good news.
            let text = format!("{} {}", problem.title(), problem.explanation()).to_lowercase();
            assert!(
                !text.contains("up to date") || text.contains("kept up to date"),
                "{text}"
            );
        }
        assert_eq!(
            Problem::NotInstalled.remedy(),
            Remedy::InstallPackageManager
        );
        assert_eq!(problems[2].remedy(), Remedy::UpdatePackageManager);
        assert!(problems[2].explanation().contains("1.0.11692"));
        assert_eq!(Problem::BlockedByPolicy.remedy(), Remedy::AskAdministrator);
        assert_eq!(problems[5].remedy(), Remedy::ResetSources);
        assert_eq!(problems[5].detail(), None);
        assert_eq!(Problem::TimedOut.remedy(), Remedy::Retry);
        assert!(problems[8].explanation().contains("0x8A150001"));
        assert_eq!(problems[8].detail(), Some("Internal Error"));
        assert_eq!(
            Remedy::InstallPackageManager.action_label(),
            Some("Open the Store page")
        );
        assert_eq!(Remedy::AskAdministrator.action_label(), None);
    }
}
