## What this changes

<!-- One or two sentences. Link the issue if there is one. -->

## How it was checked

<!-- Tests added or run, and anything you tried by hand. If part of it could not be run for
     real (needs administrator rights, special hardware, ...), say so. -->

## Checklist

- [ ] `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass
- [ ] Code that deletes or changes something goes through `zest_core::delete` / `zest_core::reg`
- [ ] Tests do not touch the real system (temporary folders and `HKCU\Software\ZestTests` only)
- [ ] `docs/FEATURES.md` and `CHANGELOG.md` are updated if behaviour changed
