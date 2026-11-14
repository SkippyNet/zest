# Software

## Uninstall

Every installed program, from the same places Windows' own list uses, plus Microsoft Store apps.

- **Uninstall** runs the program's own uninstaller. When it has finished, Zest looks for what it
  left behind: folders in Program Files and AppData, and registry keys. You see the list and
  choose **Remove selected** or **Keep everything**. Removed leftovers go to the Recycle Bin, and
  registry keys are backed up first.
- **Repair** starts the program's repair or modify option where it has one.
- A program whose uninstaller is missing can be removed from the list.

## Software updates

**Check for updates** lists installed programs that have a newer version, with the installed and
the available version. Tick what you want and press **Update selected**.

### How does it know what to update?

Zest carries no list of software. It asks **Windows Package Manager** (winget), which is part of
Windows 10 and 11, to compare whatever is installed on this PC with the public package catalog:
about 16,000 programs, maintained in the open by Microsoft and the publishers and refreshed
several times a day. Updates are downloaded from each publisher's own address and checked against
the hash in the catalog before anything runs. Because the matching happens on the PC, it works the
same on a machine with completely different software.

### When Windows Package Manager is missing or broken

Some PCs do not have it (older Windows 10 builds, Windows Server, trimmed-down installs), and
sometimes it is switched off by policy or its sources cannot be reached. Zest does not show an
empty list and call that "up to date". It tells you which of these is the case and what to do,
for example a button that opens the Microsoft Store page for *App Installer*.

In the meantime it downloads the same public catalog itself (about 4 MB, kept for 12 hours) and
matches it against your installed programs, so you still see what is out of date. That list is
for information: installing the updates needs Windows Package Manager. You can switch the catalog
download off under *Options → About*.

### Ignoring a program

**Ignore** hides a program's updates. Undo it under *Options → General*.

## Drivers

- **Installed drivers** lists third-party driver packages with provider, version and date.
- **Check Windows Update** asks Windows Update for drivers that match your hardware.
  **Install selected** installs them through Windows Update's own installer.
- Driver backup exports your current third-party drivers to a folder, so you can put one back.

Zest does not download drivers from anywhere but Windows Update. Drivers there are signed and
matched to your exact hardware by Microsoft; "driver updater" sites that offer newer-looking
versions are a common source of broken PCs.

Installing drivers needs administrator rights.

## Browser extensions

Every extension in every profile of every browser Zest recognises, with a rating of what it is
allowed to access (all sites, some sites, nothing sensitive). Use it to spot extensions you do not
remember installing. **Remove** deletes an extension from its profile; the browser has to be
closed.
