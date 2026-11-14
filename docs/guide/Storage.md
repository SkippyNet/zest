# Storage

## Disk analyzer

Pick a drive or **Choose folder**, then **Analyze**. Reading a whole drive takes seconds on an
SSD. Three views of the result:

- **Folders**: a tree sorted by size. Open folders to follow the space.
- **Largest files**: the biggest files, wherever they are.
- **File types**: totals by kind (video, pictures, archives, ...) and by extension.

The analyzer only reads. Links and junctions are counted as entries but never followed, so
nothing is counted twice, and cloud-only files are counted without being downloaded.

## Duplicate finder

Choose one or more folders and press **Scan**. Files are matched by content, not by name: first
by size, then by a quick sample, then by a full hash of every candidate, so two files are only
called duplicates when they are identical byte for byte.

- Each set shows every copy. Zest can pre-select all but one per set (newest, oldest, or the one
  in a folder you prefer).
- At least one copy of every set is always kept: **Delete selected** refuses a selection that
  would remove them all, and checks again right before deleting that the copy it keeps is still
  there and still identical.
- Removed copies go to the Recycle Bin unless you changed *Options → General*.
- System folders and cloud-only files are skipped.

## Empty folders

Finds folders that contain nothing, or only other empty folders, under a folder you choose. Review
the list, tick what should go, remove. Like duplicates, they go to the Recycle Bin by default.

Some programs expect their empty folders to exist. Folders inside Windows and program folders are
not offered.

## Drive wiper

Three tools for making deleted data unrecoverable. All three are irreversible and say so before
they start.

| Tool | What it does |
|---|---|
| **Wipe free space** | Overwrites the unused part of a drive, so files deleted earlier cannot be recovered. Your existing files are not touched. |
| **Shred** | Overwrites and deletes the files and folders you add. |
| **Erase everything** | Removes everything on a drive and overwrites it. Not available for the drive Windows runs from. |

Choose 1, 3, 7 or 35 passes. One pass is enough for a hard disk.

**About SSDs:** solid-state drives, USB sticks and memory cards move data around internally, so
overwriting cannot guarantee that every old copy is gone, and extra passes add nothing. Zest tells
you when the selected drive is an SSD. If you need that guarantee, use device encryption
(BitLocker) or the drive maker's secure-erase tool.
