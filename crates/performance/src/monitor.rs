//! Live sampling of processor, memory and per-app resource use.
//!
//! A [`Sampler`] is ticked by its owner (once a second is typical) and
//! returns a [`SystemSnapshot`]. One tick is a single process-list system
//! call plus a pass over the desktop's windows; handles are opened only the
//! first time a process is seen. Rates (processor share, I/O throughput) are
//! differences between two ticks, so the very first snapshot reports zero
//! for them.

use crate::classify::{
    self, AppKind, BackgroundImpact, Context, ProcStatic, ProtectionReason, SHELL_HOSTS,
};
use crate::sys::{self, ProcessLister};
use crate::windowing;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};
use zest_core::Result;

/// Where a program's best application window currently is.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum WindowState {
    /// No application window at all (tray tools, helpers, services).
    #[default]
    None,
    /// Has a window, but it is minimized or hidden (for example on another
    /// virtual desktop).
    Background,
    /// Has a window on screen that is not the active one.
    Visible,
    /// Owns the window the user is working in.
    Foreground,
}

/// Which account a process runs under.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProcessOwner {
    /// The user running this program.
    CurrentUser,
    /// The SYSTEM account.
    System,
    /// LOCAL SERVICE or NETWORK SERVICE.
    Service,
    /// A different user account.
    OtherUser,
    /// The process could not be inspected.
    #[default]
    Unknown,
}

/// Resource use of one process.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessUsage {
    /// Process id.
    pub pid: u32,
    /// Id of the process that started this one.
    pub parent_pid: u32,
    /// Executable file name, e.g. `chrome.exe`.
    pub name: String,
    /// Full path of the executable, when it could be read.
    pub exe_path: Option<PathBuf>,
    /// Terminal Services session (0 = services).
    pub session_id: u32,
    /// Creation time in Windows FILETIME ticks. Together with `pid` this
    /// identifies the process even after the pid has been reused.
    pub start_ticks: u64,
    /// Share of the whole PC's processor capacity, 0–100.
    pub cpu_percent: f32,
    /// Private working set in bytes – the "Memory" figure Task Manager shows.
    pub memory: u64,
    /// Total working set in bytes (includes shared pages).
    pub working_set: u64,
    /// Private committed bytes.
    pub private_bytes: u64,
    /// Bytes read plus written per second.
    pub io_bytes_per_sec: u64,
    /// Number of threads.
    pub threads: u32,
    /// Number of open handles.
    pub handles: u32,
    /// True when every thread is suspended (by Windows, this program or
    /// anything else).
    pub suspended: bool,
    /// State of the process's best application window.
    pub window: WindowState,
    /// Account the process runs under.
    pub owner: ProcessOwner,
    /// True when the process runs with administrator rights.
    pub elevated: bool,
}

impl ProcessUsage {
    /// When the process was started.
    pub fn start_time(&self) -> Option<SystemTime> {
        sys::ticks_to_system_time(self.start_ticks)
    }
}

/// One program: a main executable plus its helper processes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppUsage {
    /// Stable key: the lower-cased executable path, or the lower-cased
    /// executable name when the path is unknown.
    pub id: String,
    /// Friendly name from the executable's version information, falling
    /// back to the file name without extension.
    pub name: String,
    /// Executable file name of the main process, e.g. `Spotify.exe`.
    pub exe_name: String,
    /// Full path of the main executable, when known.
    pub exe_path: Option<PathBuf>,
    /// Publisher from the executable's version information.
    pub publisher: Option<String>,
    /// Every process that belongs to this program.
    pub processes: Vec<ProcessUsage>,
    /// Current share of the whole PC's processor capacity, 0–100.
    pub cpu_percent: f32,
    /// Smoothed processor share over the sampler's lifetime.
    pub avg_cpu_percent: f32,
    /// Private working set in bytes, summed over all processes.
    pub memory: u64,
    /// Total working set in bytes, summed (shared pages counted repeatedly).
    pub working_set: u64,
    /// Private committed bytes, summed.
    pub private_bytes: u64,
    /// Bytes read plus written per second, summed.
    pub io_bytes_per_sec: u64,
    /// Smoothed I/O throughput over the sampler's lifetime.
    pub avg_io_bytes_per_sec: u64,
    /// Thread count, summed.
    pub threads: u32,
    /// Handle count, summed.
    pub handles: u32,
    /// Best window state among the processes.
    pub window: WindowState,
    /// Session of the main process.
    pub session_id: u32,
    /// Start time of the oldest process.
    pub start_time: Option<SystemTime>,
    /// True when every process is suspended.
    pub suspended: bool,
    /// True when controlling this program needs administrator rights that
    /// the current process does not have.
    pub requires_admin: bool,
    /// Why the program must be left alone, if it must.
    pub protection: Option<ProtectionReason>,
    /// Broad role, for grouping in a user interface.
    pub kind: AppKind,
    /// Estimated cost of leaving the program running in the background.
    pub impact: BackgroundImpact,
    /// True for unprotected background programs with a noticeable cost –
    /// good candidates for being put to sleep.
    pub recommended: bool,
}

