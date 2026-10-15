//! Capability detection (A§3 `Capabilities`, A§8.10): the ffmpeg version,
//! resolved path (its identity, part of export cache keys) and encoders;
//! the effective VA render node and its vendor; the h264_vaapi export
//! check, run as a child process with a 10 s timeout (a hung GPU driver
//! can't hang Qrec); optionally the `va*h264enc` factory bound to the node
//! ([`CapsOptions::gst_factory`], e.g. [`gst_factory_probe`] for hosts that
//! record). Cached in `$XDG_CACHE_HOME/qrec/capabilities.json`, keyed by
//! the ffmpeg binary, the node and the libva environment. The session
//! sends the result as `Event::Capabilities` before its first dispatch.
//! Owner: session.
//!
//! The VA node is never chosen by VA-API itself (A§2.1, the capture
//! owner's `gpu::effective_va_device`): the project's
//! `recording.vaapiDevice`, else the config's `encoder.vaapi_device`, else
//! the first Intel or AMD render node. NVIDIA is used only when configured
//! explicitly (its `renderD128` hangs the dev machine).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use qrec_core::state::{Capabilities, FfmpegCaps, VaapiCaps};
use serde::{Deserialize, Serialize};

use crate::gpu;
use crate::proc::{self, Limit, PROBE_TIMEOUT, Spec};
use crate::store::fsx;

/// Finds the `va*h264enc` factory bound to a render node.
pub type FactoryProbe = fn(&str) -> Option<String>;

/// Where and how to detect.
#[derive(Debug, Clone)]
pub struct CapsOptions {
    /// The ffmpeg to detect (looked up on `PATH` like the media runner does).
    pub ffmpeg: OsString,
    /// The ffprobe the media runner runs (looked up on `PATH` the same way).
    pub ffprobe: OsString,
    /// Where render nodes are listed (`/sys/class/drm`)…
    pub sys_class_drm: PathBuf,
    /// …and where their device files are (`/dev/dri`).
    pub dev_dri: PathBuf,
    /// Run the h264_vaapi export check (else `ffmpeg_h264` is false).
    pub probe_va: bool,
    /// `capabilities.json`; `None` disables the cache.
    pub cache_file: Option<PathBuf>,
    /// Where the probes' stderr goes.
    pub log_dir: Option<PathBuf>,
    /// Fills `VaapiCaps::gst_h264_factory` (hosts that record; see
    /// [`gst_factory_probe`]). `None` leaves it unknown (capture plans
    /// then use x264).
    pub gst_factory: Option<FactoryProbe>,
}

impl CapsOptions {
    /// The machine's ffmpeg and render nodes, cached in `cache_file`.
    pub fn system(cache_file: Option<PathBuf>, log_dir: Option<PathBuf>) -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            sys_class_drm: "/sys/class/drm".into(),
            dev_dri: "/dev/dri".into(),
            probe_va: true,
            cache_file,
            log_dir,
            gst_factory: None,
        }
    }
}

/// The devices configured for VA-API (A§2.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VaConfig {
    /// `recording.vaapiDevice` of the project.
    pub project: Option<String>,
    /// `encoder.vaapi_device` of the user config.
    pub config: Option<String>,
}

/// What detection found, and what it noticed on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub caps: Capabilities,
    pub notes: Vec<String>,
    /// The ffmpeg and VA checks came from the cache.
    pub cached: bool,
}

/// `VaapiCaps::gst_h264_factory` through the capture owner's probe child
/// (`gpu::run_probe_child`: this executable run again with
/// `QREC_GPU_PROBE`, 10 s timeout), so GStreamer's plugin scan, which
/// touches the GPU, never runs in the host itself. Only for a `qrec` whose
/// `main` calls `gpu::run_probe_if_requested` first.
pub fn gst_factory_probe(device: &str) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let probe = gpu::Probe::VaH264Factory {
        device: device.to_owned(),
    };
    match gpu::run_probe_child(&exe, &probe, None, gpu::PROBE_TIMEOUT) {
        Ok(answer) => {
            if let Some(error) = &answer.error {
                tracing::warn!("the VA encoder probe for {device}: {error}");
            }
            answer.factory
        }
        Err(e) => {
            tracing::warn!("the VA encoder probe for {device} failed: {e}");
            None
        }
    }
}

