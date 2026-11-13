//! Scheduled cleaning through the Windows Task Scheduler.
//!
//! Zest does not need to keep running to clean on a schedule: it registers
//! one task (`\Zest\Scheduled Clean`) that starts the app in unattended
//! mode. The task runs as the current user without elevation and without
//! opening a window.

use crate::settings::{Frequency, ScheduleSettings};
use std::path::{Path, PathBuf};
use zest_core::{Error, IoContext, Result, brand, process};

/// Task Scheduler folder that holds Zest's tasks.
pub const TASK_FOLDER: &str = brand::DATA_DIR_NAME;

/// Full name of the scheduled-clean task.
pub fn task_name() -> String {
    format!("{TASK_FOLDER}\\Scheduled Clean")
}

/// Command line argument that makes the app clean unattended and exit.
pub const SCHEDULED_ARG: &str = "--scheduled-clean";

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `DOMAIN\user` of the current account.
fn current_user() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(domain) if !domain.is_empty() => format!("{domain}\\{user}"),
        _ => user,
    }
}

/// Human readable description of a schedule, e.g. `Every Monday at 12:00`.
pub fn describe(schedule: &ScheduleSettings) -> String {
    let time = format!(
        "{:02}:{:02}",
        schedule.hour.min(23),
        schedule.minute.min(59)
    );
    match schedule.frequency {
        Frequency::Daily => format!("Every day at {time}"),
        Frequency::Weekly => format!(
            "Every {} at {time}",
            WEEKDAYS[usize::from(schedule.weekday.min(6))]
        ),
        Frequency::Monthly => format!(
            "On day {} of every month at {time}",
            schedule.day_of_month.clamp(1, 28)
        ),
        Frequency::AtLogon => "Each time you sign in".to_owned(),
    }
}

/// Builds the Task Scheduler XML definition for a schedule.
pub fn task_xml(schedule: &ScheduleSettings, exe: &Path, user: &str) -> String {
    let start = format!(
        "2024-01-01T{:02}:{:02}:00",
        schedule.hour.min(23),
        schedule.minute.min(59)
    );
    let trigger = match schedule.frequency {
        Frequency::Daily => format!(
            "<CalendarTrigger><StartBoundary>{start}</StartBoundary><Enabled>true</Enabled>\
             <ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger>"
        ),
        Frequency::Weekly => format!(
            "<CalendarTrigger><StartBoundary>{start}</StartBoundary><Enabled>true</Enabled>\
             <ScheduleByWeek><DaysOfWeek><{day} /></DaysOfWeek><WeeksInterval>1</WeeksInterval>\
             </ScheduleByWeek></CalendarTrigger>",
            day = WEEKDAYS[usize::from(schedule.weekday.min(6))]
        ),
        Frequency::Monthly => format!(
            "<CalendarTrigger><StartBoundary>{start}</StartBoundary><Enabled>true</Enabled>\
             <ScheduleByMonth><DaysOfMonth><Day>{day}</Day></DaysOfMonth><Months>{months}</Months>\
             </ScheduleByMonth></CalendarTrigger>",
            day = schedule.day_of_month.clamp(1, 28),
            months = MONTHS
                .iter()
                .map(|m| format!("<{m} />"))
                .collect::<String>()
        ),
        Frequency::AtLogon => format!(
            "<LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId><Delay>PT2M</Delay></LogonTrigger>",
            xml_escape(user)
        ),
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
  <RegistrationInfo>\n\
    <Description>{description}</Description>\n\
    <URI>\\{name}</URI>\n\
  </RegistrationInfo>\n\
  <Triggers>{trigger}</Triggers>\n\
  <Principals>\n\
    <Principal id=\"Author\">\n\
      <UserId>{user}</UserId>\n\
      <LogonType>InteractiveToken</LogonType>\n\
      <RunLevel>LeastPrivilege</RunLevel>\n\
    </Principal>\n\
  </Principals>\n\
  <Settings>\n\
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n\
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n\
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n\
    <StartWhenAvailable>true</StartWhenAvailable>\n\
    <AllowStartOnDemand>true</AllowStartOnDemand>\n\
    <Enabled>true</Enabled>\n\
    <Hidden>false</Hidden>\n\
    <ExecutionTimeLimit>PT2H</ExecutionTimeLimit>\n\
    <Priority>7</Priority>\n\
  </Settings>\n\
  <Actions Context=\"Author\">\n\
    <Exec>\n\
      <Command>{command}</Command>\n\
      <Arguments>{arguments}</Arguments>\n\
    </Exec>\n\
  </Actions>\n\
</Task>\n",
        description = xml_escape(&format!(
            "Cleans this PC on a schedule with {}. Managed from {} > Options > Scheduling.",
            brand::NAME,
            brand::NAME
        )),
        name = xml_escape(&task_name()),
        user = xml_escape(user),
        command = xml_escape(&exe.to_string_lossy()),
        arguments = SCHEDULED_ARG,
    )
}

fn schtasks() -> PathBuf {
    process::system_tool("schtasks.exe")
}

