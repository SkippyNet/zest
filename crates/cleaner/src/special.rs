//! Operations that are more than deleting a file or a registry value.

use crate::cookies::KeepList;
use crate::exclude::Excluder;
use crate::model::{CleanOptions, Finding, RuleResult, Special, Target};
use crate::sqlite;
use std::collections::HashSet;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use zest_core::fmt::format_count;
use zest_core::walk::{self, EntryKind};
use zest_core::{Error, IoContext, Result, cmdline, paths, process};

/// Shared state for one scan or clean run.
pub(crate) struct RunContext<'a> {
    pub opts: &'a CleanOptions,
    pub excluder: Excluder,
    pub keep: KeepList,
    /// Lower-cased executable names of running processes.
    pub running: HashSet<String>,
    pub elevated: bool,
    pub now: SystemTime,
}

impl<'a> RunContext<'a> {
    pub fn new(opts: &'a CleanOptions) -> Self {
        RunContext {
            opts,
            excluder: Excluder::new(&opts.exclusions),
            keep: KeepList::new(&opts.cookie_keep),
            running: process::running_process_names(),
            elevated: zest_core::elevation::is_elevated(),
            now: SystemTime::now(),
        }
    }
}

/// Size of an empty event log file; clearing a log shrinks it to this.
const EMPTY_EVTX_BYTES: u64 = 69_632;

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn sidecars(db: &Path) -> Vec<PathBuf> {
    ["-journal", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut name = db.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        })
        .collect()
}

fn special_finding(special: &Special, size: u64, items: u64, note: String) -> Finding {
    Finding {
        target: Target::Special(special.clone()),
        size,
        items,
        note: Some(note),
    }
}

fn push_path(out: &mut RuleResult, ctx: &RunContext, path: PathBuf, is_dir: bool) {
    if ctx.excluder.excludes_path(&path) || zest_core::safety::is_protected(&path) {
        return;
    }
    if is_dir {
        if ctx.excluder.touches_dir(&path) {
            return;
        }
        let size = walk::tree_size(&path, None);
        out.push(Finding {
            target: Target::Dir(path),
            size: size.bytes,
            items: size.files.max(1),
            note: None,
        });
    } else {
        let size = file_len(&path);
        out.push(Finding::file(path, size));
    }
}

