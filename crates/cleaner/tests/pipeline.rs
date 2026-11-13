//! End-to-end tests of catalog -> scan -> clean against a fake PC layout
//! inside a temporary folder. Nothing outside that folder is touched.

use rusqlite::Connection;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use zest_cleaner::{
    Action, Catalog, CleanOptions, Ctx, Exclusion, Group, GroupIcon, Include, IncludeMode,
    MachineDirs, Rule, RuleKind, Section, Target, clean, scan,
};
use zest_core::Progress;
use zest_core::delete::WipeMethod;
use zest_core::paths::UserDirs;

struct FakePc {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    machine: MachineDirs,
    users: Vec<UserDirs>,
}

impl FakePc {
    fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("zest-pipeline-")
            .tempdir()
            .unwrap();
        let root = tmp.path().to_path_buf();
        let machine = MachineDirs::under(&root);
        let users = vec![UserDirs::for_profile(
            root.join("Users").join("tester"),
            None,
        )];
        fs::create_dir_all(&users[0].temp).unwrap();
        fs::create_dir_all(machine.windows.join("Temp")).unwrap();
        FakePc {
            _tmp: tmp,
            root,
            machine,
            users,
        }
    }

    fn user(&self) -> &UserDirs {
        &self.users[0]
    }

    fn catalog(&self, includes: &[Include]) -> Catalog {
        Catalog::build_with(
            &Ctx {
                users: &self.users,
                machine: &self.machine,
                registry: false,
            },
            includes,
        )
    }
}

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![0x5Au8; bytes]).unwrap();
}

fn age(path: &Path, hours: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(hours * 3600))
        .unwrap();
}

fn options(enabled: &[&str]) -> CleanOptions {
    CleanOptions {
        enabled: enabled.iter().map(|s| (*s).to_owned()).collect(),
        // Fake profiles belong to real browser names; the real browser may
        // well be running on the machine executing the tests.
        ignore_running: true,
        ..Default::default()
    }
}

fn chrome_profile(pc: &FakePc) -> PathBuf {
    let profile = pc.user().local.join(r"Google\Chrome\User Data\Default");
    fs::create_dir_all(&profile).unwrap();
    fs::write(
        profile.join("Preferences"),
        r#"{"savefile":{"default_directory":"C:\\x"}}"#,
    )
    .unwrap();
    profile
}

fn cookie_db(path: &Path, hosts: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies (host_key TEXT NOT NULL, name TEXT NOT NULL, value TEXT);",
    )
    .unwrap();
    for host in hosts {
        conn.execute("INSERT INTO cookies VALUES (?1, 'id', 'v')", [host])
            .unwrap();
    }
}

#[test]
fn temp_files_respect_minimum_age() {
    let pc = FakePc::new();
    let old = pc.user().temp.join("old.tmp");
    let fresh = pc.user().temp.join("fresh.tmp");
    let nested = pc.user().temp.join(r"installer\deep\old.bin");
    write(&old, 1000);
    write(&fresh, 500);
    write(&nested, 2000);
    age(&old, 48);
    age(&nested, 72);

    let catalog = pc.catalog(&[]);
    let mut opts = options(&["win.temp"]);
    opts.min_age_hours = 24;
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!((found.bytes, found.items), (3000, 2));

    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!((report.bytes, report.items, report.failed), (3000, 2, 0));
    assert!(!old.exists() && !nested.exists());
    assert!(fresh.exists(), "a file newer than the limit was removed");
    // Emptied sub-folders are pruned, the temp folder itself stays.
    assert!(!pc.user().temp.join("installer").exists());
    assert!(pc.user().temp.is_dir());

    // With no age limit the fresh file goes too.
    opts.min_age_hours = 0;
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.items, 1);
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!fresh.exists());
}

#[test]
fn dry_run_changes_nothing_and_reports_the_same_totals() {
    let pc = FakePc::new();
    let file = pc.user().temp.join("a.tmp");
    write(&file, 4096);
    let catalog = pc.catalog(&[]);
    let opts = options(&["win.temp"]);
    let found = scan(&catalog, &opts, &Progress::new());

    let dry = clean(&catalog, &found, &opts, true, &Progress::new());
    assert!(dry.dry_run);
    assert_eq!((dry.bytes, dry.items), (4096, 1));
    assert!(file.exists());

    let real = clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!real.dry_run);
    assert_eq!((real.bytes, real.items), (dry.bytes, dry.items));
    assert!(!file.exists());
}