impl AppUsage {
    /// Ids of all processes of this program.
    pub fn pids(&self) -> Vec<u32> {
        self.processes.iter().map(|p| p.pid).collect()
    }

    /// Number of processes.
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    /// True when the user is working in this program right now.
    pub fn is_foreground(&self) -> bool {
        self.window == WindowState::Foreground
    }

    /// True when the program has any application window.
    pub fn has_window(&self) -> bool {
        self.window != WindowState::None
    }

    /// True when the program has no application window.
    pub fn is_background(&self) -> bool {
        self.window == WindowState::None
    }

    /// True when the program must not be throttled, suspended or ended.
    pub fn is_protected(&self) -> bool {
        classify::is_protected(self)
    }
}

/// Physical memory and commit charge, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryInfo {
    /// Installed (usable) RAM.
    pub total: u64,
    /// RAM that can be handed out immediately.
    pub available: u64,
    /// `total - available`.
    pub used: u64,
    /// RAM holding cached file data.
    pub cached: u64,
    /// Committed virtual memory.
    pub commit_total: u64,
    /// Maximum commit (RAM plus page files).
    pub commit_limit: u64,
    /// `used / total` as a percentage.
    pub used_percent: f32,
}

/// Everything measured in one tick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// Wall-clock time of the tick.
    pub taken_at: SystemTime,
    /// Time since the previous tick; zero for the first snapshot, whose
    /// rate fields are therefore all zero.
    pub interval: Duration,
    /// Processor use of the whole PC, 0–100.
    pub cpu_percent: f32,
    /// Processor use per logical processor (empty when disabled).
    pub per_core: Vec<f32>,
    /// Memory figures.
    pub memory: MemoryInfo,
    /// Time since Windows started.
    pub uptime: Duration,
    /// When Windows started.
    pub boot_time: SystemTime,
    /// Number of processes.
    pub process_count: u32,
    /// Number of threads.
    pub thread_count: u32,
    /// Number of handles.
    pub handle_count: u32,
    /// Programs, sorted by memory use (largest first).
    pub apps: Vec<AppUsage>,
    /// Process ids behind the foreground window.
    pub foreground_pids: Vec<u32>,
}

impl SystemSnapshot {
    /// Finds a program by its [`AppUsage::id`].
    pub fn app(&self, id: &str) -> Option<&AppUsage> {
        self.apps.iter().find(|a| a.id == id)
    }

    /// Finds the program a process belongs to.
    pub fn app_for_pid(&self, pid: u32) -> Option<&AppUsage> {
        self.apps
            .iter()
            .find(|a| a.processes.iter().any(|p| p.pid == pid))
    }

    /// Programs worth putting to sleep, highest impact first.
    pub fn recommended(&self) -> Vec<&AppUsage> {
        let mut apps: Vec<&AppUsage> = self.apps.iter().filter(|a| a.recommended).collect();
        apps.sort_by(|a, b| b.impact.cmp(&a.impact).then(b.memory.cmp(&a.memory)));
        apps
    }
}

/// Sampler configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SamplerOptions {
    /// Measure each logical processor separately (one extra system call).
    pub per_core: bool,
}

impl Default for SamplerOptions {
    fn default() -> Self {
        SamplerOptions { per_core: true }
    }
}

struct PrevProcess {
    create_time: u64,
    cpu_time: u64,
    io_bytes: u64,
}

struct PrevTick {
    at: Instant,
    system_idle: u64,
    system_total: u64,
    cores: Vec<(u64, u64)>,
    processes: HashMap<u32, PrevProcess>,
}

#[derive(Clone, Default)]
struct ExeMeta {
    description: Option<String>,
    company: Option<String>,
}

#[derive(Clone, Copy, Default)]
struct History {
    samples: u32,
    avg_cpu: f32,
    avg_io: f64,
}

/// Smoothing factor of the running averages (weight of the newest sample).
const SMOOTHING: f32 = 0.25;