/// Appends what `special` would remove to `out`.
pub(crate) fn scan(special: &Special, ctx: &RunContext, out: &mut RuleResult) {
    // While the owning program runs its databases are locked. Probing them
    // would only wait for lock timeouts, so database-backed items are
    // reported once the program is closed and the analysis repeated.
    let locked = out.blocked_by.is_some();
    if locked
        && matches!(
            special,
            Special::ChromiumDownloads { .. }
                | Special::ChromiumAutofill { .. }
                | Special::FirefoxHistory { .. }
                | Special::SqliteVacuum { .. }
        )
    {
        return;
    }
    match special {
        Special::RecycleBin => {
            if let Some((bytes, items)) = recycle_bin_usage()
                && items > 0
            {
                out.push(special_finding(
                    special,
                    bytes,
                    items,
                    format!("{} items", format_count(items)),
                ));
            }
        }
        Special::DnsCache => {
            out.push(special_finding(special, 0, 1, "Resolver cache".to_owned()));
        }
        Special::Clipboard => {
            if clipboard_has_content() {
                out.push(special_finding(special, 0, 1, "Copied content".to_owned()));
            }
        }
        Special::EventLogs => {
            let dir = paths::system32_dir().join("winevt").join("Logs");
            let mut bytes = 0u64;
            let mut logs = 0u64;
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    if len > EMPTY_EVTX_BYTES {
                        bytes += len - EMPTY_EVTX_BYTES;
                        logs += 1;
                    }
                }
            }
            if logs > 0 {
                out.push(special_finding(
                    special,
                    bytes,
                    logs,
                    format!("{} logs with entries", format_count(logs)),
                ));
            }
        }
        Special::BrokenShortcuts { dir } => {
            for shortcut in broken_shortcuts(dir) {
                push_path(out, ctx, shortcut, false);
            }
        }
        Special::ChromiumCookies { db } => {
            scan_cookies(special, db, "cookies", "host_key", ctx, out)
        }
        Special::FirefoxCookies { db } => {
            scan_cookies(special, db, "moz_cookies", "host", ctx, out)
        }
        Special::ChromiumSiteData { profile } => scan_chromium_site_data(profile, ctx, out),
        Special::FirefoxSiteData { profile } => scan_firefox_site_data(profile, ctx, out),
        Special::ChromiumDownloads { db } => {
            scan_rows(special, db, "downloads", "download records", out);
        }
        Special::ChromiumAutofill { db } => {
            scan_rows(special, db, "autofill", "form entries", out);
        }
        Special::FirefoxHistory { db } => {
            if db.is_file() {
                match sqlite::firefox_history_count(db) {
                    Ok(0) => {}
                    Ok(visits) => out.push(special_finding(
                        special,
                        0,
                        visits,
                        format!("{} visits", format_count(visits)),
                    )),
                    Err(e) => {
                        log::debug!("{e}");
                        out.push(special_finding(
                            special,
                            0,
                            1,
                            "History (database in use)".to_owned(),
                        ));
                    }
                }
            }
        }
        Special::SqliteVacuum { dbs } => {
            let mut bytes = 0u64;
            let mut count = 0u64;
            for db in dbs.iter().filter(|db| db.is_file()) {
                if let Ok(free) = sqlite::reclaimable_bytes(db)
                    && free > 0
                {
                    bytes += free;
                    count += 1;
                }
            }
            if count > 0 {
                out.push(special_finding(
                    special,
                    bytes,
                    count,
                    format!("{} databases", format_count(count)),
                ));
            }
        }
        Special::JsonRemoveKeys { file, keys } => {
            if let Ok(json) = read_json(file) {
                let present = keys.iter().filter(|k| json_get(&json, k).is_some()).count() as u64;
                if present > 0 {
                    out.push(special_finding(
                        special,
                        0,
                        present,
                        format!("{} settings", format_count(present)),
                    ));
                }
            }
        }
    }
}

fn scan_rows(special: &Special, db: &Path, table: &str, what: &str, out: &mut RuleResult) {
    if !db.is_file() {
        return;
    }
    match sqlite::count_rows(db, table) {
        Ok(0) => {}
        Ok(rows) => out.push(special_finding(
            special,
            0,
            rows,
            format!("{} {what}", format_count(rows)),
        )),
        Err(e) => log::debug!("{e}"),
    }
}

fn scan_cookies(
    special: &Special,
    db: &Path,
    table: &str,
    column: &str,
    ctx: &RunContext,
    out: &mut RuleResult,
) {
    if !db.is_file() || ctx.excluder.excludes_path(db) {
        return;
    }
    let counts = if out.blocked_by.is_some() {
        Err(zest_core::Error::Other("the browser is running".to_owned()))
    } else {
        sqlite::count_cookies(db, table, column, &ctx.keep)
    };
    if ctx.keep.is_empty() {
        // Nothing to preserve: the whole database goes, which also frees space.
        let cookies = counts.map(|(removable, _)| removable).unwrap_or(1);
        if cookies == 0 {
            return;
        }
        out.push(Finding {
            target: Target::File(db.to_path_buf()),
            size: file_len(db),
            items: cookies,
            note: Some(format!("{} cookies", format_count(cookies))),
        });
        for sidecar in sidecars(db).into_iter().filter(|p| p.is_file()) {
            let size = file_len(&sidecar);
            out.push(Finding {
                target: Target::File(sidecar),
                size,
                items: 0,
                note: None,
            });
        }
        return;
    }
    match counts {
        Ok((0, _)) => {}
        Ok((removable, kept)) => out.push(special_finding(
            special,
            0,
            removable,
            format!(
                "{} cookies ({} kept)",
                format_count(removable),
                format_count(kept)
            ),
        )),
        Err(e) => {
            log::debug!("{e}");
            out.push(special_finding(
                special,
                0,
                1,
                "Cookies (database in use)".to_owned(),
            ));
        }
    }
}

