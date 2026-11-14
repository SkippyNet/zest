# Security

Zest runs with the rights of the person using it and, for some tools, as administrator. A bug in
it can delete the wrong thing or be used to make it do so. Reports are very welcome.

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Use GitHub's private reporting
instead: [Report a vulnerability](https://github.com/SkippyNet/zest/security/advisories/new).

Include what you found, how to reproduce it and which version you used. You will get an answer
as soon as a maintainer has read it, and credit in the release notes if you want it.

## What counts

- Deleting, overwriting or changing something the user did not approve.
- Following a junction or symbolic link out of the folder being cleaned.
- Running a program or command chosen by another, less privileged, user or by file contents.
- Problems in the update path: accepting an update that does not match its checksum, or
  downloading from anywhere but this repository's releases.
- Anything that leaks data from the PC. Zest is meant to make exactly two kinds of network
  request: the update check against this repository, and the public software catalog when the
  user asks for software updates.

## Supported versions

Only the latest release receives fixes.

## Verifying a download

Every release is built by [GitHub Actions](.github/workflows/release.yml) from the tagged source.

```powershell
# 1. Checksum: compare with SHA256SUMS.txt from the same release
Get-FileHash .\Zest-<version>-x64-setup.exe -Algorithm SHA256

# 2. Provenance: confirm the file was built by this repository's workflow
gh attestation verify .\Zest-<version>-x64-setup.exe --repo SkippyNet/zest
```