/// Collects [`SystemSnapshot`]s.
pub struct Sampler {
    options: SamplerOptions,
    lister: ProcessLister,
    prev: Option<PrevTick>,
    statics: HashMap<(u32, u64), ProcStatic>,
    exe_meta: HashMap<String, ExeMeta>,
    history: HashMap<String, History>,
    elevated: bool,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    /// Creates a sampler with default options.
    pub fn new() -> Self {
        Self::with_options(SamplerOptions::default())
    }

    /// Creates a sampler with explicit options.
    pub fn with_options(options: SamplerOptions) -> Self {
        // Resolve the security product folders in the background so the
        // first ticks do not wait for it.
        crate::security::warm_up();
        Sampler {
            options,
            lister: ProcessLister::new(),
            prev: None,
            statics: HashMap::new(),
            exe_meta: HashMap::new(),
            history: HashMap::new(),
            elevated: zest_core::elevation::is_elevated(),
        }
    }

    /// Takes one snapshot.
    ///
    /// Fails only if the process list itself cannot be read; processes that
    /// cannot be inspected individually are still reported (as protected).
    pub fn sample(&mut self) -> Result<SystemSnapshot> {
        let now = Instant::now();
        let taken_at = SystemTime::now();
        let raw = self.lister.list()?;
        let (system_idle, system_total) = sys::system_times().unwrap_or((0, 0));
        let cores = if self.options.per_core {
            sys::processor_times()
        } else {
            Vec::new()
        };
        let windows = windowing::scan();
        let ctx = Context::capture(&raw, &windows.foreground_pids, false);

        let prev = self.prev.as_ref();
        let interval = prev.map_or(Duration::ZERO, |p| now.duration_since(p.at));
        let system_delta = prev.map_or(0, |p| system_total.saturating_sub(p.system_total));
        let idle_delta = prev.map_or(0, |p| system_idle.saturating_sub(p.system_idle));
        let cpu_percent = if system_delta > 0 {
            (100.0 * (1.0 - idle_delta as f64 / system_delta as f64)).clamp(0.0, 100.0) as f32
        } else {
            0.0
        };
        let per_core: Vec<f32> = match prev {
            Some(p) if p.cores.len() == cores.len() => cores
                .iter()
                .zip(&p.cores)
                .map(|((idle, total), (prev_idle, prev_total))| {
                    let total_delta = total.saturating_sub(*prev_total);
                    let idle_delta = idle.saturating_sub(*prev_idle);
                    if total_delta > 0 {
                        (100.0 * (1.0 - idle_delta as f64 / total_delta as f64)).clamp(0.0, 100.0)
                            as f32
                    } else {
                        0.0
                    }
                })
                .collect(),
            _ => vec![0.0; cores.len()],
        };

        // Per-process rows.
        let mut rows: Vec<Row> = Vec::with_capacity(raw.len());
        let mut thread_count = 0u32;
        let mut handle_count = 0u32;
        for process in &raw {
            thread_count = thread_count.saturating_add(process.threads);
            handle_count = handle_count.saturating_add(process.handles);
            if process.pid == 0 {
                // The idle pseudo process only accounts for unused time.
                continue;
            }
            let info = self
                .statics
                .entry((process.pid, process.create_time))
                .or_insert_with(|| classify::inspect_process(process.pid, process.create_time))
                .clone();
            let before = prev
                .and_then(|p| p.processes.get(&process.pid))
                .filter(|b| b.create_time == process.create_time);
            let cpu = match before {
                Some(b) if system_delta > 0 => (100.0
                    * process.cpu_time.saturating_sub(b.cpu_time) as f64
                    / system_delta as f64)
                    .clamp(0.0, 100.0) as f32,
                _ => 0.0,
            };
            let io = match before {
                Some(b) if !interval.is_zero() => {
                    (process.io_bytes.saturating_sub(b.io_bytes) as f64 / interval.as_secs_f64())
                        as u64
                }
                _ => 0,
            };
            let protection = classify::evaluate(process, &info, &ctx);
            let window = windows
                .states
                .get(&process.pid)
                .copied()
                .unwrap_or(WindowState::None);
            rows.push(Row {
                usage: ProcessUsage {
                    pid: process.pid,
                    parent_pid: process.parent_pid,
                    name: process.name.clone(),
                    exe_path: info.exe_path.clone(),
                    session_id: process.session_id,
                    start_ticks: process.create_time,
                    cpu_percent: cpu,
                    memory: process.private_working_set,
                    working_set: process.working_set,
                    private_bytes: process.private_bytes,
                    io_bytes_per_sec: io,
                    threads: process.threads,
                    handles: process.handles,
                    suspended: process.suspended,
                    window,
                    owner: info.owner,
                    elevated: info.elevated,
                },
                protection,
            });
        }

        let memory = memory_info();
        let apps = self.build_apps(rows, memory.total, !interval.is_zero());

        // Remember this tick and forget processes that are gone.
        let processes: HashMap<u32, PrevProcess> = raw
            .iter()
            .map(|p| {
                (
                    p.pid,
                    PrevProcess {
                        create_time: p.create_time,
                        cpu_time: p.cpu_time,
                        io_bytes: p.io_bytes,
                    },
                )
            })
            .collect();
        self.statics.retain(|(pid, created), _| {
            processes
                .get(pid)
                .is_some_and(|p| p.create_time == *created)
        });
        self.history
            .retain(|id, _| apps.iter().any(|a| &a.id == id));
        self.prev = Some(PrevTick {
            at: now,
            system_idle,
            system_total,
            cores,
            processes,
        });

        let uptime = uptime();
        Ok(SystemSnapshot {
            taken_at,
            interval,
            cpu_percent,
            per_core,
            memory,
            uptime,
            boot_time: taken_at
                .checked_sub(uptime)
                .unwrap_or(SystemTime::UNIX_EPOCH),
            process_count: raw.len() as u32,
            thread_count,
            handle_count,
            apps,
            foreground_pids: windows.foreground_pids,
        })
    }