/// Host of a Chromium per-origin storage folder such as
/// `https_www.example.com_0.indexeddb.leveldb`.
fn chromium_origin_host(name: &str) -> Option<&str> {
    // The name is "<scheme>_<host>_<port>[.indexeddb.<kind>]"; the host may
    // itself contain dots and underscores, so cut on the first and the last
    // underscore of the part before the storage suffix.
    let base = name.find(".indexeddb").map_or(name, |i| &name[..i]);
    let (scheme, rest) = base.split_once('_')?;
    let (host, port) = rest.rsplit_once('_')?;
    let scheme_ok = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    let port_ok = !port.is_empty() && port.chars().all(|c| c.is_ascii_digit());
    (scheme_ok && port_ok && !host.is_empty()).then_some(host)
}

fn scan_chromium_site_data(profile: &Path, ctx: &RunContext, out: &mut RuleResult) {
    // Per-origin stores can honour the keep list.
    for store in ["IndexedDB", "databases"] {
        let Ok(entries) = std::fs::read_dir(profile.join(store)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            match chromium_origin_host(&name) {
                Some(host) if ctx.keep.keeps(host) => {}
                Some(_) => push_path(out, ctx, entry.path(), is_dir),
                // Index files that describe all origins: only removable when
                // nothing is kept.
                None if ctx.keep.is_empty() => push_path(out, ctx, entry.path(), is_dir),
                None => {}
            }
        }
    }
    // Stores shared by all sites cannot be filtered; they are only removed
    // when the keep list is empty.
    if ctx.keep.is_empty() {
        for shared in [
            r"Local Storage\leveldb",
            "File System",
            r"Service Worker\Database",
            "WebStorage",
        ] {
            let path = profile.join(shared);
            if path.is_dir() {
                push_path(out, ctx, path, true);
            }
        }
    }
}

/// Host of a Firefox storage folder such as `https+++www.example.com^userContextId=2`.
fn firefox_origin_host(name: &str) -> Option<&str> {
    let (scheme, rest) = name.split_once("+++")?;
    if scheme.starts_with("moz-extension") || scheme == "about" || scheme == "resource" {
        return None;
    }
    let host = rest.split(['^', '+']).next().unwrap_or(rest);
    (!host.is_empty()).then_some(host)
}

fn scan_firefox_site_data(profile: &Path, ctx: &RunContext, out: &mut RuleResult) {
    if let Ok(entries) = std::fs::read_dir(profile.join("storage").join("default")) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(host) = firefox_origin_host(&name) else {
                continue;
            };
            if !ctx.keep.keeps(host) && entry.file_type().is_ok_and(|t| t.is_dir()) {
                push_path(out, ctx, entry.path(), true);
            }
        }
    }
    if ctx.keep.is_empty() {
        let legacy = profile.join("webappsstore.sqlite");
        if legacy.is_file() {
            push_path(out, ctx, legacy, false);
        }
    }
}