#[test]
fn only_enabled_rules_run_and_defaults_exclude_privacy() {
    let pc = FakePc::new();
    let profile = chrome_profile(&pc);
    write(&profile.join(r"Cache\Cache_Data\f_000001"), 10_000);
    write(&profile.join(r"Code Cache\js\abc"), 5_000);
    write(&profile.join("History"), 7_000);
    write(&pc.user().temp.join("t.tmp"), 100);

    let catalog = pc.catalog(&[]);
    let defaults = catalog.default_selection();
    assert!(defaults.contains("chrome.cache") && defaults.contains("win.temp"));
    for (_, rule) in catalog.rules() {
        if rule.kind == RuleKind::Privacy {
            assert!(
                !defaults.contains(&rule.id),
                "{} is privacy but default-on",
                rule.id
            );
        }
    }

    let mut opts = options(&[]);
    opts.enabled = defaults;
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.rule("chrome.cache").map(|r| r.bytes), Some(15_000));
    assert!(found.rule("chrome.history").is_none());
    assert_eq!(
        found
            .bytes_by_group()
            .iter()
            .find(|g| g.0 == "chrome")
            .map(|g| g.1),
        Some(15_000)
    );

    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(
        profile.join("History").exists(),
        "history was removed although not enabled"
    );
    assert!(
        profile.join("Cache").is_dir(),
        "the cache folder itself must stay"
    );
    assert!(!profile.join(r"Cache\Cache_Data").exists());

    // Explicitly enabling history removes it.
    let opts = options(&["chrome.history"]);
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.bytes, 7_000);
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!profile.join("History").exists());
}

#[test]
fn cookie_keep_list_is_honoured() {
    let pc = FakePc::new();
    let profile = chrome_profile(&pc);
    let db = profile.join(r"Network\Cookies");
    cookie_db(
        &db,
        &[
            ".bank.example",
            "www.bank.example",
            ".tracker.example",
            "ads.example",
        ],
    );

    let catalog = pc.catalog(&[]);
    let mut opts = options(&["chrome.cookies"]);
    opts.cookie_keep = vec!["bank.example".to_owned()];
    let found = scan(&catalog, &opts, &Progress::new());
    let result = found
        .rule("chrome.cookies")
        .expect("cookies should be found");
    assert_eq!(result.items, 2);
    assert!(matches!(result.findings[0].target, Target::Special(_)));

    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!((report.items, report.failed), (2, 0));
    let conn = Connection::open(&db).unwrap();
    let mut left: Vec<String> = conn
        .prepare("SELECT host_key FROM cookies")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .flatten()
        .collect();
    left.sort();
    assert_eq!(left, vec![".bank.example", "www.bank.example"]);
    drop(conn);

    // Nothing left to remove: a second scan is silent.
    assert!(
        scan(&catalog, &opts, &Progress::new())
            .rule("chrome.cookies")
            .is_none()
    );

    // Without a keep list the whole database is deleted.
    opts.cookie_keep.clear();
    let found = scan(&catalog, &opts, &Progress::new());
    assert!(found.bytes > 0);
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!db.exists());
}

#[test]
fn site_data_follows_the_keep_list() {
    let pc = FakePc::new();
    let profile = chrome_profile(&pc);
    let kept = profile.join(r"IndexedDB\https_app.bank.example_0.indexeddb.leveldb");
    let gone = profile.join(r"IndexedDB\https_tracker.example_0.indexeddb.leveldb");
    let shared = profile.join(r"Local Storage\leveldb");
    write(&kept.join("000003.log"), 100);
    write(&gone.join("000003.log"), 200);
    write(&shared.join("000005.ldb"), 300);

    let catalog = pc.catalog(&[]);
    let mut opts = options(&["chrome.site_data"]);
    opts.cookie_keep = vec!["bank.example".to_owned()];
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.bytes, 200, "only the unkept origin may be listed");
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(kept.exists() && shared.exists() && !gone.exists());

    opts.cookie_keep.clear();
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.bytes, 400);
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!kept.exists() && !shared.exists());
}

