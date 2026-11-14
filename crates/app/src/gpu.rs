//! Graphics set-up: which graphics interface draws the window.
//!
//! The window is drawn through `wgpu`, which can sit on OpenGL or on
//! Direct3D 12. Both draw the same picture (the screenshots are identical
//! to within one colour step), but they cost very different amounts:
//!
//! * **OpenGL** loads only the driver of the chip that actually draws. On
//!   the development laptop that meant 72 MB of private memory and a first
//!   frame about 130 ms after start.
//! * **Direct3D 12** has to look at every graphics adapter in the machine
//!   before it can pick one, which loads all of their drivers (and wakes a
//!   sleeping gaming GPU): 176 MB and about 300 ms on the same laptop. In
//!   exchange it works everywhere, including PCs with no graphics driver at
//!   all, where it renders in software.
//!
//! So OpenGL is tried first and Direct3D is the safety net:
//!
//! 1. Before OpenGL is initialised a note is written to the data folder.
//!    It is replaced by "works" once frames have been drawn.
//! 2. If OpenGL reports an error, the app notes "use Direct3D" and starts
//!    itself again; the user sees one slightly slower start.
//! 3. If a start finds the note from step 1 still there, the previous
//!    attempt died half-way (a crashing driver). It uses Direct3D from then
//!    on and tries OpenGL again only after the app has been updated.
//!
//! `--graphics opengl|direct3d|software` (or the `ZEST_GRAPHICS` variable)
//! overrides all of this for troubleshooting.

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use eframe::wgpu;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use zest_engine::core::{brand, paths};

/// Name of the note in the data folder.
const STATE_FILE: &str = "graphics.state";
/// Environment variable that forces a backend.
pub const OVERRIDE_ENV: &str = "ZEST_GRAPHICS";
/// Command line option that forces a backend.
pub const OVERRIDE_ARG: &str = "--graphics";
/// Frames that must have been drawn before OpenGL counts as working.
const FRAMES_TO_CONFIRM: u32 = 3;

/// The graphics interface a launch uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// OpenGL through the graphics driver.
    OpenGl,
    /// Direct3D 12 on the most economical adapter (software if there is none).
    Direct3d,
    /// Direct3D 12 on the software adapter, whatever hardware exists.
    Software,
}

impl Backend {
    /// Name used in logs, notes and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Backend::OpenGl => "opengl",
            Backend::Direct3d => "direct3d",
            Backend::Software => "software",
        }
    }

    fn parse(text: &str) -> Option<Backend> {
        match text.trim().to_ascii_lowercase().as_str() {
            "opengl" | "gl" => Some(Backend::OpenGl),
            "direct3d" | "d3d" | "d3d12" | "dx12" => Some(Backend::Direct3d),
            "software" | "warp" => Some(Backend::Software),
            _ => None,
        }
    }
}

/// What the note in the data folder says.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Note {
    /// No note, or one that cannot be read.
    None,
    /// OpenGL drew frames the last time.
    OpenGlWorks,
    /// A launch started OpenGL and has not drawn a frame (yet).
    OpenGlTrying,
    /// OpenGL failed for this version of the app.
    UseDirect3d { version: String },
}

impl Note {
    fn parse(text: &str) -> Note {
        let mut words = text.split_whitespace();
        match (words.next(), words.next()) {
            (Some("opengl"), Some("works")) => Note::OpenGlWorks,
            (Some("opengl"), Some("trying")) => Note::OpenGlTrying,
            (Some("direct3d"), Some(version)) => Note::UseDirect3d {
                version: version.to_owned(),
            },
            _ => Note::None,
        }
    }

    fn text(&self) -> String {
        match self {
            Note::None => String::new(),
            Note::OpenGlWorks => "opengl works".to_owned(),
            Note::OpenGlTrying => "opengl trying".to_owned(),
            Note::UseDirect3d { version } => format!("direct3d {version}"),
        }
    }
}

/// The decision for one launch, before anything is initialised.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Decision {
    backend: Backend,
    /// OpenGL is on trial: an error falls back to Direct3D.
    trial: bool,
    /// Note to leave before starting, if any.
    write_first: Option<Note>,
}

/// Pure decision logic: what to use given an override, the note left by
/// earlier launches and this build's version.
fn decide(forced: Option<Backend>, note: &Note, version: &str, may_write: bool) -> Decision {
    if let Some(backend) = forced {
        return Decision {
            backend,
            trial: false,
            write_first: None,
        };
    }
    let try_opengl = Decision {
        backend: Backend::OpenGl,
        trial: true,
        write_first: may_write.then_some(Note::OpenGlTrying),
    };
    let use_direct3d = |write: bool| Decision {
        backend: Backend::Direct3d,
        trial: false,
        write_first: (write && may_write).then(|| Note::UseDirect3d {
            version: version.to_owned(),
        }),
    };
    match note {
        Note::None | Note::OpenGlWorks => try_opengl,
        // The launch that left this note never drew a frame.
        Note::OpenGlTrying => use_direct3d(true),
        Note::UseDirect3d { version: failed } if failed == version => use_direct3d(false),
        // A different version of the app: give OpenGL another chance.
        Note::UseDirect3d { .. } => try_opengl,
    }
}