    fn build_apps(&mut self, rows: Vec<Row>, total_memory: u64, has_rates: bool) -> Vec<AppUsage> {
        let lowered: Vec<(String, Option<String>)> = rows
            .iter()
            .map(|r| {
                (
                    r.usage.name.to_lowercase(),
                    r.usage
                        .exe_path
                        .as_ref()
                        .map(|p| p.to_string_lossy().to_lowercase()),
                )
            })
            .collect();
        let inputs: Vec<GroupInput<'_>> = rows
            .iter()
            .zip(&lowered)
            .map(|(r, (name, path))| GroupInput {
                pid: r.usage.pid,
                parent_pid: r.usage.parent_pid,
                create_time: r.usage.start_ticks,
                name,
                exe_path: path.as_deref(),
                has_window: r.usage.window != WindowState::None,
                independent_children: classify::is_system_process_name(name)
                    || SHELL_HOSTS.contains(&name.as_str()),
            })
            .collect();
        let roots = assign_roots(&inputs);

        // Merge by the identity of each root's executable.
        let mut order: Vec<String> = Vec::new();
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        let mut main_of: HashMap<String, usize> = HashMap::new();
        for (index, &root) in roots.iter().enumerate() {
            let (name, path) = &lowered[root];
            let key = path.clone().unwrap_or_else(|| name.clone());
            let members = groups.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            });
            members.push(index);
            main_of.entry(key).or_insert(root);
        }
        drop(inputs);

        let mut rows: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
        let mut apps = Vec::with_capacity(order.len());
        for key in order {
            let members = groups.remove(&key).unwrap_or_default();
            let main_index = main_of.get(&key).copied().unwrap_or(members[0]);
            let main_pid = rows[main_index].as_ref().map(|r| r.usage.pid);
            let mut processes = Vec::with_capacity(members.len());
            let mut protection: Option<ProtectionReason> = None;
            for index in members {
                let Some(row) = rows[index].take() else {
                    continue;
                };
                if let Some(reason) = row.protection
                    && protection.is_none_or(|p| !p.is_permanent() && reason.is_permanent())
                {
                    protection = Some(reason);
                }
                processes.push(row.usage);
            }
            let Some(main) = processes
                .iter()
                .find(|p| Some(p.pid) == main_pid)
                .or(processes.first())
                .cloned()
            else {
                continue;
            };

            let meta = self.exe_meta_for(main.exe_path.as_deref());
            let stem = Path::new(&main.name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| main.name.clone());
            let name = meta
                .description
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or(stem);

            let cpu_percent = processes
                .iter()
                .map(|p| p.cpu_percent)
                .sum::<f32>()
                .min(100.0);
            let io: u64 = processes.iter().map(|p| p.io_bytes_per_sec).sum();
            let history = self.history.entry(key.clone()).or_default();
            if has_rates {
                if history.samples == 0 {
                    history.avg_cpu = cpu_percent;
                    history.avg_io = io as f64;
                } else {
                    history.avg_cpu += (cpu_percent - history.avg_cpu) * SMOOTHING;
                    history.avg_io += (io as f64 - history.avg_io) * SMOOTHING as f64;
                }
                history.samples = history.samples.saturating_add(1);
            }
            let history = *history;

            let window = processes.iter().map(|p| p.window).max().unwrap_or_default();
            let memory: u64 = processes.iter().map(|p| p.memory).sum();
            let suspended = !processes.is_empty() && processes.iter().all(|p| p.suspended);
            let impact = classify::estimate_impact(
                history.avg_cpu,
                memory,
                history.avg_io as u64,
                total_memory,
            );
            let kind = match protection {
                Some(reason) if reason.is_permanent() => AppKind::System,
                _ if window != WindowState::None => AppKind::Windowed,
                _ => AppKind::Background,
            };
            let recommended = protection.is_none()
                && window == WindowState::None
                && !suspended
                && impact >= BackgroundImpact::Medium;

            apps.push(AppUsage {
                id: key,
                name,
                exe_name: main.name.clone(),
                exe_path: main.exe_path.clone(),
                publisher: meta.company.clone(),
                cpu_percent,
                avg_cpu_percent: history.avg_cpu,
                memory,
                working_set: processes.iter().map(|p| p.working_set).sum(),
                private_bytes: processes.iter().map(|p| p.private_bytes).sum(),
                io_bytes_per_sec: io,
                avg_io_bytes_per_sec: history.avg_io as u64,
                threads: processes.iter().map(|p| p.threads).sum(),
                handles: processes.iter().map(|p| p.handles).sum(),
                window,
                session_id: main.session_id,
                start_time: processes
                    .iter()
                    .map(|p| p.start_ticks)
                    .filter(|&t| t > 0)
                    .min()
                    .and_then(sys::ticks_to_system_time),
                suspended,
                requires_admin: !self.elevated && processes.iter().any(|p| p.elevated),
                protection,
                kind,
                impact,
                recommended,
                processes,
            });
        }
        apps.sort_by(|a, b| b.memory.cmp(&a.memory).then_with(|| a.id.cmp(&b.id)));
        apps
    }

    fn exe_meta_for(&mut self, path: Option<&Path>) -> ExeMeta {
        let Some(path) = path else {
            return ExeMeta::default();
        };
        let key = path.to_string_lossy().to_lowercase();
        if let Some(meta) = self.exe_meta.get(&key) {
            return meta.clone();
        }
        let meta = zest_core::fileinfo::version_info(path)
            .map(|info| ExeMeta {
                description: info.description.or(info.product),
                company: info.company,
            })
            .unwrap_or_default();
        // The cache is bounded by the number of distinct programs ever seen
        // in one session; cap it anyway so a pathological workload cannot
        // grow it without limit.
        if self.exe_meta.len() > 4096 {
            self.exe_meta.clear();
        }
        self.exe_meta.insert(key, meta.clone());
        meta
    }
}