#[test]
fn exclusions_protect_files_folders_and_patterns() {
    let pc = FakePc::new();
    let temp = pc.user().temp.clone();
    write(&temp.join("delete-me.tmp"), 10);
    write(&temp.join("keep-me.tmp"), 10);
    write(&temp.join(r"keepdir\inner.bin"), 10);
    write(&temp.join(r"logs\app.log"), 10);
    write(&temp.join(r"logs\app.bin"), 10);

    let catalog = pc.catalog(&[]);
    let mut opts = options(&["win.temp"]);
    opts.exclusions = vec![
        Exclusion::Path(temp.join("keep-me.tmp")),
        Exclusion::Path(temp.join("keepdir")),
        Exclusion::Pattern {
            dir: temp.clone(),
            pattern: "*.log".to_owned(),
        },
    ];
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.items, 2);
    clean(&catalog, &found, &opts, false, &Progress::new());
    assert!(!temp.join("delete-me.tmp").exists());
    assert!(!temp.join(r"logs\app.bin").exists());
    assert!(temp.join("keep-me.tmp").exists());
    assert!(temp.join(r"keepdir\inner.bin").exists());
    assert!(temp.join(r"logs\app.log").exists());
}

#[test]
fn custom_includes_in_every_mode() {
    let pc = FakePc::new();
    let base = pc.root.join("Custom");
    write(&base.join(r"flat\a.bak"), 10);
    write(&base.join(r"flat\b.txt"), 10);
    write(&base.join(r"flat\sub\c.bak"), 10);
    write(&base.join(r"deep\x.bak"), 10);
    write(&base.join(r"deep\sub\y.bak"), 10);
    write(&base.join(r"whole\any\thing.dat"), 10);
    write(&base.join("single.log"), 10);

    let includes = vec![
        Include {
            path: base.join("flat"),
            pattern: "*.bak".into(),
            mode: IncludeMode::FilesOnly,
        },
        Include {
            path: base.join("deep"),
            pattern: "*.bak".into(),
            mode: IncludeMode::FilesAndSubfolders,
        },
        Include {
            path: base.join("whole"),
            pattern: String::new(),
            mode: IncludeMode::WholeFolder,
        },
        Include {
            path: base.join("single.log"),
            pattern: String::new(),
            mode: IncludeMode::FilesOnly,
        },
    ];
    let catalog = pc.catalog(&includes);
    let custom: Vec<String> = catalog
        .section(Section::Custom)
        .flat_map(|g| g.rules.iter().map(|r| r.id.clone()))
        .collect();
    assert_eq!(custom.len(), 4);
    let mut opts = options(&[]);
    opts.enabled = custom.into_iter().collect();

    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.bytes, 50);
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!(report.failed, 0, "{:?}", report.failures);

    assert!(!base.join(r"flat\a.bak").exists());
    assert!(base.join(r"flat\b.txt").exists());
    assert!(
        base.join(r"flat\sub\c.bak").exists(),
        "FilesOnly must not recurse"
    );
    assert!(!base.join(r"deep\x.bak").exists() && !base.join(r"deep\sub").exists());
    assert!(base.join("deep").is_dir());
    assert!(!base.join("whole").exists());
    assert!(!base.join("single.log").exists());
}

#[test]
fn secure_wipe_still_removes_everything() {
    let pc = FakePc::new();
    write(&pc.user().temp.join("secret.tmp"), 20_000);
    write(&pc.user().temp.join(r"d\secret2.tmp"), 5);
    let catalog = pc.catalog(&[]);
    let mut opts = options(&["win.temp"]);
    opts.wipe = WipeMethod::Dod3;
    let found = scan(&catalog, &opts, &Progress::new());
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!((report.bytes, report.failed), (20_005, 0));
    assert_eq!(fs::read_dir(&pc.user().temp).unwrap().count(), 0);
}