/// The graphics plan of this launch.
#[derive(Clone, Debug)]
pub struct Plan {
    backend: Backend,
    trial: bool,
    /// Where the note lives; `None` when this launch must not write one
    /// (screenshot runs, forced backends).
    note_file: Option<PathBuf>,
}

/// Set once frames have been drawn.
static CONFIRMED: AtomicBool = AtomicBool::new(false);
static FRAMES: AtomicU32 = AtomicU32::new(0);
static STARTED: OnceLock<Instant> = OnceLock::new();
static ACTIVE: OnceLock<Plan> = OnceLock::new();

/// Reads the forced backend from the command line or the environment.
fn forced_backend(args: &[String]) -> Option<Backend> {
    let from_args = args
        .iter()
        .position(|a| a == OVERRIDE_ARG)
        .and_then(|i| args.get(i + 1))
        .and_then(|value| Backend::parse(value));
    from_args.or_else(|| {
        std::env::var(OVERRIDE_ENV)
            .ok()
            .and_then(|value| Backend::parse(&value))
    })
}

impl Plan {
    /// Decides how this launch draws. `persist` is false for runs that must
    /// leave no trace (screenshots).
    pub fn for_launch(args: &[String], persist: bool) -> Plan {
        STARTED.get_or_init(Instant::now);
        let forced = forced_backend(args);
        let file = paths::data_dir().join(STATE_FILE);
        let note = std::fs::read_to_string(&file)
            .map(|text| Note::parse(&text))
            .unwrap_or(Note::None);
        let may_write = persist && forced.is_none();
        let decision = decide(forced, &note, brand::VERSION, may_write);
        let plan = Plan {
            backend: decision.backend,
            trial: decision.trial,
            note_file: may_write.then_some(file),
        };
        if let Some(first) = decision.write_first {
            plan.write(&first);
        }
        if note == Note::OpenGlTrying && forced.is_none() {
            log::warn!(
                "the previous start did not get as far as drawing with OpenGL; using Direct3D"
            );
        }
        let _ = ACTIVE.set(plan.clone());
        plan
    }

    /// The backend this launch uses.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    fn write(&self, note: &Note) {
        let Some(file) = &self.note_file else {
            return;
        };
        if let Some(dir) = file.parent() {
            let _ = paths::ensure_dir(dir);
        }
        if let Err(e) = std::fs::write(file, note.text()) {
            log::warn!("could not write {}: {e}", file.display());
        }
    }

    /// Whether a failed start should be retried with Direct3D: OpenGL was
    /// on trial and never drew a frame.
    pub fn should_fall_back(&self) -> bool {
        self.trial && self.backend == Backend::OpenGl && !CONFIRMED.load(Ordering::Relaxed)
    }

    /// Records that OpenGL does not work with this version of the app.
    pub fn note_opengl_failed(&self) {
        // Screenshot runs fall back too, but leave no note.
        self.write(&Note::UseDirect3d {
            version: brand::VERSION.to_owned(),
        });
    }

    /// The renderer configuration for this plan.
    pub fn wgpu_options(&self) -> WgpuConfiguration {
        let mut setup = WgpuSetupCreateNew::without_display_handle();
        setup.instance_descriptor.backends = match self.backend {
            Backend::OpenGl => wgpu::Backends::GL,
            Backend::Direct3d | Backend::Software => wgpu::Backends::DX12,
        };
        // An interface like this one needs no gaming GPU: prefer the
        // integrated chip and let the other one sleep. Without any
        // hardware adapter, wgpu picks the software one by itself.
        setup.power_preference = wgpu::PowerPreference::LowPower;
        if self.backend == Backend::Software {
            setup.native_adapter_selector = Some(Arc::new(|adapters, _surface| {
                adapters
                    .iter()
                    .find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
                    .cloned()
                    .ok_or_else(|| "this PC offers no software graphics adapter".to_owned())
            }));
        }
        setup.device_descriptor = Arc::new(|adapter| {
            let base_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
                wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                wgpu::Limits::default()
            };
            wgpu::DeviceDescriptor {
                label: Some("zest"),
                required_limits: wgpu::Limits {
                    // Room for the glyph atlas on high-resolution displays,
                    // but never more than the adapter has.
                    max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d.min(8192),
                    ..base_limits
                },
                // The default reserves memory in blocks of hundreds of
                // megabytes for workloads this interface never has.
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            }
        });
        WgpuConfiguration {
            wgpu_setup: WgpuSetup::CreateNew(setup),
            ..Default::default()
        }
    }
}