/// Detects the capabilities (from the cache when nothing changed). ffprobe
/// is only looked up on `PATH` (nothing runs), so it is never cached.
pub fn detect(opts: &CapsOptions, va: &VaConfig) -> Detected {
    let cores = std::thread::available_parallelism()
        .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX));
    let mut notes = Vec::new();
    let ffprobe = resolve_program(&opts.ffprobe).map(|p| p.to_string_lossy().into_owned());
    if ffprobe.is_none() {
        notes.push(format!(
            "{} is not installed (or not on PATH): exports and take probes won't work",
            opts.ffprobe.to_string_lossy()
        ));
    }
    let Some(ffmpeg) = resolve_program(&opts.ffmpeg) else {
        notes.push(format!(
            "{} is not installed (or not on PATH): exports, frames and autocut won't work",
            opts.ffmpeg.to_string_lossy()
        ));
        return Detected {
            caps: Capabilities {
                ffmpeg: None,
                ffprobe,
                vaapi: None,
                cores,
            },
            notes,
            cached: false,
        };
    };
    let nodes = gpu::scan_render_nodes(&opts.sys_class_drm, &opts.dev_dri);
    let choice = gpu::effective_va_device(va.project.as_deref(), va.config.as_deref(), &nodes)
        .map(|choice| (choice.device, choice.vendor));
    if let Some((device, None)) = &choice {
        notes.push(format!(
            "{device} isn't a render node this machine lists; VA-API is off"
        ));
    }
    let key = CacheKey {
        ffmpeg: ffmpeg.to_string_lossy().into_owned(),
        ffmpeg_len: fs::metadata(&ffmpeg).map_or(0, |m| m.len()),
        ffmpeg_mtime_ns: fs::metadata(&ffmpeg)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos()),
        va_device: choice.as_ref().map(|(d, _)| d.clone()),
        va_vendor: choice.as_ref().and_then(|(_, v)| *v),
        probed_va: opts.probe_va,
        libva_driver: std::env::var("LIBVA_DRIVER_NAME").ok(),
        libva_drivers_path: std::env::var("LIBVA_DRIVERS_PATH").ok(),
    };
    if let Some(mut cached) = opts.cache_file.as_deref().and_then(|f| read_cache(f, &key)) {
        // Only the GStreamer factory may be missing (a host that doesn't
        // record filled the cache): ask for it alone.
        if let (Some(find), false, Some(vaapi)) =
            (opts.gst_factory, cached.gst_probed, cached.vaapi.as_mut())
        {
            vaapi.gst_h264_factory = find(&vaapi.device);
            cached.gst_probed = true;
            if let Some(file) = &opts.cache_file {
                write_cache(file, &cached);
            }
        }
        return Detected {
            caps: Capabilities {
                ffmpeg: Some(cached.ffmpeg),
                ffprobe,
                vaapi: cached.vaapi,
                cores,
            },
            notes,
            cached: true,
        };
    }

    let log = opts.log_dir.as_deref().and_then(|dir| {
        let stamp = proc::log_stamp(chrono::Local::now());
        proc::create_log(dir, &format!("{stamp}-{}-caps", std::process::id()))
            .ok()
            .map(|(file, _)| file)
    });
    let run = |args: &[&str], env: Option<(&str, &str)>| {
        let mut spec = Spec::new(&ffmpeg, Limit::Timeout(PROBE_TIMEOUT))
            .label("ffmpeg")
            .args(args)
            .capture_stdout(4 << 20);
        if let Some((k, v)) = env {
            spec = spec.env(k, v);
        }
        if let Some(file) = log.as_ref().and_then(|f| f.try_clone().ok()) {
            spec = spec.stderr_to(file);
        }
        spec.run()
    };

    let version = match run(&["-hide_banner", "-version"], None) {
        Ok(out) if out.success() => parse_version(&out.stdout_text()),
        Ok(out) => {
            notes.push(format!("`ffmpeg -version`: {}", out.describe()));
            None
        }
        Err(e) => {
            notes.push(format!("`ffmpeg -version`: {e}"));
            None
        }
    };
    let encoders = match run(&["-hide_banner", "-encoders"], None) {
        Ok(out) if out.success() => parse_encoders(&out.stdout_text()),
        Ok(out) => {
            notes.push(format!("`ffmpeg -encoders`: {}", out.describe()));
            BTreeSet::new()
        }
        Err(e) => {
            notes.push(format!("`ffmpeg -encoders`: {e}"));
            BTreeSet::new()
        }
    };
    let ffmpeg_caps = FfmpegCaps {
        version: version.unwrap_or_else(|| "unknown".to_owned()),
        identity: key.ffmpeg.clone(),
        encoders,
    };

    let vaapi = match choice {
        Some((device, Some(vendor))) => {
            let ffmpeg_h264 = if !opts.probe_va {
                false
            } else if !ffmpeg_caps.encoders.contains("h264_vaapi") {
                notes.push("this ffmpeg has no h264_vaapi encoder".to_owned());
                false
            } else {
                let argv = va_probe_args(&device);
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                // An explicitly chosen NVIDIA node keeps the inherited driver (A§8.10).
                let env = gpu::libva_driver_for(vendor).map(|d| (gpu::LIBVA_DRIVER_NAME, d));
                match run(&args, env) {
                    Ok(out) if out.success() => true,
                    Ok(out) => {
                        notes.push(format!(
                            "the h264_vaapi check on {device} failed: {}",
                            out.describe()
                        ));
                        false
                    }
                    Err(e) => {
                        notes.push(format!("the h264_vaapi check on {device} failed: {e}"));
                        false
                    }
                }
            };
            Some(VaapiCaps {
                gst_h264_factory: opts.gst_factory.and_then(|find| find(&device)),
                device,
                vendor,
                ffmpeg_h264,
            })
        }
        _ => None,
    };
    if let Some(file) = &opts.cache_file {
        write_cache(
            file,
            &CacheFile {
                key,
                ffmpeg: ffmpeg_caps.clone(),
                vaapi: vaapi.clone(),
                gst_probed: opts.gst_factory.is_some(),
            },
        );
    }
    Detected {
        caps: Capabilities {
            ffmpeg: Some(ffmpeg_caps),
            ffprobe,
            vaapi,
            cores,
        },
        notes,
        cached: false,
    }
}

