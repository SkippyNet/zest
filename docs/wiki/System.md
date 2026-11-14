# System

## System info

What is inside the PC and how healthy it is. Loading takes about a second and only reads.

| Section | Shows |
|---|---|
| **System** | Maker and model, firmware version and date, Windows edition and build, install date, uptime, Secure Boot, TPM |
| **Processor** | Name, cores and threads, speed, cache |
| **Memory** | Total, each module with size and speed, free slots |
| **Graphics** | Each adapter with memory and driver version and date; displays with resolution and refresh rate |
| **Storage** | Each drive with model, type (NVMe, SATA, USB), size, firmware and **health**; where the drive reports them, temperature, wear, power-on hours and error counts; and the volumes on it |
| **Battery** | Design capacity, current full capacity, wear in percent, charge and power state |
| **Network** | Adapters with type and link speed |

At the top, an **attention list** points out what deserves a look: a drive that reports problems,
a worn battery, a nearly full system drive, a PC that has not been restarted for many days.

**Copy report** and **Save report** produce the same information as text. The report leaves out
serial numbers, network addresses, product keys and the names of the PC and its users, so it is
safe to paste into a support request.

On NVMe drives everything is readable with normal rights. For SATA and RAID drives Windows only
gives wear and error counters to administrators; the page says so and offers
*Restart as administrator*.

From the command line: `zest-cli system` (add `--json` for a machine-readable report).

## Windows tools

Windows ships with good repair tools that are awkward to find and to run. This page lists them
with a plain description, what each one changes, roughly how long it takes, and its live output.

| Tool | What it is |
|---|---|
| **System file check** (verify / repair) | `sfc`: compares Windows' system files with known-good copies and replaces damaged ones |
| **Windows image check / scan / repair** | `DISM`: checks and repairs the component store that system file check repairs from |
| **Component store size / cleanup** | Shows how much room old Windows components take and removes superseded ones |
| **Disk check** | `chkdsk /scan`: checks the file system of the Windows drive while it is in use |
| **Flush DNS cache** | Forgets cached name lookups; fixes "site not found" after a network change |
| **Reset Winsock / TCP/IP** | Resets the network stack to defaults; needs a restart |
| **Reset Store cache** | Clears the Microsoft Store's cache |

And one-click shortcuts to Disk Cleanup, Reliability Monitor, Resource Monitor, Event Viewer,
Services, System Configuration, Memory Diagnostic and Optimize Drives.

- Tools that change the system or need a restart ask first.
- Most repair tools need administrator rights.
- **Stop** ends a running tool. **Copy** copies its output, for example to attach to a support
  request.
- A usual order for "Windows is acting up": Windows image repair, then System file check
  (repair), then restart.
