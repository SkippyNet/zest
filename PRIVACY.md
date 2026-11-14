Zest privacy policy
===================

Zest runs on your PC and keeps what it finds there. It has no telemetry, no
analytics, no crash reporting, no advertising and no account. There is no
server of ours for it to talk to.

This page lists every case in which Zest uses the network, what is sent, and
how to switch it off.


What Zest does by itself
------------------------

One thing: at most once a day, while Zest is open, it asks GitHub whether a
newer version of Zest has been published.

- Where it goes: api.github.com, the public release list of
  https://github.com/SkippyNet/zest
- What is sent: an ordinary web request that names the program and its
  version (for example "Zest/0.9.1"). Like any web request it comes from
  your IP address.
- What is not sent: anything about you, your PC, your files, your programs or
  how you use Zest. There is no installation ID and no cookie.
- How to switch it off: untick "Check for new versions of Zest once a day"
  in the installer, or turn off "Check for updates automatically" under
  Options > About, or run "zest-cli update auto off". Checking by hand keeps
  working.


What Zest does only when you ask
--------------------------------

- Check for updates / Update now (Options > About): the same request as
  above, then a download of the new version from github.com. The download is
  checked against the checksums published with the release.
- Software updates and Health check: Zest asks Windows Package Manager
  (winget, a part of Windows) which of your programs have newer versions.
  Windows Package Manager contacts Microsoft's catalog to answer. On a PC
  where it cannot be used, Zest downloads Microsoft's public catalog file
  from cdn.winget.microsoft.com instead and compares it with your programs on
  your PC; the list of what you have installed does not leave your PC. That
  download can be switched off under Options > About ("Software catalog
  fallback").
- Installing a software update: Windows Package Manager downloads it from the
  publisher's address.
- Driver updates: Zest asks Windows Update, through Windows' own service.
- Links: a link you click in Zest opens in your web browser.


What stays on your PC
---------------------

Settings, clean history, registry backups, file backups and the log are files
in Zest's data folder (Options > About > Open data folder). Zest never
uploads them. The log contains file paths; read it before you attach it to a
problem report.

Hardware details shown under System info are read from your PC and stay
there. A report is only saved or copied when you press the button for it.


Other parties
-------------

When Zest uses the network, the service on the other end sees the request
and handles it under its own policy:

- GitHub (update check, downloads of Zest):
  https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement
- Microsoft (Windows Package Manager, the software catalog, Windows Update):
  https://privacy.microsoft.com/privacystatement

The website https://skippynet.github.io/zest/ is hosted by GitHub Pages. It
loads no trackers and no third-party scripts; it asks GitHub once for the
newest version number.


Changes and questions
---------------------

Changes to this policy are made in the open, in the history of this file:
https://github.com/SkippyNet/zest/commits/main/PRIVACY.md

Questions and problems: https://github.com/SkippyNet/zest/issues
