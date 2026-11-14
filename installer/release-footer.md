---

### Installing

| | |
|---|---|
| **Installer** | Download `Zest-{version}-x64-setup.exe` and run it. No administrator rights needed. |
| **Portable** | Unzip `Zest-{version}-x64-portable.zip` anywhere and start `zest.exe`. Settings stay in that folder. |

Already using Zest? **Options → About → Check for updates** installs this version for you.

### Checking your download

Windows SmartScreen may warn about these files because they are not code-signed yet. Choose **More info**, then **Run anyway** — or check first:

- Compare the file's SHA-256 with `SHA256SUMS.txt` (`Get-FileHash <file>` in PowerShell).
- Confirm it was built from this repository by GitHub Actions: `gh attestation verify <file> --repo {repo}`.

[Code signing policy](https://github.com/{repo}#code-signing-policy) · [Privacy policy](https://github.com/{repo}/blob/main/PRIVACY.md)