/// Marks the start of the process for the start-up time in the log.
pub fn mark_start() {
    STARTED.get_or_init(Instant::now);
}

/// Called once per drawn frame. Logs the start-up time on the first frame
/// and confirms a working OpenGL after a few.
pub fn frame_drawn() {
    if CONFIRMED.load(Ordering::Relaxed) {
        return;
    }
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let plan = ACTIVE.get();
    if frame == 1 {
        let elapsed = STARTED.get().map_or(0, |at| at.elapsed().as_millis());
        log::info!(
            "first frame after {elapsed} ms ({})",
            plan.map_or("default", |p| p.backend.name())
        );
    }
    if frame >= FRAMES_TO_CONFIRM {
        CONFIRMED.store(true, Ordering::Relaxed);
        if let Some(plan) = plan
            && plan.trial
        {
            plan.write(&Note::OpenGlWorks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "1.2.3";

    #[test]
    fn a_fresh_install_tries_opengl_and_leaves_a_note_first() {
        let decision = decide(None, &Note::None, V, true);
        assert_eq!(decision.backend, Backend::OpenGl);
        assert!(decision.trial);
        assert_eq!(decision.write_first, Some(Note::OpenGlTrying));
    }

    #[test]
    fn working_opengl_is_still_guarded_on_every_start() {
        // A driver update can break what worked yesterday.
        let decision = decide(None, &Note::OpenGlWorks, V, true);
        assert_eq!(decision.backend, Backend::OpenGl);
        assert_eq!(decision.write_first, Some(Note::OpenGlTrying));
    }

    #[test]
    fn an_unfinished_attempt_switches_to_direct3d_for_this_version() {
        let decision = decide(None, &Note::OpenGlTrying, V, true);
        assert_eq!(decision.backend, Backend::Direct3d);
        assert!(!decision.trial);
        assert_eq!(
            decision.write_first,
            Some(Note::UseDirect3d {
                version: V.to_owned()
            })
        );
        // ... and the note it leaves keeps it there.
        let again = decide(None, &decision.write_first.unwrap(), V, true);
        assert_eq!(again.backend, Backend::Direct3d);
        assert_eq!(again.write_first, None);
    }

    #[test]
    fn an_update_gives_opengl_another_chance() {
        let note = Note::UseDirect3d {
            version: "1.2.2".to_owned(),
        };
        let decision = decide(None, &note, V, true);
        assert_eq!(decision.backend, Backend::OpenGl);
        assert!(decision.trial);
    }

    #[test]
    fn forced_backends_ignore_and_keep_the_note() {
        for backend in [Backend::OpenGl, Backend::Direct3d, Backend::Software] {
            let decision = decide(Some(backend), &Note::OpenGlTrying, V, true);
            assert_eq!(decision.backend, backend);
            assert!(!decision.trial);
            assert_eq!(decision.write_first, None);
        }
    }

    #[test]
    fn runs_that_must_leave_no_trace_never_write() {
        for note in [
            Note::None,
            Note::OpenGlWorks,
            Note::OpenGlTrying,
            Note::UseDirect3d {
                version: V.to_owned(),
            },
        ] {
            assert_eq!(decide(None, &note, V, false).write_first, None, "{note:?}");
        }
        // They still stay away from an OpenGL that may be crashing.
        assert_eq!(
            decide(None, &Note::OpenGlTrying, V, false).backend,
            Backend::Direct3d
        );
    }

    #[test]
    fn notes_round_trip_and_garbage_reads_as_no_note() {
        for note in [
            Note::OpenGlWorks,
            Note::OpenGlTrying,
            Note::UseDirect3d {
                version: V.to_owned(),
            },
        ] {
            assert_eq!(Note::parse(&note.text()), note);
        }
        assert_eq!(Note::parse(""), Note::None);
        assert_eq!(Note::parse("opengl"), Note::None);
        assert_eq!(Note::parse("\u{0}\u{0}\u{0}"), Note::None);
        assert_eq!(Note::parse("something else entirely"), Note::None);
    }

    #[test]
    fn backend_names_parse_back() {
        for backend in [Backend::OpenGl, Backend::Direct3d, Backend::Software] {
            assert_eq!(Backend::parse(backend.name()), Some(backend));
        }
        assert_eq!(Backend::parse("D3D12"), Some(Backend::Direct3d));
        assert_eq!(Backend::parse("warp"), Some(Backend::Software));
        assert_eq!(Backend::parse("vulkan"), None);
    }

    #[test]
    fn the_override_is_read_from_the_command_line() {
        let args: Vec<String> = ["--page", "clean", "--graphics", "software"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        assert_eq!(forced_backend(&args), Some(Backend::Software));
        let dangling = vec!["--graphics".to_owned()];
        // No value: no override from the command line (the environment may
        // still set one, which this test does not touch).
        if std::env::var(OVERRIDE_ENV).is_err() {
            assert_eq!(forced_backend(&dangling), None);
        }
    }
}