/// The exact VA export chain of A§9.3 on two frames, as the core builds
/// it (`media_argv::vaapi_check_argv`, which `qrec doctor` runs too),
/// without the program name: the caller runs the resolved ffmpeg.
pub fn va_probe_args(device: &str) -> Vec<String> {
    qrec_core::media_argv::vaapi_check_argv(device)
        .into_iter()
        .skip(1)
        .collect()
}

/// `ffmpeg version 8.1.2 Copyright …` → `8.1.2`.
pub fn parse_version(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("ffmpeg version "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
}

/// The encoder names of `ffmpeg -encoders` (the lines after ` ------`).
pub fn parse_encoders(text: &str) -> BTreeSet<String> {
    text.lines()
        .skip_while(|line| line.trim() != "------")
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            let name = parts.next()?;
            (flags.len() == 6 && flags.starts_with(['V', 'A', 'S'])).then(|| name.to_owned())
        })
        .collect()
}

/// `program` resolved like `exec` would, then canonicalized.
fn resolve_program(program: &std::ffi::OsStr) -> Option<PathBuf> {
    let found = which::which(program).ok()?;
    Some(fs::canonicalize(&found).unwrap_or(found))
}

// ------------------------------------------------------------------ cache

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheKey {
    ffmpeg: String,
    ffmpeg_len: u64,
    ffmpeg_mtime_ns: u128,
    va_device: Option<String>,
    va_vendor: Option<u16>,
    probed_va: bool,
    libva_driver: Option<String>,
    libva_drivers_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    key: CacheKey,
    ffmpeg: FfmpegCaps,
    vaapi: Option<VaapiCaps>,
    /// `vaapi.gst_h264_factory` was looked for (it may still be `None`).
    gst_probed: bool,
}

