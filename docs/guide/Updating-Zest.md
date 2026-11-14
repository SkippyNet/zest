# Updating Zest

Zest updates itself from the releases of its own repository,
[github.com/SkippyNet/zest](https://github.com/SkippyNet/zest/releases).

## What happens

1. Once a day, in the background, Zest asks GitHub for the newest release. The request carries
   the Zest version and nothing about you or your PC.
2. If there is a newer version, an **Update to x.y.z** entry appears in the navigation rail and
   under *Options → About*, with the release notes.
3. **Update now** downloads the right file for your copy and checks its SHA-256 against the
   `SHA256SUMS.txt` published with the release. A file that does not match is refused.
4. Then:
   - **Installed copy:** the installer runs quietly, replaces the program and starts it again.
   - **Portable copy:** the two program files in the folder are swapped for the new ones and Zest
     restarts. If anything fails half-way, the old files are put back.

Zest only accepts downloads from this repository's release page.

## Your choices

*Options → About*:

- **Check for updates** checks right now.
- **Skip this version** hides the offer until the next release.
- The automatic daily check can be switched off. You can still check by hand.

From the command line:

```text
zest-cli update check
zest-cli update install
```

## Updating by hand

Download the new installer from the
[latest release](https://github.com/SkippyNet/zest/releases/latest) and run it over the old
version, or unzip the new portable zip over the old folder. Your settings are kept.

## If you built Zest yourself

A copy that is neither installed nor portable (for example one you compiled) is never modified.
It shows the link to the download page instead.

## Limits

- Behind a proxy, the update check only works if the proxy is set through the usual environment
  variables (`HTTPS_PROXY`).
- The background agent keeps running the old version until it is restarted (signing out and in
  again is enough).