#[test]
fn files_in_use_are_counted_separately_not_as_failures() {
    let pc = FakePc::new();
    let locked = pc.user().temp.join("locked.tmp");
    write(&locked, 64);
    write(&pc.user().temp.join("free.tmp"), 32);
    // Opening without FILE_SHARE_DELETE makes deletion fail with a sharing violation.
    use std::os::windows::fs::OpenOptionsExt;
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&locked)
        .unwrap();

    let catalog = pc.catalog(&[]);
    let opts = options(&["win.temp"]);
    let found = scan(&catalog, &opts, &Progress::new());
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!(
        (report.items, report.bytes, report.in_use, report.failed),
        (1, 32, 1, 0)
    );
    assert!(locked.exists());
    drop(handle);
}

#[test]
fn running_program_blocks_its_group() {
    let pc = FakePc::new();
    let cache = pc.root.join("AppCache");
    write(&cache.join("blob.bin"), 123);
    let me = std::env::current_exe()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let catalog = Catalog {
        groups: vec![Group {
            id: "selftest".into(),
            section: Section::Applications,
            name: "Self Test".into(),
            icon: GroupIcon::App,
            processes: vec![me],
            rules: vec![Rule {
                id: "selftest.cache".into(),
                name: "Cache".into(),
                description: "test".into(),
                kind: RuleKind::Junk,
                default_enabled: true,
                warning: None,
                requires_admin: false,
                age_filtered: false,
                actions: vec![Action::Files {
                    dir: cache.clone(),
                    pattern: "*".into(),
                    recursive: true,
                    min_age_days: 0,
                }],
            }],
        }],
    };
    let mut opts = CleanOptions {
        enabled: HashSet::from(["selftest.cache".to_owned()]),
        ..Default::default()
    };
    let found = scan(&catalog, &opts, &Progress::new());
    assert_eq!(found.blockers(), vec!["Self Test is running"]);
    assert_eq!(found.bytes, 123, "blocked groups are still measured");

    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!(report.items, 0);
    assert_eq!(
        report.rules[0].skipped.as_deref(),
        Some("Self Test is running")
    );
    assert!(cache.join("blob.bin").exists());

    opts.ignore_running = true;
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!(report.items, 1);
    assert!(!cache.join("blob.bin").exists());
}

#[test]
fn stale_scan_results_are_harmless() {
    let pc = FakePc::new();
    let file = pc.user().temp.join("gone-soon.tmp");
    write(&file, 10);
    let catalog = pc.catalog(&[]);
    let opts = options(&["win.temp"]);
    let found = scan(&catalog, &opts, &Progress::new());
    fs::remove_file(&file).unwrap();
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!((report.failed, report.in_use, report.bytes), (0, 0, 0));
}

#[test]
fn a_rule_can_never_reach_outside_through_a_junction() {
    let pc = FakePc::new();
    let precious = pc.root.join("Documents");
    write(&precious.join("thesis.docx"), 999);
    let temp = pc.user().temp.clone();
    write(&temp.join("junk.tmp"), 1);
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(temp.join("shortcut-to-docs"))
        .arg(&precious)
        .output()
        .unwrap();
    assert!(out.status.success());

    let catalog = pc.catalog(&[]);
    let opts = options(&["win.temp"]);
    let found = scan(&catalog, &opts, &Progress::new());
    // The junction counts as one zero-byte item; nothing behind it is listed.
    assert_eq!((found.bytes, found.items), (1, 2));
    let report = clean(&catalog, &found, &opts, false, &Progress::new());
    assert_eq!(report.failed, 0, "{:?}", report.failures);
    assert!(!temp.join("shortcut-to-docs").exists());
    assert_eq!(fs::read(precious.join("thesis.docx")).unwrap().len(), 999);
}

#[test]
fn cancelled_scan_returns_quickly_and_cleanly() {
    let pc = FakePc::new();
    for i in 0..50 {
        write(&pc.user().temp.join(format!("f{i}.tmp")), 10);
    }
    let catalog = pc.catalog(&[]);
    let opts = options(&["win.temp"]);
    let progress = Progress::new();
    progress.cancel();
    let found = scan(&catalog, &opts, &progress);
    assert!(found.results.is_empty());
    assert_eq!(fs::read_dir(&pc.user().temp).unwrap().count(), 50);
}
