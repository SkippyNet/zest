//! File work behind a portable update: unpacking the release archive and
//! swapping the program files in place.
//!
//! Windows does not let a running executable be overwritten, but it does let
//! it be renamed. The swap therefore never writes into a file that may be
//! running: the new files are first copied beside the old ones under a
//! temporary name, then each old file is renamed to `<name>.old` and the new
//! one takes its place. If anything fails on the way, every rename is undone,
//! so the folder never ends up with a mix of two versions. The `.old` files
//! are removed the next time Zest starts.

use super::UpdateError;
use std::io::Read;
use std::path::{Path, PathBuf};
use zest_core::brand;

/// The executables of a release. They are swapped by renaming.
const PROGRAM_FILES: [&str; 2] = [brand::GUI_EXE, brand::CLI_EXE];

/// Documents shipped next to the executables. Refreshed when possible; a
/// failure to do so never fails an update.
const DOCUMENT_FILES: [&str; 3] = ["LICENSE", "NOTICE", "README.txt"];

/// Nothing in a release is anywhere near this size when unpacked.
const MAX_ENTRY_BYTES: u64 = 400 * 1024 * 1024;

const NEW_SUFFIX: &str = ".new";
const OLD_SUFFIX: &str = ".old";

fn bad(message: impl Into<String>) -> UpdateError {
    UpdateError::BadPackage(message.into())
}

fn io(action: &str, path: &Path, error: &std::io::Error) -> UpdateError {
    UpdateError::Io(format!("Could not {action} {}: {error}", path.display()))
}

/// Checks that `path` is a Windows program (it starts with the `MZ` mark).
pub(super) fn require_executable(path: &Path) -> Result<(), UpdateError> {
    let mut mark = [0u8; 2];
    let read = std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut mark))
        .is_ok();
    if read && &mark == b"MZ" {
        Ok(())
    } else {
        Err(bad(format!(
            "{} is not a program",
            path.file_name().unwrap_or_default().to_string_lossy()
        )))
    }
}

/// The canonical name of a release file, if `name` is one (any letter case).
fn known_file(name: &str) -> Option<&'static str> {
    PROGRAM_FILES
        .iter()
        .chain(DOCUMENT_FILES.iter())
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
}

