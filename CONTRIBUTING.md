# Contributing to Zest

Thanks for wanting to help. Zest deletes files and changes system settings for a living, so the
bar for changes is "safe first, then useful, then fast".

## Ways to help

- **Report a problem.** Use the [issue tracker](https://github.com/SkippyNet/zest/issues/new/choose).
  Say what you did, what you expected and what happened, and attach the log from
  *Options → About → Open data folder → logs*. Read it first: logs contain file paths.
- **Suggest a cleaning rule.** Tell us the program, the folder or registry key, and why it is
  safe to remove. Rules live in `crates/cleaner/src/catalog/`.
- **Test a feature on real hardware.** [docs/FEATURES.md](docs/FEATURES.md) lists every feature and
  how far it has been verified. Rows marked *Built* need someone to run them on a spare PC or a
  virtual machine and report back.
- **Send a pull request.** Small and focused is easiest to review.

## Building

You need Windows 10 or 11, Rust 1.92 or later and the Visual Studio C++ build tools.

```powershell
git clone https://github.com/SkippyNet/zest
cd zest
cargo run -p zest            # the app
cargo run -p zest-cli -- --help
```

Before you push:

```powershell
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three commands with the compiler version pinned in the workflow files.

## Rules for code that changes a PC

1. **Analyze and act are separate.** Anything that deletes or changes must first be able to list
   exactly what it would do, and that list is what the user approves.
2. **Go through the guards.** File deletion goes through `zest_core::delete` (which asks
   `zest_core::safety`), registry changes through `zest_core::reg` (which writes a `.reg` backup
   first). Do not call `std::fs::remove_*` or raw registry deletes from feature code.
3. **Never follow links.** Junctions and symbolic links are leaves. Use `zest_core::walk`.
4. **Tests never touch the real system.** Use temporary folders, scratch keys under
   `HKCU\Software\ZestTests\...` and processes the test itself starts. A test must pass on a
   machine that has none of the software the rule is about.
5. **Say what was verified.** If your change adds something that could not be run for real,
   mark it *Built* in `docs/FEATURES.md` and say what still needs checking.

## Running the app without touching your own settings

```powershell
$env:ZEST_DATA_DIR = "$env:TEMP\zest-dev"   # settings, history and backups go here
cargo run -p zest -- --demo                  # made-up data on every page that supports it
cargo run -p zest -- --demo --page health --screenshots docs/screenshots
```

Screenshots in the README and on the website must come from `--demo`. Never publish a
screenshot of a real PC without checking what it reveals.

## Releases

Maintainers cut a release by updating the version in the root `Cargo.toml` and `CHANGELOG.md`,
merging to `main`, and pushing a tag named `v<version>`. The
[release workflow](.github/workflows/release.yml) builds the installer and the portable zip on
GitHub, tests the installer, writes `SHA256SUMS.txt`, attests the build and publishes the
release. The app's update check reads that release.

## Licence

By contributing you agree that your work is licensed under the [Apache License 2.0](LICENSE).
