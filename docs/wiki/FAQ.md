# Questions and answers

### Is it really free?

Yes. No paid tier, no trial, no ads, no bundled software. The code is public under the
[Apache License 2.0](https://github.com/SkippyNet/zest/blob/main/LICENSE).

### Is it safe?

Zest shows you what it will remove before it removes anything, refuses to touch Windows and your
personal folders, and backs up the registry before changing it. The details are on the
[Safety](Safety) page. It is also a young program: the
[feature plan](https://github.com/SkippyNet/zest/blob/main/docs/FEATURES.md) says which features
have been verified on real PCs and which have only been tested on sandboxed data. Start with
**Analyze** and read the list.

### Windows says "Windows protected your PC" when I run the download.

That is SmartScreen reacting to a file that is new and not code-signed. See
[Installing](Installing#windows-protected-your-pc) for how to continue and how to verify the file.

### Will cleaning sign me out of websites?

Not unless you ask for it. Cookie cleaning is off by default in Custom clean. Health check does
remove tracking cookies, but leaves the sites on your keep list alone
(*Options → Cookies*). Saved passwords are never touched unless you tick that rule yourself.

### Does cleaning the registry make my PC faster?

Not noticeably. It removes entries that point to things that no longer exist, which tidies up
after uninstalled programs and can fix the odd error message. Every fix is backed up and can be
restored.

### How does Zest know which of my programs have updates? It has never seen my PC.

It does not carry a list of software. It asks **Windows Package Manager** (winget), which is part
of Windows 10 and 11 and has a public catalog of many thousands of programs maintained by
Microsoft and the publishers. Whatever you have installed is matched against that catalog on your
PC. On a PC without Windows Package Manager, Zest reads the same public catalog directly to tell
you what is out of date, and offers to install Windows Package Manager so it can do the updating.
More on the [Software](Software#software-updates) page.

### And drivers?

From Windows Update only, where drivers are signed and matched to your hardware by Microsoft.
Zest does not download drivers from anywhere else.

### Why does it want administrator rights?

Most of it does not. Tools that change things for every user of the PC (machine-wide startup
entries, system restore points, system images, some repair tools) need them, say so, and offer
*Restart as administrator*.

### Does it run in the background?

Only if you turn on *Options → Smart cleaning → Watch in the background* or a schedule.
Otherwise nothing of Zest runs when its window is closed.

### What does it send over the internet?

One request a day to GitHub asking for the newest Zest version (can be switched off), and catalog
lookups when you ask for software updates. No telemetry, no account. See
[Safety](Safety#what-zest-sends-over-the-network).

### Something was removed that I wanted to keep.

- Files removed by the duplicate finder, empty folders or the uninstaller's leftover sweep are in
  the Recycle Bin (unless you turned that off).
- Registry changes: *Registry → Backups → Restore*.
- Junk removed by a clean is deleted, not recycled. Please
  [tell us](https://github.com/SkippyNet/zest/issues/new/choose) what it was, so the rule can be
  fixed, and add the location under *Options → Include and exclude* so it is left alone.

### Can I use it from a script?

Yes: [Command line](Command-line).

### How do I remove Zest completely?

Uninstall it from *Settings → Apps*, and answer yes when asked whether to remove settings. For
the portable version, delete its folder. If you turned on a schedule or the background agent,
turn them off first (the uninstaller also removes both).