/// Unpacks the release files from the portable archive into the empty
/// folder `into`.
///
/// Only the files a release is known to contain are taken, and only from the
/// top level of the archive (or from one enclosing folder). Everything else
/// in the archive, including entries that try to leave the target folder, is
/// ignored. The graphical executable must be present and must be a program.
pub(super) fn unpack(archive: &Path, into: &Path) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive).map_err(|e| io("open", archive, &e))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| bad(format!("it is not a zip archive ({e})")))?;
    if into.exists() {
        // A leftover staging folder of our own from an earlier attempt.
        std::fs::remove_dir_all(into).map_err(|e| io("clear", into, &e))?;
    }
    std::fs::create_dir_all(into).map_err(|e| io("create", into, &e))?;

    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|e| bad(format!("an entry cannot be read ({e})")))?;
        if entry.is_dir() {
            continue;
        }
        // `enclosed_name` rejects absolute paths and paths that climb out.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        let depth = path.components().count();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(known) = known_file(name) else {
            continue;
        };
        if depth > 2 || entry.size() > MAX_ENTRY_BYTES {
            continue;
        }
        let target = into.join(known);
        if target.exists() {
            return Err(bad(format!("it contains {known} more than once")));
        }
        let mut out = std::fs::File::create(&target).map_err(|e| io("write", &target, &e))?;
        let copied = std::io::copy(&mut (&mut entry).take(MAX_ENTRY_BYTES + 1), &mut out)
            .map_err(|e| bad(format!("{known} cannot be unpacked ({e})")))?;
        if copied > MAX_ENTRY_BYTES {
            return Err(bad(format!("{known} is implausibly large")));
        }
    }

    let gui = into.join(brand::GUI_EXE);
    if !gui.is_file() {
        return Err(bad(format!("it does not contain {}", brand::GUI_EXE)));
    }
    for name in PROGRAM_FILES {
        let path = into.join(name);
        if path.is_file() {
            require_executable(&path)?;
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// A free `<name>.old` path beside `target`. An earlier `.old` file that is
/// still running (and so cannot be deleted) is left alone and a numbered
/// name is used instead.
fn free_old_path(target: &Path) -> PathBuf {
    let plain = with_suffix(target, OLD_SUFFIX);
    if !plain.exists() || std::fs::remove_file(&plain).is_ok() {
        return plain;
    }
    (2..100)
        .map(|n| with_suffix(target, &format!("{OLD_SUFFIX}.{n}")))
        .find(|candidate| !candidate.exists() || std::fs::remove_file(candidate).is_ok())
        .unwrap_or(plain)
}

/// One file of the swap and how far it got.
struct Step {
    target: PathBuf,
    staged: PathBuf,
    /// Where the previous file went, once it has been moved aside.
    moved_aside: Option<PathBuf>,
    /// The new file is in place.
    installed: bool,
}

/// Replaces the executables in `program_dir` with those in `unpacked`.
///
/// All or nothing: when a file cannot be replaced, the files replaced so far
/// are put back and the error is returned. Other files in `program_dir`
/// (settings, the portable marker, the data folder) are never touched.
pub(super) fn replace_program_files(
    unpacked: &Path,
    program_dir: &Path,
) -> Result<(), UpdateError> {
    let mut steps: Vec<Step> = Vec::new();

    // 1. Copy the new files beside the old ones under a temporary name. A
    //    folder that cannot be written to fails here, before anything changed.
    for name in PROGRAM_FILES {
        let source = unpacked.join(name);
        if !source.is_file() {
            continue;
        }
        let target = program_dir.join(name);
        let staged = with_suffix(&target, NEW_SUFFIX);
        if let Err(error) = std::fs::copy(&source, &staged) {
            let _ = std::fs::remove_file(&staged);
            discard_staged(&steps);
            return Err(UpdateError::Io(format!(
                "{} cannot write to its own folder ({}): {error}. Move this portable copy to a folder you can write to, or download the new version from the project page.",
                brand::NAME,
                program_dir.display()
            )));
        }
        steps.push(Step {
            target,
            staged,
            moved_aside: None,
            installed: false,
        });
    }
    if steps.is_empty() {
        return Err(bad("it contains no program files"));
    }

    // 2. Swap, one file after the other.
    for index in 0..steps.len() {
        if let Err(error) = swap_one(&mut steps[index]) {
            undo(&mut steps);
            discard_staged(&steps);
            return Err(error);
        }
    }

    // 3. Documents: refreshed when possible.
    for name in DOCUMENT_FILES {
        let source = unpacked.join(name);
        if source.is_file() {
            let _ = std::fs::copy(&source, program_dir.join(name));
        }
    }
    Ok(())
}

fn swap_one(step: &mut Step) -> Result<(), UpdateError> {
    if step.target.exists() {
        let aside = free_old_path(&step.target);
        std::fs::rename(&step.target, &aside).map_err(|e| io("replace", &step.target, &e))?;
        step.moved_aside = Some(aside);
    }
    std::fs::rename(&step.staged, &step.target).map_err(|e| io("install", &step.target, &e))?;
    step.installed = true;
    Ok(())
}

/// Puts every file back the way it was before the swap began.
fn undo(steps: &mut [Step]) {
    for step in steps.iter_mut().rev() {
        if step.installed {
            // Move the new file back out of the way.
            if std::fs::rename(&step.target, &step.staged).is_err() {
                let _ = std::fs::remove_file(&step.target);
            }
            step.installed = false;
        }
        if let Some(aside) = step.moved_aside.take()
            && let Err(error) = std::fs::rename(&aside, &step.target)
        {
            log::error!(
                "could not restore {} from {}: {error}",
                step.target.display(),
                aside.display()
            );
        }
    }
}

fn discard_staged(steps: &[Step]) {
    for step in steps {
        // Our own temporary copies.
        let _ = std::fs::remove_file(&step.staged);
    }
}

/// True for `<program file>.old`, `<program file>.old.<n>` and
/// `<program file>.new`: the names this module creates beside the
/// executables, and nothing else.
fn is_swap_leftover(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    PROGRAM_FILES.iter().any(|program| {
        let Some(rest) = lower.strip_prefix(&program.to_ascii_lowercase()) else {
            return false;
        };
        if rest == NEW_SUFFIX || rest == OLD_SUFFIX {
            return true;
        }
        rest.strip_prefix(OLD_SUFFIX)
            .and_then(|tail| tail.strip_prefix('.'))
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    })
}

/// Deletes the files a finished swap left beside the executables. A file
/// that is still running stays until a later start.
pub(super) fn remove_replaced_files(program_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(program_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if is_file && is_swap_leftover(&name.to_string_lossy()) {
            match std::fs::remove_file(entry.path()) {
                Ok(()) => log::info!(
                    "removed {} from the previous version",
                    name.to_string_lossy()
                ),
                Err(error) => log::debug!(
                    "{} is still in use and stays for now: {error}",
                    name.to_string_lossy()
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zest_core::paths::PORTABLE_MARKER;

    const GUI: &str = brand::GUI_EXE;
    const CLI: &str = brand::CLI_EXE;

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn write_zip(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join("package.zip");
        std::fs::write(&path, zip_of(entries)).unwrap();
        path
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_release_files_are_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write_zip(
            dir.path(),
            &[
                (GUI, b"MZ new gui"),
                (CLI, b"MZ new cli"),
                (PORTABLE_MARKER, b""),
                ("LICENSE", b"licence text"),
                ("readme.TXT", b"read me"),
                ("other.dll", b"MZ something else"),
                ("../zest.exe.evil", b"MZ climbs out"),
                ("data/settings.json", b"{}"),
                ("a/b/zest-cli.exe", b"MZ too deep"),
            ],
        );
        let into = dir.path().join("unpacked");
        // A leftover from an earlier attempt is cleared first.
        std::fs::create_dir_all(&into).unwrap();
        std::fs::write(into.join("stale.txt"), "old").unwrap();

        unpack(&archive, &into).unwrap();
        assert_eq!(names_in(&into), ["LICENSE", "README.txt", CLI, GUI]);
        assert_eq!(std::fs::read(into.join(GUI)).unwrap(), b"MZ new gui");
        assert_eq!(std::fs::read(into.join("README.txt")).unwrap(), b"read me");
        // Nothing escaped the staging folder.
        assert_eq!(names_in(dir.path()), ["package.zip", "unpacked"]);
    }

    #[test]
    fn an_archive_with_one_enclosing_folder_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let archive = write_zip(
            dir.path(),
            &[
                ("Zest-1.0.0/zest.exe", b"MZ gui"),
                ("Zest-1.0.0/zest-cli.exe", b"MZ cli"),
            ],
        );
        let into = dir.path().join("unpacked");
        unpack(&archive, &into).unwrap();
        assert_eq!(names_in(&into), [CLI, GUI]);
    }

    #[test]
    fn damaged_packages_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("unpacked");

        let not_zip = dir.path().join("package.zip");
        std::fs::write(&not_zip, b"this is not an archive").unwrap();
        assert!(matches!(
            unpack(&not_zip, &into),
            Err(UpdateError::BadPackage(_))
        ));

        let no_gui = write_zip(dir.path(), &[(CLI, b"MZ cli"), ("LICENSE", b"x")]);
        let error = unpack(&no_gui, &into).unwrap_err();
        assert!(
            matches!(&error, UpdateError::BadPackage(m) if m.contains(GUI)),
            "{error:?}"
        );

        let not_program = write_zip(dir.path(), &[(GUI, b"<html>error page</html>")]);
        assert!(matches!(
            unpack(&not_program, &into),
            Err(UpdateError::BadPackage(_))
        ));

        let bad_cli = write_zip(dir.path(), &[(GUI, b"MZ gui"), (CLI, b"")]);
        assert!(matches!(
            unpack(&bad_cli, &into),
            Err(UpdateError::BadPackage(_))
        ));

        let twice = write_zip(
            dir.path(),
            &[(GUI, b"MZ one"), ("copy/ZEST.EXE", b"MZ two")],
        );
        assert!(matches!(
            unpack(&twice, &into),
            Err(UpdateError::BadPackage(_))
        ));

        assert!(matches!(
            unpack(&dir.path().join("missing.zip"), &into),
            Err(UpdateError::Io(_))
        ));
    }

    /// A portable folder holding version "old" and an unpacked version "new".
    fn portable_fixture(dir: &Path) -> (PathBuf, PathBuf) {
        let program = dir.join("portable");
        std::fs::create_dir_all(program.join("data")).unwrap();
        std::fs::write(program.join(GUI), b"MZ old gui").unwrap();
        std::fs::write(program.join(CLI), b"MZ old cli").unwrap();
        std::fs::write(program.join(PORTABLE_MARKER), b"").unwrap();
        std::fs::write(
            program.join("data").join("settings.json"),
            b"{\"keep\":true}",
        )
        .unwrap();
        std::fs::write(program.join("LICENSE"), b"old licence").unwrap();

        let unpacked = dir.join("unpacked");
        std::fs::create_dir_all(&unpacked).unwrap();
        std::fs::write(unpacked.join(GUI), b"MZ new gui").unwrap();
        std::fs::write(unpacked.join(CLI), b"MZ new cli").unwrap();
        std::fs::write(unpacked.join("LICENSE"), b"new licence").unwrap();
        (program, unpacked)
    }

    #[test]
    fn program_files_are_swapped_and_the_old_ones_cleared_later() {
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());

        replace_program_files(&unpacked, &program).unwrap();
        assert_eq!(std::fs::read(program.join(GUI)).unwrap(), b"MZ new gui");
        assert_eq!(std::fs::read(program.join(CLI)).unwrap(), b"MZ new cli");
        assert_eq!(
            std::fs::read(program.join("LICENSE")).unwrap(),
            b"new licence"
        );
        // The previous version waits beside the new one.
        assert_eq!(
            std::fs::read(program.join(format!("{GUI}.old"))).unwrap(),
            b"MZ old gui"
        );
        assert_eq!(
            std::fs::read(program.join(format!("{CLI}.old"))).unwrap(),
            b"MZ old cli"
        );
        // What belongs to the user is untouched.
        assert!(program.join(PORTABLE_MARKER).exists());
        assert_eq!(
            std::fs::read(program.join("data").join("settings.json")).unwrap(),
            b"{\"keep\":true}"
        );
        assert!(!program.join(format!("{GUI}.new")).exists());

        // Files that merely look similar are not ours to delete.
        std::fs::write(program.join("zest.exe.older"), b"keep").unwrap();
        std::fs::write(program.join("notes.old"), b"keep").unwrap();
        std::fs::write(program.join("zest.exe.old.x"), b"keep").unwrap();
        std::fs::write(program.join("zest-cli.exe.old.7"), b"ours").unwrap();
        remove_replaced_files(&program);
        assert_eq!(
            names_in(&program),
            [
                "LICENSE",
                "data",
                "notes.old",
                CLI,
                GUI,
                "zest.exe.old.x",
                "zest.exe.older",
                PORTABLE_MARKER,
            ]
        );
    }

    #[test]
    fn a_copy_without_the_command_line_tool_gains_it() {
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());
        std::fs::remove_file(program.join(CLI)).unwrap();
        replace_program_files(&unpacked, &program).unwrap();
        assert_eq!(std::fs::read(program.join(CLI)).unwrap(), b"MZ new cli");
        assert!(!program.join(format!("{CLI}.old")).exists());
    }

    #[test]
    fn a_failed_swap_puts_everything_back() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());

        // Holding the second file open without sharing makes its rename fail
        // after the first file was already swapped.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(program.join(CLI))
            .unwrap();
        let error = replace_program_files(&unpacked, &program).unwrap_err();
        assert!(matches!(error, UpdateError::Io(_)), "{error:?}");
        drop(lock);

        assert_eq!(std::fs::read(program.join(GUI)).unwrap(), b"MZ old gui");
        assert_eq!(std::fs::read(program.join(CLI)).unwrap(), b"MZ old cli");
        assert_eq!(
            std::fs::read(program.join("LICENSE")).unwrap(),
            b"old licence"
        );
        assert_eq!(
            names_in(&program),
            ["LICENSE", "data", CLI, GUI, PORTABLE_MARKER],
            "no temporary files may stay behind"
        );

        // With the lock gone the same update goes through.
        replace_program_files(&unpacked, &program).unwrap();
        assert_eq!(std::fs::read(program.join(CLI)).unwrap(), b"MZ new cli");
    }

    #[test]
    fn an_empty_package_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());
        std::fs::remove_file(unpacked.join(GUI)).unwrap();
        std::fs::remove_file(unpacked.join(CLI)).unwrap();
        assert!(matches!(
            replace_program_files(&unpacked, &program),
            Err(UpdateError::BadPackage(_))
        ));
        assert_eq!(std::fs::read(program.join(GUI)).unwrap(), b"MZ old gui");
        assert_eq!(
            std::fs::read(program.join("LICENSE")).unwrap(),
            b"old licence"
        );
    }

    /// Not a test of its own: [`a_running_program_can_be_replaced`] starts a
    /// copy of the test program with this "test" selected, to have a process
    /// that keeps its own executable busy for a while.
    #[test]
    #[ignore = "helper process for a_running_program_can_be_replaced"]
    fn helper_process_stays_alive() {
        if std::env::var_os("ZEST_SWAP_HELPER").is_some() {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }

    #[test]
    fn a_running_program_can_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());
        // The "old version" is a real program that is running.
        let running = program.join(GUI);
        std::fs::copy(std::env::current_exe().unwrap(), &running).unwrap();
        let mut child = zest_core::process::hidden_command(&running)
            .args([
                "--exact",
                "selfupdate::swap::tests::helper_process_stays_alive",
                "--ignored",
            ])
            .env("ZEST_SWAP_HELPER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        assert!(
            child.try_wait().unwrap().is_none(),
            "the helper must be running"
        );
        // Windows refuses to overwrite it...
        assert!(std::fs::write(&running, b"MZ direct overwrite").is_err());

        // ...but the swap goes through.
        let result = replace_program_files(&unpacked, &program);
        let after_swap = std::fs::read(&running);
        let old = program.join(format!("{GUI}.old"));
        let old_exists = old.exists();
        // While the old version runs, its file cannot be cleared away yet.
        remove_replaced_files(&program);
        let old_survives_while_running = old.exists();

        let _ = child.kill();
        let _ = child.wait();
        result.unwrap();
        assert_eq!(after_swap.unwrap(), b"MZ new gui");
        assert!(old_exists && old_survives_while_running);

        // A second update while the first `.old` is still locked would pick
        // a numbered name; once nothing runs, everything is cleared.
        remove_replaced_files(&program);
        assert!(
            !old.exists(),
            "the old version is removed once it has exited"
        );
    }

    #[test]
    fn a_locked_old_file_does_not_block_the_next_update() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (program, unpacked) = portable_fixture(dir.path());
        let stuck = program.join(format!("{GUI}.old"));
        std::fs::write(&stuck, b"MZ even older").unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&stuck)
            .unwrap();
        replace_program_files(&unpacked, &program).unwrap();
        drop(lock);
        assert_eq!(std::fs::read(program.join(GUI)).unwrap(), b"MZ new gui");
        assert_eq!(std::fs::read(&stuck).unwrap(), b"MZ even older");
        assert_eq!(
            std::fs::read(program.join(format!("{GUI}.old.2"))).unwrap(),
            b"MZ old gui"
        );
        remove_replaced_files(&program);
        assert!(!stuck.exists() && !program.join(format!("{GUI}.old.2")).exists());
    }

    #[test]
    fn leftover_names_are_recognised_exactly() {
        for ours in [
            "zest.exe.old",
            "ZEST.EXE.OLD",
            "zest.exe.old.2",
            "zest-cli.exe.old.14",
            "zest.exe.new",
            "zest-cli.exe.new",
        ] {
            assert!(is_swap_leftover(ours), "{ours}");
        }
        for theirs in [
            "zest.exe",
            "zest-cli.exe",
            "zest.exe.old.",
            "zest.exe.old.x",
            "zest.exe.older",
            "zest.exe.new.txt",
            "report.old",
            "zest.portable",
            "other.exe.old",
        ] {
            assert!(!is_swap_leftover(theirs), "{theirs}");
        }
    }
}
