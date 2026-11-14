Zest - portable edition
=======================

Start zest.exe. Nothing is installed.

Because the file "zest.portable" sits beside the program, Zest keeps its
settings, history and registry backups in a "data" folder right here instead
of in your Windows profile. Copy the whole folder to a USB stick and it works
the same on another PC. Delete "zest.portable" to use your profile instead.

zest-cli.exe is the command line version:

    zest-cli --help
    zest-cli analyze          what could be cleaned (changes nothing)
    zest-cli clean --dry-run  what a clean would remove (changes nothing)

Website:   https://skippynet.github.io/zest/
Source:    https://github.com/SkippyNet/zest
Problems:  https://github.com/SkippyNet/zest/issues

Zest is free software under the Apache License 2.0. See LICENSE and NOTICE.