/// Runs `special`. Returns `(bytes freed, items removed)`.
pub(crate) fn clean(special: &Special, ctx: &RunContext) -> Result<(u64, u64)> {
    match special {
        Special::RecycleBin => {
            let (bytes, items) = recycle_bin_usage().unwrap_or((0, 0));
            if items == 0 {
                return Ok((0, 0));
            }
            empty_recycle_bin()?;
            Ok((bytes, items))
        }
        Special::DnsCache => {
            process::run_checked(process::system_tool("ipconfig.exe"), ["/flushdns"])?;
            Ok((0, 1))
        }
        Special::Clipboard => {
            clear_clipboard()?;
            Ok((0, 1))
        }
        Special::EventLogs => {
            if !ctx.elevated {
                return Err(Error::NeedsElevation("clear event logs".to_owned()));
            }
            let dir = paths::system32_dir().join("winevt").join("Logs");
            let before = walk::tree_size(&dir, None).bytes;
            let cleared = clear_event_logs()?;
            let after = walk::tree_size(&dir, None).bytes;
            Ok((before.saturating_sub(after), cleared))
        }
        Special::ChromiumCookies { db } => Ok((
            0,
            sqlite::delete_cookies(db, "cookies", "host_key", &ctx.keep)?,
        )),
        Special::FirefoxCookies { db } => Ok((
            0,
            sqlite::delete_cookies(db, "moz_cookies", "host", &ctx.keep)?,
        )),
        Special::ChromiumDownloads { db } => {
            let rows = sqlite::count_rows(db, "downloads")?;
            sqlite::clear_tables(
                db,
                &[
                    "downloads",
                    "downloads_url_chains",
                    "downloads_slices",
                    "downloads_reroute_info",
                ],
            )?;
            Ok((0, rows))
        }
        Special::ChromiumAutofill { db } => {
            let rows = sqlite::count_rows(db, "autofill")?;
            sqlite::clear_tables(db, &["autofill", "autofill_dates"])?;
            Ok((0, rows))
        }
        Special::FirefoxHistory { db } => Ok((0, sqlite::firefox_clear_history(db)?)),
        Special::SqliteVacuum { dbs } => {
            let mut bytes = 0;
            let mut count = 0;
            for db in dbs.iter().filter(|db| db.is_file()) {
                if sqlite::reclaimable_bytes(db).unwrap_or(0) == 0 {
                    continue;
                }
                match sqlite::vacuum(db) {
                    Ok(saved) => {
                        bytes += saved;
                        count += 1;
                    }
                    Err(e) => log::warn!("{e}"),
                }
            }
            Ok((bytes, count))
        }
        Special::JsonRemoveKeys { file, keys } => {
            let mut json = read_json(file)?;
            let mut removed = 0;
            for key in keys {
                if json_remove(&mut json, key) {
                    removed += 1;
                }
            }
            if removed > 0 {
                write_json_atomically(file, &json)?;
            }
            Ok((0, removed))
        }
        // These only ever produce plain file/folder findings.
        Special::BrokenShortcuts { .. }
        | Special::ChromiumSiteData { .. }
        | Special::FirefoxSiteData { .. } => Ok((0, 0)),
    }
}

// ── JSON helpers ─────────────────────────────────────────────────────

fn read_json(file: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(file).ctx(format!("reading {}", file.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn json_get<'a>(json: &'a serde_json::Value, dotted: &str) -> Option<&'a serde_json::Value> {
    dotted
        .split('.')
        .try_fold(json, |node, part| node.get(part))
}

fn json_remove(json: &mut serde_json::Value, dotted: &str) -> bool {
    let Some((parents, leaf)) = dotted.rsplit_once('.').or(Some(("", dotted))) else {
        return false;
    };
    let mut node = json;
    if !parents.is_empty() {
        for part in parents.split('.') {
            match node.get_mut(part) {
                Some(next) => node = next,
                None => return false,
            }
        }
    }
    node.as_object_mut()
        .is_some_and(|map| map.remove(leaf).is_some())
}

fn write_json_atomically(file: &Path, json: &serde_json::Value) -> Result<()> {
    let mut tmp = file.as_os_str().to_os_string();
    tmp.push(".zest-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, serde_json::to_vec(json)?).ctx(format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, file)
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
        .ctx(format!("replacing {}", file.display()))
}

// ── Recycle Bin ──────────────────────────────────────────────────────

/// `(bytes, items)` currently in the Recycle Bin across all drives.
pub fn recycle_bin_usage() -> Option<(u64, u64)> {
    use windows::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    use windows::core::PCWSTR;
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a correctly sized, initialised out structure.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.ok()?;
    Some((info.i64Size.max(0) as u64, info.i64NumItems.max(0) as u64))
}

fn empty_recycle_bin() -> Result<()> {
    use windows::Win32::UI::Shell::{
        SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW,
    };
    use windows::core::PCWSTR;
    let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
    // SAFETY: null root path means "all drives"; no window handle is needed
    // because every UI flag is suppressed.
    match unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), flags) } {
        Ok(()) => Ok(()),
        // E_UNEXPECTED is returned when the bin is already empty.
        Err(e) if e.code().0 as u32 == 0x8000_FFFF => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ── Clipboard ────────────────────────────────────────────────────────

fn clipboard_has_content() -> bool {
    use windows::Win32::System::DataExchange::CountClipboardFormats;
    // SAFETY: no arguments, no preconditions.
    unsafe { CountClipboardFormats() > 0 }
}

fn clear_clipboard() -> Result<()> {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard};
    // SAFETY: the clipboard is opened without an owner window, emptied and
    // closed again on every path.
    unsafe {
        OpenClipboard(None)?;
        let emptied = EmptyClipboard();
        let _ = CloseClipboard();
        emptied?;
    }
    Ok(())
}