struct Row {
    usage: ProcessUsage,
    protection: Option<ProtectionReason>,
}

/// What the grouping algorithm needs to know about one process.
/// Names and paths are lower-cased.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GroupInput<'a> {
    pub pid: u32,
    pub parent_pid: u32,
    pub create_time: u64,
    pub name: &'a str,
    pub exe_path: Option<&'a str>,
    pub has_window: bool,
    /// Children of this process are programs in their own right (shells,
    /// the desktop shell, service hosts).
    pub independent_children: bool,
}

/// Helper executables that always belong to whichever program started them.
const EMBEDDED_HELPERS: &[&str] = &[
    "msedgewebview2.exe",
    "crashpad_handler.exe",
    "cefsharp.browsersubprocess.exe",
    "qtwebengineprocess.exe",
];

/// True when `child` lives inside the install folder of `root` and that
/// folder is specific enough to stand for one program.
fn inside_install_dir(child: Option<&str>, root: Option<&str>) -> bool {
    let (Some(child), Some(root)) = (child, root) else {
        return false;
    };
    let Some(dir_end) = root.rfind('\\') else {
        return false;
    };
    let dir = &root[..dir_end];
    if dir.split('\\').filter(|c| !c.is_empty()).count() < 2 {
        return false;
    }
    let windows = zest_core::paths::windows_dir()
        .to_string_lossy()
        .to_lowercase();
    if dir == windows || dir.starts_with(&format!("{windows}\\")) {
        return false;
    }
    child.len() > dir.len() + 1 && child.starts_with(dir) && child.as_bytes()[dir.len()] == b'\\'
}