fn register_named(name: &str, xml: &str) -> Result<()> {
    // schtasks wants the definition as a UTF-16 file with a byte order mark.
    let file = std::env::temp_dir().join(format!("zest-task-{}.xml", std::process::id()));
    let mut bytes = vec![0xFF, 0xFE];
    for unit in xml.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&file, bytes).ctx("writing task definition")?;
    let result = process::run_checked(
        schtasks(),
        [
            std::ffi::OsStr::new("/Create"),
            std::ffi::OsStr::new("/TN"),
            std::ffi::OsStr::new(name),
            std::ffi::OsStr::new("/XML"),
            file.as_os_str(),
            std::ffi::OsStr::new("/F"),
        ],
    );
    let _ = std::fs::remove_file(&file);
    result.map(|_| ())
}

fn exists_named(name: &str) -> bool {
    process::run_capture(schtasks(), ["/Query", "/TN", name])
        .map(|out| out.success())
        .unwrap_or(false)
}

fn remove_named(name: &str) -> Result<()> {
    if !exists_named(name) {
        return Ok(());
    }
    process::run_checked(schtasks(), ["/Delete", "/TN", name, "/F"]).map(|_| ())
}

/// True when the scheduled-clean task is registered.
pub fn is_registered() -> bool {
    exists_named(&task_name())
}

/// Registers (or replaces) the scheduled-clean task for `exe`.
pub fn register(schedule: &ScheduleSettings, exe: &Path) -> Result<()> {
    if !exe.is_file() {
        return Err(Error::NotFound(exe.display().to_string()));
    }
    register_named(&task_name(), &task_xml(schedule, exe, &current_user()))
}

/// Removes the scheduled-clean task. Not an error when it does not exist.
pub fn unregister() -> Result<()> {
    remove_named(&task_name())
}

/// Brings Task Scheduler in line with the settings: registers the task when
/// scheduling is enabled, removes it otherwise.
pub fn apply(schedule: &ScheduleSettings, exe: &Path) -> Result<()> {
    if schedule.enabled {
        register(schedule, exe)
    } else {
        unregister()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(frequency: Frequency) -> ScheduleSettings {
        ScheduleSettings {
            enabled: true,
            frequency,
            hour: 3,
            minute: 30,
            weekday: 4,
            day_of_month: 15,
        }
    }

    #[test]
    fn descriptions() {
        assert_eq!(describe(&schedule(Frequency::Daily)), "Every day at 03:30");
        assert_eq!(
            describe(&schedule(Frequency::Weekly)),
            "Every Friday at 03:30"
        );
        assert_eq!(
            describe(&schedule(Frequency::Monthly)),
            "On day 15 of every month at 03:30"
        );
        assert_eq!(
            describe(&schedule(Frequency::AtLogon)),
            "Each time you sign in"
        );
    }

    #[test]
    fn xml_contains_the_right_trigger_and_action() {
        let exe = Path::new(r"C:\Program Files\Zest & Co\zest.exe");
        let daily = task_xml(&schedule(Frequency::Daily), exe, r"PC\me");
        assert!(daily.contains("<ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay>"));
        assert!(daily.contains("<StartBoundary>2024-01-01T03:30:00</StartBoundary>"));
        assert!(daily.contains(r"<Command>C:\Program Files\Zest &amp; Co\zest.exe</Command>"));
        assert!(daily.contains("<Arguments>--scheduled-clean</Arguments>"));
        assert!(daily.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(daily.contains(r"<UserId>PC\me</UserId>"));

        let weekly = task_xml(&schedule(Frequency::Weekly), exe, "me");
        assert!(weekly.contains("<DaysOfWeek><Friday /></DaysOfWeek>"));
        let monthly = task_xml(&schedule(Frequency::Monthly), exe, "me");
        assert!(monthly.contains("<DaysOfMonth><Day>15</Day></DaysOfMonth>"));
        assert!(monthly.contains("<January /><February />"));
        assert!(monthly.contains("<December /></Months>"));
        let logon = task_xml(&schedule(Frequency::AtLogon), exe, "me");
        assert!(logon.contains("<LogonTrigger>") && !logon.contains("CalendarTrigger"));
    }

    #[test]
    fn registering_needs_a_real_executable() {
        let missing = Path::new(r"C:\Zest No Such Dir\zest.exe");
        assert!(matches!(
            register(&schedule(Frequency::Daily), missing),
            Err(Error::NotFound(_))
        ));
    }

    /// Creates, queries and deletes a throw-away task in Task Scheduler to
    /// prove the generated XML is accepted. It touches system state (briefly
    /// and only with its own uniquely named task), so it runs on request:
    /// `cargo test -p zest-engine -- --ignored task_scheduler_round_trip`
    #[test]
    #[ignore = "registers a temporary task in Task Scheduler"]
    fn task_scheduler_round_trip() {
        let name = format!("{TASK_FOLDER}\\SelfTest-{}", std::process::id());
        let exe = process::system_tool("cmd.exe");
        for frequency in [
            Frequency::Daily,
            Frequency::Weekly,
            Frequency::Monthly,
            Frequency::AtLogon,
        ] {
            let xml = task_xml(&schedule(frequency), &exe, &current_user());
            register_named(&name, &xml).unwrap_or_else(|e| panic!("{frequency:?}: {e}"));
            assert!(exists_named(&name));
            remove_named(&name).unwrap();
            assert!(!exists_named(&name));
        }
        // Removing something that is not there is fine.
        remove_named(&name).unwrap();
    }
}