fn read_cache(path: &Path, key: &CacheKey) -> Option<CacheFile> {
    let bytes = fs::read(path).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    (file.key == *key).then_some(file)
}

fn write_cache(path: &Path, file: &CacheFile) {
    let Ok(bytes) = serde_json::to_vec_pretty(file) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fsx::replace(path, &bytes, fsx::Sync::None) {
        tracing::debug!("can't write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::gpu::{VENDOR_AMD, VENDOR_INTEL, VENDOR_NVIDIA};

    const ENCODERS: &str = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              libx264 H.264\n V....D h264_vaapi           H.264/AVC (VAAPI) (codec h264)\n VF...D png                  PNG image\n A....D aac                  AAC\n";

    /// A fake ffmpeg: canned `-version`/`-encoders`; the VA check records
    /// `LIBVA_DRIVER_NAME` and its arguments, and fails when `va-fails` exists.
    fn fake_ffmpeg(dir: &Path) -> PathBuf {
        let script = dir.join("ffmpeg");
        let body = format!(
            r#"#!/bin/sh
echo run >> "{dir}/runs"
case "$*" in
  *-version*) echo "ffmpeg version 9.9-test Copyright (c) the FFmpeg developers"; echo "built with tests" ;;
  *-encoders*) printf '{encoders}' ;;
  *h264_vaapi*) echo "$LIBVA_DRIVER_NAME $*" > "{dir}/va-probe"; [ -e "{dir}/va-fails" ] && exit 1; exit 0 ;;
esac
"#,
            dir = dir.display(),
            encoders = ENCODERS.replace('\n', "\\n"),
        );
        fs::write(&script, body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn fake_sysfs(dir: &Path, nodes: &[(&str, &str)]) -> PathBuf {
        let sys = dir.join("sys");
        for (name, vendor) in nodes {
            fs::create_dir_all(sys.join(name).join("device")).unwrap();
            fs::write(sys.join(name).join("device/vendor"), format!("{vendor}\n")).unwrap();
        }
        fs::create_dir_all(sys.join("card0")).unwrap();
        sys
    }

    fn runs(dir: &Path) -> usize {
        fs::read_to_string(dir.join("runs")).map_or(0, |s| s.lines().count())
    }

    /// An executable `ffprobe` that does nothing.
    fn fake_ffprobe(dir: &Path) -> PathBuf {
        let script = dir.join("ffprobe");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn options(dir: &Path, sys: PathBuf) -> CapsOptions {
        CapsOptions {
            ffmpeg: fake_ffmpeg(dir).into_os_string(),
            ffprobe: fake_ffprobe(dir).into_os_string(),
            sys_class_drm: sys,
            dev_dri: "/dev/dri".into(),
            probe_va: true,
            cache_file: Some(dir.join("cache/capabilities.json")),
            log_dir: Some(dir.join("logs")),
            gst_factory: None,
        }
    }

    #[test]
    fn detects_ffmpeg_and_the_intel_node_never_nvidia() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(
            tmp.path(),
            &[("renderD128", "0x10de"), ("renderD129", "0x8086")],
        );
        let opts = options(tmp.path(), sys);
        let found = detect(&opts, &VaConfig::default());
        assert!(!found.cached);
        let ffmpeg = found.caps.ffmpeg.as_ref().unwrap();
        assert_eq!(ffmpeg.version, "9.9-test");
        assert_eq!(
            ffmpeg.identity,
            fs::canonicalize(&opts.ffmpeg).unwrap().to_string_lossy()
        );
        assert_eq!(
            ffmpeg
                .encoders
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["aac", "h264_vaapi", "libx264", "png"]
        );
        let va = found.caps.vaapi.as_ref().unwrap();
        assert_eq!(
            (va.device.as_str(), va.vendor, va.ffmpeg_h264),
            ("/dev/dri/renderD129", VENDOR_INTEL, true)
        );
        assert_eq!(va.gst_h264_factory, None);
        let probe = fs::read_to_string(tmp.path().join("va-probe")).unwrap();
        assert!(probe.starts_with("iHD "), "{probe}");
        assert!(
            probe.contains("-init_hw_device vaapi=va:/dev/dri/renderD129"),
            "{probe}"
        );
        assert!(found.caps.cores >= 1);
        assert_eq!(runs(tmp.path()), 3);
        let logs = fs::read_dir(tmp.path().join("logs")).unwrap().count();
        assert_eq!(logs, 1, "one log for the three probes");

        assert_eq!(
            found.caps.ffprobe.as_deref(),
            Some(
                fs::canonicalize(&opts.ffprobe)
                    .unwrap()
                    .to_string_lossy()
                    .as_ref()
            )
        );

        // Nothing changed: the cache answers without running anything.
        let again = detect(&opts, &VaConfig::default());
        assert!(again.cached);
        assert_eq!(again.caps, found.caps);
        assert_eq!(runs(tmp.path()), 3);

        // ffprobe is looked up every time, cache or not (exports need it).
        let mut no_probe = opts.clone();
        no_probe.ffprobe = "qrec-no-such-ffprobe".into();
        let missing = detect(&no_probe, &VaConfig::default());
        assert!(missing.cached);
        assert_eq!(missing.caps.ffprobe, None);
        assert!(
            missing.notes.iter().any(|n| n.contains("qrec-no-such-ffprobe")),
            "{:?}",
            missing.notes
        );

        // A configured node changes the key and is probed (NVIDIA too, when chosen).
        let nvidia = VaConfig {
            project: None,
            config: Some("/dev/dri/renderD128".into()),
        };
        let chosen = detect(&opts, &nvidia);
        let va = chosen.caps.vaapi.unwrap();
        assert_eq!(
            (va.device.as_str(), va.vendor),
            ("/dev/dri/renderD128", VENDOR_NVIDIA)
        );
        // An explicitly chosen NVIDIA node keeps the inherited LIBVA_DRIVER_NAME (A§8.10).
        let inherited = std::env::var("LIBVA_DRIVER_NAME").unwrap_or_default();
        let probe = fs::read_to_string(tmp.path().join("va-probe")).unwrap();
        assert!(probe.starts_with(&format!("{inherited} -")), "{probe}");
    }

    #[test]
    fn the_gst_factory_is_looked_for_once_and_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD129", "0x8086")]);
        let mut opts = options(tmp.path(), sys);
        let plain = detect(&opts, &VaConfig::default());
        assert_eq!(plain.caps.vaapi.as_ref().unwrap().gst_h264_factory, None);
        let ran = runs(tmp.path());

        // A host that records asks for the factory: only that runs.
        opts.gst_factory = Some(|device| Some(format!("va-for-{device}")));
        let recording = detect(&opts, &VaConfig::default());
        assert!(recording.cached);
        assert_eq!(
            recording.caps.vaapi.unwrap().gst_h264_factory.as_deref(),
            Some("va-for-/dev/dri/renderD129")
        );
        assert_eq!(runs(tmp.path()), ran, "ffmpeg isn't asked again");

        // Later hosts that don't record still get it from the cache.
        opts.gst_factory = None;
        let later = detect(&opts, &VaConfig::default());
        assert_eq!(
            later.caps.vaapi.unwrap().gst_h264_factory.as_deref(),
            Some("va-for-/dev/dri/renderD129")
        );
    }

    #[test]
    fn failures_and_switches() {
        let tmp = tempfile::tempdir().unwrap();
        let sys = fake_sysfs(tmp.path(), &[("renderD128", "0x10de")]);
        let mut opts = options(tmp.path(), sys);
        opts.cache_file = None;
        let only_nvidia = detect(&opts, &VaConfig::default());
        assert_eq!(
            only_nvidia.caps.vaapi, None,
            "NVIDIA is never picked automatically"
        );

        let sys = fake_sysfs(tmp.path(), &[("renderD130", "0x1002")]);
        opts.sys_class_drm = sys;
        fs::write(tmp.path().join("va-fails"), "").unwrap();
        let failed = detect(&opts, &VaConfig::default());
        let va = failed.caps.vaapi.unwrap();
        assert_eq!((va.vendor, va.ffmpeg_h264), (VENDOR_AMD, false));
        assert!(
            failed.notes.iter().any(|n| n.contains("h264_vaapi check")),
            "{:?}",
            failed.notes
        );
        assert!(
            fs::read_to_string(tmp.path().join("va-probe"))
                .unwrap()
                .starts_with("radeonsi ")
        );

        opts.probe_va = false;
        let before = runs(tmp.path());
        let skipped = detect(&opts, &VaConfig::default());
        assert!(!skipped.caps.vaapi.unwrap().ffmpeg_h264);
        assert_eq!(runs(tmp.path()), before + 2, "no VA check");

        let unknown = detect(
            &opts,
            &VaConfig {
                project: Some("/dev/dri/renderD199".into()),
                config: None,
            },
        );
        assert_eq!(unknown.caps.vaapi, None);

        opts.ffmpeg = "qrec-no-such-ffmpeg".into();
        let missing = detect(&opts, &VaConfig::default());
        assert_eq!((missing.caps.ffmpeg, missing.caps.vaapi), (None, None));
        assert!(missing.caps.ffprobe.is_some(), "ffprobe is looked up anyway");
        assert!(missing.notes[0].contains("not installed"));
    }

    #[test]
    fn the_real_ffmpeg_without_touching_a_gpu() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = CapsOptions {
            sys_class_drm: tmp.path().join("no-sysfs"),
            cache_file: None,
            log_dir: Some(tmp.path().join("logs")),
            ..CapsOptions::system(None, None)
        };
        let found = detect(&opts, &VaConfig::default());
        let ffmpeg = found.caps.ffmpeg.expect("the dev shell has ffmpeg");
        assert!(
            ffmpeg.version.starts_with(char::is_numeric),
            "{}",
            ffmpeg.version
        );
        for encoder in ["libx264", "libvpx-vp9", "aac", "libopus", "png"] {
            assert!(ffmpeg.encoders.contains(encoder), "{encoder} missing");
        }
        assert!(Path::new(&ffmpeg.identity).is_absolute());
        assert_eq!(found.caps.vaapi, None);
    }

    #[test]
    fn parsers() {
        assert_eq!(
            parse_version("ffmpeg version 8.1.2 Copyright (c) 2000-2026").as_deref(),
            Some("8.1.2")
        );
        assert_eq!(parse_version("nope"), None);
        assert_eq!(parse_encoders(ENCODERS).len(), 4);
        assert!(
            parse_encoders("V....D x\n").is_empty(),
            "nothing before the separator counts"
        );
        let args = va_probe_args("/dev/dri/renderD129");
        assert!(args.windows(2).any(|w| w == ["-c:v", "h264_vaapi"]));
        assert_eq!(args.last().unwrap(), "-");
    }
}