// ── Event logs ───────────────────────────────────────────────────────

fn clear_event_logs() -> Result<u64> {
    use windows::Win32::System::EventLog::{
        EvtClearLog, EvtClose, EvtNextChannelPath, EvtOpenChannelEnum,
    };
    use windows::core::PCWSTR;

    let mut cleared = 0u64;
    // SAFETY: the enumeration handle is closed before returning; the buffer
    // passed to EvtNextChannelPath is valid for its full length and the
    // returned name is NUL terminated within `used` characters.
    unsafe {
        let channels = EvtOpenChannelEnum(None, 0)?;
        let mut buffer = vec![0u16; 512];
        loop {
            let mut used = 0u32;
            match EvtNextChannelPath(channels, Some(&mut buffer), &mut used) {
                Ok(()) => {
                    if EvtClearLog(None, PCWSTR(buffer.as_ptr()), PCWSTR::null(), 0).is_ok() {
                        cleared += 1;
                    }
                }
                // ERROR_INSUFFICIENT_BUFFER: grow and retry the same channel.
                Err(e) if e.code().0 as u32 == 0x8007_007A => {
                    buffer.resize((used as usize).max(buffer.len() * 2), 0);
                }
                // ERROR_NO_MORE_ITEMS or anything unexpected ends the walk.
                Err(_) => break,
            }
        }
        let _ = EvtClose(channels);
    }
    Ok(cleared)
}

// ── Shortcuts ────────────────────────────────────────────────────────

struct ComGuard(bool);

impl ComGuard {
    fn new() -> Self {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
        // SAFETY: balanced by CoUninitialize in Drop when initialisation
        // succeeded (S_OK or S_FALSE). A different existing apartment model
        // is fine for our purposes and is simply left alone.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComGuard(hr.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: paired with the successful CoInitializeEx above.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// The raw target path stored in a `.lnk` file, if it has one.
fn shortcut_target(link_path: &Path) -> Option<String> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink};
    use windows::core::{Interface, PCWSTR};

    let wide: Vec<u16> = link_path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: COM is initialised by the caller's guard; `wide` is NUL
    // terminated and `buffer` is valid for its whole length.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        let mut buffer = [0u16; 1024];
        link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        let target = String::from_utf16_lossy(&buffer[..len]);
        (!target.trim().is_empty()).then_some(target)
    }
}