/// For every process, the index of the process that leads its group.
///
/// A process joins its parent's group when it is clearly a helper: the same
/// executable, a well-known embedded helper, or a window-less executable
/// from the parent program's install folder. Everything else – in
/// particular anything started by a shell or by Windows itself – leads its
/// own group.
pub(crate) fn assign_roots(processes: &[GroupInput<'_>]) -> Vec<usize> {
    const UNSET: usize = usize::MAX;
    let by_pid: HashMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.pid, i))
        .collect();
    let mut roots = vec![UNSET; processes.len()];

    fn resolve(
        index: usize,
        processes: &[GroupInput<'_>],
        by_pid: &HashMap<u32, usize>,
        roots: &mut Vec<usize>,
        depth: usize,
    ) -> usize {
        if roots[index] != UNSET {
            return roots[index];
        }
        // Provisional answer: breaks parent cycles caused by pid reuse.
        roots[index] = index;
        let process = &processes[index];
        let parent_index = by_pid
            .get(&process.parent_pid)
            .copied()
            .filter(|&pi| pi != index && process.parent_pid != 0)
            // A "parent" created after its child is a recycled pid.
            .filter(|&pi| processes[pi].create_time <= process.create_time);
        let root = match parent_index {
            Some(pi) if depth < 64 => {
                let parent = &processes[pi];
                let same_program = parent.name == process.name
                    && (parent.exe_path == process.exe_path
                        || parent.exe_path.is_none()
                        || process.exe_path.is_none());
                if same_program {
                    resolve(pi, processes, by_pid, roots, depth + 1)
                } else if parent.independent_children {
                    index
                } else if EMBEDDED_HELPERS.contains(&process.name) {
                    resolve(pi, processes, by_pid, roots, depth + 1)
                } else if process.has_window {
                    index
                } else {
                    let parent_root = resolve(pi, processes, by_pid, roots, depth + 1);
                    if inside_install_dir(process.exe_path, processes[parent_root].exe_path) {
                        parent_root
                    } else {
                        index
                    }
                }
            }
            _ => index,
        };
        roots[index] = root;
        root
    }

    for index in 0..processes.len() {
        resolve(index, processes, &by_pid, &mut roots, 0);
    }
    roots
}

