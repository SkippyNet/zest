# Protect

## Backup

Two kinds of backup, on two tabs.

### File backup

Keeps versions of the folders you choose, in a destination you choose (another drive is the point
of a backup; Zest refuses a destination inside a folder being backed up).

1. Add the folders to back up and pick a destination.
2. **Back up now.** The first run copies everything. Later runs only read files whose size or
   date changed, so they are quick.
3. Each run adds a **snapshot** to the list: the state of your folders at that moment.

What you can do with a snapshot:

- **Browse** its folders and files as they were.
- **Restore** all of it, one folder, or single files, to the original place or to another folder.
  If a file already exists you choose: skip it (the default), keep both, or replace it.
- **Verify** re-reads the stored data and checks it against its checksums, so you know the backup
  is intact before you need it.
- **Delete** a snapshot, or keep only the last few automatically.

How it stores things: files are cut into blocks, each block is compressed and stored once under
the fingerprint of its contents. Ten snapshots of a folder that barely changed take little more
room than one, and identical files in different folders are stored once. The backup lives in a
`ZestBackup` folder at the destination.

Good to know:

- A file that another program holds open exclusively cannot be read. It is skipped and listed in
  the result, not silently missed.
- Files that exist only in the cloud are skipped (reading them would download them).
- Links and junctions are not followed.
- File contents, dates and the read-only, hidden and system attributes are kept. Permissions are
  not.
- Backups are not encrypted. Put them on a drive you control.

From the command line: `zest-cli backup create | list | restore | verify`
(see [Command line](Command-line)).

### System image

A complete image of the Windows drive, made by the imaging engine that is part of Windows, onto
another NTFS drive. An image can bring back the whole PC, Windows included, after a drive failure.

- Needs administrator rights and a second drive with enough room.
- Restoring is done from the Windows recovery environment (*Settings → System → Recovery →
  Advanced startup*, then *Troubleshoot → Advanced options → System Image Recovery*). The page
  shows these steps and opens the recovery settings for you.

This feature has been built and its command handling tested, but at the time of writing it has
not been run end to end by the developers. Make your first image when you do not need it yet, and
check that Windows' recovery environment can see it.

## System restore

Restore points are Windows' own snapshots of system files and settings (not your documents).

- The page lists existing restore points, creates a new one with a description you type, and
  removes old ones.
- If system protection is turned off for the Windows drive, the page says so; turn it on in
  *System Properties → System Protection*.
- Needs administrator rights.

Make a restore point before a big change: a driver update, a registry clean of machine-wide
entries, or a batch of privacy switches.