/// Shortcuts below `dir` whose target is definitely gone.
fn broken_shortcuts(dir: &Path) -> Vec<PathBuf> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut links = Vec::new();
    walk::walk_tree(dir, None, &mut |path, _, kind| {
        if kind == EntryKind::File
            && path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
        {
            links.push(path.to_path_buf());
        }
    });
    if links.is_empty() {
        return links;
    }
    let _com = ComGuard::new();
    links
        .into_iter()
        .filter(|link| {
            shortcut_target(link).is_some_and(|target| cmdline::check_path(&target).is_missing())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_hosts() {
        assert_eq!(
            chromium_origin_host("https_www.example.com_0.indexeddb.leveldb"),
            Some("www.example.com")
        );
        assert_eq!(
            chromium_origin_host("https_example.com_8443.indexeddb.blob"),
            Some("example.com")
        );
        assert_eq!(
            chromium_origin_host("http_localhost_3000"),
            Some("localhost")
        );
        assert_eq!(chromium_origin_host("Databases.db"), None);
        assert_eq!(chromium_origin_host("LOCK"), None);
        assert_eq!(
            firefox_origin_host("https+++www.example.com"),
            Some("www.example.com")
        );
        assert_eq!(
            firefox_origin_host("https+++example.com^userContextId=3"),
            Some("example.com")
        );
        assert_eq!(
            firefox_origin_host("http+++localhost+8080"),
            Some("localhost")
        );
        assert_eq!(firefox_origin_host("moz-extension+++abcd-1234"), None);
        assert_eq!(firefox_origin_host("permanent"), None);
    }

    #[test]
    fn json_key_removal() {
        let mut json: serde_json::Value = serde_json::from_str(
            r#"{"savefile":{"default_directory":"C:\\x","type":1},"download":{"default_directory":"D:\\dl"},"top":true}"#,
        )
        .unwrap();
        assert!(json_get(&json, "savefile.default_directory").is_some());
        assert!(json_remove(&mut json, "savefile.default_directory"));
        assert!(!json_remove(&mut json, "savefile.default_directory"));
        assert!(!json_remove(&mut json, "missing.parent.key"));
        assert!(json_remove(&mut json, "top"));
        assert_eq!(
            json,
            serde_json::json!({"savefile":{"type":1},"download":{"default_directory":"D:\\dl"}})
        );
    }

    #[test]
    fn json_file_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("Preferences");
        std::fs::write(
            &file,
            r#"{"selectfile":{"last_directory":"C:\\secret"},"keep":[1,2,3]}"#,
        )
        .unwrap();
        let opts = CleanOptions::default();
        let ctx = RunContext::new(&opts);
        let special = Special::JsonRemoveKeys {
            file: file.clone(),
            keys: vec![
                "selectfile.last_directory".into(),
                "savefile.default_directory".into(),
            ],
        };
        let mut found = RuleResult::default();
        scan(&special, &ctx, &mut found);
        assert_eq!(found.items, 1);
        assert_eq!(clean(&special, &ctx).unwrap(), (0, 1));
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(after, serde_json::json!({"selectfile":{},"keep":[1,2,3]}));
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            1,
            "temp file left behind"
        );
        // Nothing left to remove: scan is silent.
        let mut again = RuleResult::default();
        scan(&special, &ctx, &mut again);
        assert!(again.is_empty());
    }

    #[test]
    fn recycle_bin_query_works() {
        // Read-only: just make sure the query does not fail on this system.
        assert!(recycle_bin_usage().is_some());
    }

    #[test]
    fn shortcut_resolution_finds_only_dead_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let alive = tmp.path().join("alive.lnk");
        let dead = tmp.path().join("dead.lnk");
        let target_gone = zest_core::paths::system_drive().join(r"Zest No Such Dir\gone.exe");
        let script = format!(
            "$s = New-Object -ComObject WScript.Shell; \
             $a = $s.CreateShortcut('{}'); $a.TargetPath = '{}'; $a.Save(); \
             $d = $s.CreateShortcut('{}'); $d.TargetPath = '{}'; $d.Save()",
            alive.display(),
            paths::system32_dir().join("notepad.exe").display(),
            dead.display(),
            target_gone.display(),
        );
        let out = process::run_capture(
            paths::system32_dir().join(r"WindowsPowerShell\v1.0\powershell.exe"),
            ["-NoProfile", "-NonInteractive", "-Command", &script],
        )
        .unwrap();
        assert!(out.success(), "{}", out.stderr);
        assert!(alive.is_file() && dead.is_file());
        std::fs::write(tmp.path().join("notes.txt"), b"not a shortcut").unwrap();

        assert_eq!(broken_shortcuts(tmp.path()), vec![dead]);
        assert!(broken_shortcuts(&tmp.path().join("missing")).is_empty());
    }
}