fn memory_info() -> MemoryInfo {
    use windows::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
    let mut info = PERFORMANCE_INFORMATION {
        cb: size_of::<PERFORMANCE_INFORMATION>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is a correctly sized, initialised structure.
    if unsafe { GetPerformanceInfo(&mut info, info.cb) }.is_err() {
        return MemoryInfo::default();
    }
    let page = info.PageSize as u64;
    let total = info.PhysicalTotal as u64 * page;
    let available = info.PhysicalAvailable as u64 * page;
    let used = total.saturating_sub(available);
    MemoryInfo {
        total,
        available,
        used,
        cached: info.SystemCache as u64 * page,
        commit_total: info.CommitTotal as u64 * page,
        commit_limit: info.CommitLimit as u64 * page,
        used_percent: if total > 0 {
            (100.0 * used as f64 / total as f64) as f32
        } else {
            0.0
        },
    }
}

/// Time since Windows started.
pub(crate) fn uptime() -> Duration {
    // SAFETY: no arguments, cannot fail.
    Duration::from_millis(unsafe { windows::Win32::System::SystemInformation::GetTickCount64() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        pid: u32,
        parent: u32,
        created: u64,
        name: &'a str,
        path: Option<&'a str>,
    ) -> GroupInput<'a> {
        GroupInput {
            pid,
            parent_pid: parent,
            create_time: created,
            name,
            exe_path: path,
            has_window: false,
            independent_children: classify::is_system_process_name(name)
                || SHELL_HOSTS.contains(&name),
        }
    }

    #[test]
    fn same_executable_children_join_their_parent() {
        let browser = Some(r"c:\program files\vendor\browser\browser.exe");
        let procs = [
            input(10, 1, 100, "explorer.exe", Some(r"c:\windows\explorer.exe")),
            input(20, 10, 200, "browser.exe", browser),
            input(21, 20, 210, "browser.exe", browser),
            input(22, 21, 220, "browser.exe", browser),
        ];
        assert_eq!(assign_roots(&procs), vec![0, 1, 1, 1]);
    }

    #[test]
    fn helpers_from_the_install_folder_join_but_windowed_programs_do_not() {
        let mut game = input(
            32,
            30,
            320,
            "game.exe",
            Some(r"c:\program files\launcher\library\game\game.exe"),
        );
        game.has_window = true;
        let procs = [
            input(
                30,
                10,
                300,
                "launcher.exe",
                Some(r"c:\program files\launcher\launcher.exe"),
            ),
            input(
                31,
                30,
                310,
                "helper.exe",
                Some(r"c:\program files\launcher\bin\helper.exe"),
            ),
            game,
            // A window-less tool from somewhere else stays on its own.
            input(33, 30, 330, "tool.exe", Some(r"c:\other\place\tool.exe")),
            // A sibling folder that merely shares the name prefix.
            input(
                34,
                30,
                340,
                "x.exe",
                Some(r"c:\program files\launcher-extras\x.exe"),
            ),
            // A well-known embedded helper joins regardless of its location.
            input(
                35,
                30,
                350,
                "msedgewebview2.exe",
                Some(r"c:\program files (x86)\web\view\msedgewebview2.exe"),
            ),
            // ... and its own children follow it.
            input(
                36,
                35,
                360,
                "msedgewebview2.exe",
                Some(r"c:\program files (x86)\web\view\msedgewebview2.exe"),
            ),
        ];
        assert_eq!(assign_roots(&procs), vec![0, 0, 2, 3, 4, 0, 0]);
    }

    #[test]
    fn shells_and_system_hosts_do_not_swallow_their_children() {
        let procs = [
            input(10, 1, 100, "explorer.exe", Some(r"c:\windows\explorer.exe")),
            input(40, 10, 400, "cmd.exe", Some(r"c:\windows\system32\cmd.exe")),
            input(
                41,
                40,
                410,
                "ping.exe",
                Some(r"c:\windows\system32\ping.exe"),
            ),
            input(42, 40, 420, "cmd.exe", Some(r"c:\windows\system32\cmd.exe")),
            input(
                50,
                2,
                50,
                "svchost.exe",
                Some(r"c:\windows\system32\svchost.exe"),
            ),
            input(
                51,
                50,
                510,
                "widget.exe",
                Some(r"c:\windows\system32\widget.exe"),
            ),
            // System32 is never treated as one program's install folder.
            input(60, 10, 600, "mmc.exe", Some(r"c:\windows\system32\mmc.exe")),
            input(
                61,
                60,
                610,
                "other.exe",
                Some(r"c:\windows\system32\other.exe"),
            ),
        ];
        assert_eq!(assign_roots(&procs), vec![0, 1, 2, 1, 4, 5, 6, 7]);
    }

    #[test]
    fn recycled_parent_pids_and_cycles_are_ignored() {
        let app = Some(r"c:\apps\thing\thing.exe");
        let procs = [
            // "Parent" 70 was created after its supposed child 71.
            input(70, 1, 900, "thing.exe", app),
            input(71, 70, 800, "thing.exe", app),
            // A two-process cycle (possible after pid reuse).
            input(80, 81, 100, "a.exe", Some(r"c:\apps\a\a.exe")),
            input(81, 80, 100, "a.exe", Some(r"c:\apps\a\a.exe")),
            // Parent that no longer exists.
            input(90, 12345, 100, "orphan.exe", None),
        ];
        let roots = assign_roots(&procs);
        assert_eq!(roots[0], 0);
        assert_eq!(roots[1], 1);
        assert!(roots[2] == 2 || roots[2] == 3);
        assert_eq!(roots[2], roots[3]);
        assert_eq!(roots[4], 4);
    }

    #[test]
    fn install_folder_rules() {
        assert!(inside_install_dir(
            Some(r"c:\apps\thing\bin\helper.exe"),
            Some(r"c:\apps\thing\thing.exe")
        ));
        assert!(!inside_install_dir(
            Some(r"c:\apps\thing-two\helper.exe"),
            Some(r"c:\apps\thing\thing.exe")
        ));
        // A program sitting directly in a drive root owns no folder.
        assert!(!inside_install_dir(
            Some(r"c:\tools\x.exe"),
            Some(r"c:\run.exe")
        ));
        assert!(!inside_install_dir(None, Some(r"c:\apps\thing\thing.exe")));
        assert!(!inside_install_dir(Some(r"c:\apps\thing\x.exe"), None));
    }

    fn burn(duration: Duration) {
        let start = Instant::now();
        let mut x = 0u64;
        while start.elapsed() < duration {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            std::hint::black_box(x);
        }
    }

    #[test]
    fn first_snapshot_has_no_rates_and_second_has() {
        let mut sampler = Sampler::new();
        let first = sampler.sample().unwrap();
        assert!(first.interval.is_zero());
        assert_eq!(first.cpu_percent, 0.0);
        assert!(
            first
                .apps
                .iter()
                .all(|a| a.cpu_percent == 0.0 && a.io_bytes_per_sec == 0)
        );

        burn(Duration::from_millis(300));
        let second = sampler.sample().unwrap();
        assert!(second.interval >= Duration::from_millis(250));
        assert!(second.cpu_percent > 0.0 && second.cpu_percent <= 100.0);
        assert_eq!(second.per_core.len(), first.per_core.len());
        assert!(!second.per_core.is_empty());

        // One busy thread is 100/N percent of an N-processor PC. Allow a wide
        // margin for scheduling noise, but the scale must be right. On a PC
        // that is busy with other work one 300 ms window can be starved of
        // processor time, so up to two more windows are measured.
        let one_core = 100.0 / second.per_core.len() as f32;
        let mut busy = second;
        for _ in 0..2 {
            let measured = busy
                .app_for_pid(std::process::id())
                .map_or(0.0, |app| app.cpu_percent);
            if measured >= one_core * 0.3 {
                break;
            }
            burn(Duration::from_millis(300));
            busy = sampler.sample().unwrap();
        }
        let me = busy
            .app_for_pid(std::process::id())
            .expect("own process must be part of an app");
        assert!(
            me.cpu_percent >= one_core * 0.3,
            "busy loop measured as {:.2}% (one core = {one_core:.2}%)",
            me.cpu_percent
        );
        assert!(me.cpu_percent <= 100.0);
        assert!(me.memory > 0 && me.working_set >= me.memory);
        assert_eq!(me.protection, Some(ProtectionReason::ThisProgram));
        assert!(me.is_protected());
        assert!(!me.recommended);
        assert_eq!(me.kind, AppKind::System);
    }

    #[test]
    fn snapshot_is_internally_consistent() {
        let mut sampler = Sampler::new();
        let snapshot = sampler.sample().unwrap();
        // Every process appears in exactly one app.
        let mut seen = std::collections::HashSet::new();
        for app in &snapshot.apps {
            assert!(!app.processes.is_empty(), "{} has no processes", app.id);
            assert!(!app.name.is_empty());
            assert_eq!(app.id, app.id.to_lowercase());
            for process in &app.processes {
                assert!(seen.insert(process.pid), "pid {} listed twice", process.pid);
            }
            assert_eq!(
                app.memory,
                app.processes.iter().map(|p| p.memory).sum::<u64>()
            );
            if app.recommended {
                assert!(!app.is_protected());
                assert!(app.is_background());
            }
        }
        // The idle pseudo process is the only one left out.
        assert_eq!(seen.len() as u32 + 1, snapshot.process_count);
        assert!(!seen.contains(&0));

        // Core Windows components are present and protected.
        let system = snapshot.app_for_pid(4).expect("System process");
        assert_eq!(system.protection, Some(ProtectionReason::SystemProcess));
        for app in &snapshot.apps {
            if classify::is_system_process_name(&app.exe_name) {
                assert!(
                    app.protection.is_some_and(|r| r.is_permanent()),
                    "{}",
                    app.exe_name
                );
                assert_eq!(app.kind, AppKind::System);
            }
            if app.processes.iter().any(|p| p.session_id == 0) {
                assert!(app.is_protected(), "{} runs in session 0", app.exe_name);
            }
        }

        // Memory figures are plausible.
        let m = snapshot.memory;
        assert!(m.total > 512 * 1024 * 1024);
        assert!(m.available <= m.total);
        assert_eq!(m.used, m.total - m.available);
        assert!(m.commit_limit >= m.commit_total);
        assert!(m.used_percent > 0.0 && m.used_percent <= 100.0);
        assert!(snapshot.uptime > Duration::from_secs(1));
        assert!(snapshot.boot_time < snapshot.taken_at);
        assert!(snapshot.thread_count >= snapshot.process_count);
    }

    #[test]
    fn ticks_stay_cheap_after_the_first() {
        let mut sampler = Sampler::new();
        sampler.sample().unwrap();
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let start = Instant::now();
            sampler.sample().unwrap();
            best = best.min(start.elapsed());
        }
        // Generous bound: a warm tick is a few milliseconds on real hardware.
        assert!(best < Duration::from_millis(250), "warm tick took {best:?}");
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let mut sampler = Sampler::with_options(SamplerOptions { per_core: false });
        let snapshot = sampler.sample().unwrap();
        assert!(snapshot.per_core.is_empty());
        let json = serde_json::to_string(&snapshot).unwrap();
        let back: SystemSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.apps.len(), snapshot.apps.len());
        assert_eq!(back.memory, snapshot.memory);
    }
}
