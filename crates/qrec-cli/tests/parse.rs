//! The command tree accepts every command line of SPEC §14, the CLI column
//! of SPEC §10.2 (and its niri snippet) and the A§4.4/A§10 extensions, and
//! rejects malformed lines as usage errors (exit 2).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::CommandFactory;
use clap::error::ErrorKind;
use pretty_assertions::assert_eq;
use qrec_cli::*;
use qrec_core::error::NOT_MY_SWAMP;
use qrec_core::{ClipId, Crop, LayerId, Rect, TakeId, TrackId};
use serde_json::{Value, json};

/// Splits a command line on whitespace; double quotes group words.
fn words(line: &str) -> Vec<OsString> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    out.push(OsString::from(std::mem::take(&mut word)));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(word.into());
    }
    out
}

fn parse(line: &str) -> Result<Cli, clap::Error> {
    parse_args(words(line))
}

#[track_caller]
fn ok(line: &str) -> Cli {
    parse(line).unwrap_or_else(|e| panic!("`{line}` should parse:\n{e}"))
}

#[track_caller]
fn cmd(line: &str) -> Cmd {
    ok(line)
        .command
        .unwrap_or_else(|| panic!("`{line}` parsed without a command"))
}

#[track_caller]
fn all_parse(lines: &[&str]) {
    let failures: Vec<String> = lines
        .iter()
        .filter_map(|line| parse(line).err().map(|e| format!("`{line}`:\n{e}")))
        .collect();
    assert!(
        failures.is_empty(),
        "{} line(s) failed to parse:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Asserts `line` is a usage error with exit code 2 and returns its kind.
#[track_caller]
fn usage_error(line: &str) -> ErrorKind {
    match parse(line) {
        Ok(cli) => panic!("`{line}` should be a usage error, parsed as {cli:?}"),
        Err(e) => {
            assert_eq!(e.exit_code(), 2, "`{line}`: {e}");
            assert!(e.use_stderr(), "`{line}` printed help instead of an error");
            e.kind()
        }
    }
}

/// Runs `line` through `run_with`: (exit code, stdout, stderr).
fn run(line: &str) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_with(words(line), &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn clip(id: &str) -> ClipId {
    id.parse().unwrap()
}

fn take(id: &str) -> TakeId {
    id.parse().unwrap()
}

fn layer(id: &str) -> LayerId {
    id.parse().unwrap()
}

fn track(id: &str) -> TrackId {
    id.parse().unwrap()
}

// ------------------------------------------------------------- accepted lines

#[test]
fn spec_14_reference_lines_parse() {
    all_parse(&[
        "qrec",
        "qrec init",
        "qrec init --canvas 1920x1080@30",
        "qrec status",
        "qrec status --json",
        "qrec doctor",
        "qrec sources list",
        "qrec scene layer add screen --output DP-4",
        "qrec scene layer add screen --output DP-4 --rect 0,0,1920,1080",
        "qrec scene layer add screen --output DP-4 --rect 0,0,1920,1080 --name \"Main screen\"",
        "qrec scene layer add camera --device /dev/video0",
        "qrec scene layer add camera --device /dev/video0 --rect 1500,760,384,288",
        "qrec scene layer set L1 --rect 0,0,960,540",
        "qrec scene layer set L1 --crop 0,0,40,0",
        "qrec scene layer set L1 --opacity 0.8",
        "qrec scene layer set L1 --visible true",
        "qrec scene layer set L1 --visible false",
        "qrec scene layer set L1 --fit contain",
        "qrec scene layer set L2 --fit cover",
        "qrec scene layer set L1 --rect 0,0,960,540 --crop 0,0,0,0 --opacity 0.8 --visible true --fit contain",
        "qrec scene layer order L2 --to 0",
        "qrec scene layer remove L2",
        "qrec scene audio add mic",
        "qrec scene audio add system",
        "qrec scene audio add mic --source alsa_input.usb-Blue_Microphones_Yeti_Nano-00.analog-stereo",
        "qrec scene audio add system --source alsa_output.pci-0000_00_1f.3.analog-stereo.monitor",
        "qrec scene audio set A1 --gain-db -3",
        "qrec scene audio set A1 --mute true",
        "qrec scene audio set A1 --mute false",
        "qrec scene audio set A1 --sync-offset-ms 80",
        "qrec scene audio set A1 --gain-db -3 --mute false --sync-offset-ms 80",
        "qrec scene audio remove A2",
        "qrec scene apply --take take-0001",
        "qrec scene apply --to-all-takes",
        "qrec scene apply --from-take take-0001",
        // "scene commands accept --take <take> to edit a take's snapshot"
        "qrec scene layer set L1 --take take-0001 --opacity 0.5",
        "qrec scene layer order L1 --to 1 --take take-0002",
        "qrec scene layer remove L2 --take take-0001",
        "qrec scene audio set A2 --take take-0001 --mute true",
        "qrec scene audio remove A2 --take take-0001",
        "qrec record start",
        "qrec record start --over c7f3",
        "qrec record start --over c7f3 c8a1",
        "qrec record start --over-selection",
        "qrec record start --countdown 3",
        "qrec record start --over c7f3 c8a1 --countdown 3",
        "qrec record start --over-selection --countdown 10",
        "qrec record stop",
        "qrec record toggle",
        "qrec record cancel",
        "qrec take list",
        "qrec take list --unused",
        "qrec take show take-0001",
        "qrec take discard-last",
        "qrec take purge take-0001 --yes",
        "qrec take purge take-0001 take-0002 --yes",
        "qrec clip list",
        "qrec clip add take-0001",
        "qrec clip add take-0001 --at 2",
        "qrec clip add take-0001 --after c7f3",
        "qrec clip move c7f3 --to 2",
        "qrec clip move c7f3 --by -1",
        "qrec clip move c7f3 --by +1",
        "qrec clip move c7f3 c8a1 --by 2",
        "qrec clip move c7f3 --before c8a1",
        "qrec clip move c7f3 --after c8a1",
        "qrec clip trim c7f3 --start 13f",
        "qrec clip trim c7f3 --end 400f",
        "qrec clip trim c7f3 --start 13f --end 400f",
        "qrec clip split c7f3 --at 13f",
        "qrec clip split c7f3 --at 412f",
        "qrec clip split c7f3 --at 13.7s",
        "qrec clip split c7f3 --at 00:00:13:21",
        "qrec clip split c7f3 --at 00:00:13.700",
        "qrec clip split --timeline --at 13f",
        "qrec clip remove c7f3",
        "qrec clip remove c7f3 c8a1",
        "qrec autocut",
        "qrec autocut --clips c7f3",
        "qrec autocut --clips c7f3 c8a1",
        "qrec autocut --all",
        "qrec autocut --track A1",
        "qrec autocut --threshold-db -35",
        "qrec autocut --min-silence-ms 400",
        "qrec autocut --padding-ms 120",
        "qrec autocut --min-keep-ms 200",
        "qrec autocut --dry-run",
        "qrec autocut --clips c7f3 c8a1 --track A1 --threshold-db -35 --min-silence-ms 400 \
         --padding-ms 120 --min-keep-ms 200 --dry-run",
        "qrec autocut --all --track A1 --threshold-db -35 --min-silence-ms 400 --padding-ms 120 \
         --min-keep-ms 200 --dry-run",
        "qrec undo",
        "qrec undo 3",
        "qrec redo",
        "qrec redo 2",
        "qrec history",
        "qrec history --limit 20",
        "qrec export",
        "qrec export --format mp4",
        "qrec export --format webm",
        "qrec export --quality draft",
        "qrec export --quality standard",
        "qrec export --quality high",
        "qrec export --out /tmp/out.mp4",
        "qrec export --format webm --quality high --out /tmp/out.webm",
        "qrec frame save --at 13f",
        "qrec frame save --at 13f --layer L2",
        "qrec frame save --at 13f --out /tmp/frame.png",
        "qrec frame save --at 00:00:13:21 --layer L2 --out /tmp/frame.png",
        "qrec seek --to 13f",
        "qrec seek --by -2s",
        "qrec seek --by +2s",
        "qrec seek --edit prev",
        "qrec seek --edit next",
        "qrec play",
        "qrec pause",
        "qrec config get theme",
        "qrec config set theme swamp-day",
        "qrec keys",
        "qrec keys --json",
        "qrec schema",
        "qrec agent-guide",
        // SPEC §16
        "qrec history restore 12 --yes",
    ]);
}

#[test]
fn spec_10_2_cli_column_parses() {
    all_parse(&[
        "qrec record toggle",
        "qrec record start --over c7f3 c8a1",
        "qrec take discard-last",
        "qrec record cancel",
        "qrec play",
        "qrec seek --by -1f",
        "qrec seek --by -1s",
        "qrec seek --edit prev",
        "qrec seek --to start",
        "qrec clip move c7f3 --by -1",
        "qrec clip split c7f3 --at 13f",
        "qrec clip trim c7f3 --start 13f",
        "qrec clip trim c7f3 --end 13f",
        "qrec clip trim c7f3 --start +1f",
        "qrec clip remove c7f3 c8a1",
        "qrec autocut --clips c7f3 c8a1",
        "qrec autocut --dry-run",
        "qrec undo",
        "qrec redo",
        "qrec frame save --at 13f",
        "qrec export",
        "qrec take list --unused",
        "qrec keys",
        "qrec config",
    ]);
}

#[test]
fn niri_snippet_parses() {
    // binds { Mod+Shift+R { spawn "qrec" "record" "toggle"; } … }
    let bind = |argv: &[&str]| {
        parse_args(argv.iter().map(OsString::from))
            .expect("niri bind parses")
            .command
    };
    assert_eq!(
        bind(&["qrec", "record", "toggle"]),
        Some(Cmd::Record(RecordCmd::Toggle(WaitArgs { wait: false })))
    );
    assert_eq!(
        bind(&["qrec", "record", "start", "--over-selection"]),
        Some(Cmd::Record(RecordCmd::Start(RecordStartArgs {
            over_selection: true,
            ..Default::default()
        })))
    );
    assert_eq!(
        bind(&["qrec", "take", "discard-last"]),
        Some(Cmd::Take(TakeCmd::DiscardLast))
    );
}

#[test]
fn architecture_extensions_parse() {
    all_parse(&[
        "qrec init --canvas 1920x1080@30 --name demo",
        "qrec init --canvas 1920x1080@30000/1001",
        "qrec project set --name demo",
        "qrec project set --canvas 2560x1440@60 --yes",
        "qrec project set --background #12170F",
        "qrec project set --countdown 3 --encoder vaapi --vaapi-device /dev/dri/renderD129",
        "qrec project set --vaapi-device auto --encoder x264",
        "qrec project set --encoder auto",
        "qrec scene layer set L1 --name Face --locked true",
        "qrec scene layer set L1 --locked false",
        "qrec scene audio set A1 --label Yeti --source alsa_input.fake",
        "qrec scene audio add system --source alsa_output.fake.monitor --label Speakers",
        "qrec scene apply --from-take take-0001 --to-all-takes",
        "qrec scene apply --from-take take-0001 --take take-0002 take-0003",
        "qrec scene apply --take take-0002,take-0003",
        "qrec autocut settings --auto-run true",
        "qrec autocut settings --track A1",
        "qrec autocut settings --track auto",
        "qrec autocut settings --auto-run false --threshold-db -40 --min-silence-ms 500 \
         --padding-ms 100 --min-keep-ms 250",
        "qrec record start --over c7f3 --countdown 3 --wait",
        "qrec record start --over c7f3,c8a1",
        "qrec record toggle --wait",
        "qrec clip trim c7f3 --start -2s --end +1f --timeline",
        "qrec clip split c7f3 --at 450f --timeline",
        "qrec clip add take-0001 --after-selection",
        "qrec clip remove c7f3,c8a1",
        "qrec history restore 12",
        "qrec history clear --yes",
        "qrec export --format webm --codec av1",
        "qrec export --format webm --codec vp9",
        "qrec export --codec h264",
        "qrec export cancel",
        "qrec seek --to end",
        "qrec config",
        "qrec completions bash",
        "qrec completions zsh",
        "qrec completions fish",
        "qrec version",
        "qrec host --headless --project /tmp/p --lock-fd 3 --ready-fd 4",
        "qrec host --headless",
    ]);
}

#[test]
fn hyphen_values_parse_as_values() {
    assert_eq!(
        cmd("qrec seek --by -1f"),
        Cmd::Seek(SeekArgs {
            by: Some("-1f".into()),
            ..Default::default()
        })
    );
    assert_eq!(
        cmd("qrec seek --by -00:00:01:00"),
        Cmd::Seek(SeekArgs {
            by: Some("-00:00:01:00".into()),
            ..Default::default()
        })
    );
    assert_eq!(
        cmd("qrec clip move c7f3 c8a1 --by -2"),
        Cmd::Clip(ClipCmd::Move(ClipMoveArgs {
            clips: vec![clip("c7f3"), clip("c8a1")],
            to: MoveToArgs {
                by: Some(-2),
                ..Default::default()
            },
        }))
    );
    assert_eq!(
        cmd("qrec clip move c7f3 --by +1"),
        Cmd::Clip(ClipCmd::Move(ClipMoveArgs {
            clips: vec![clip("c7f3")],
            to: MoveToArgs {
                by: Some(1),
                ..Default::default()
            },
        }))
    );
    assert_eq!(
        cmd("qrec scene audio set A1 --gain-db -3 --sync-offset-ms -80"),
        Cmd::Scene(SceneCmd::Audio(AudioCmd::Set(AudioSetArgs {
            track: track("A1"),
            scene: SceneTargetArgs::default(),
            patch: AudioPatchArgs {
                gain_db: Some(-3.0),
                sync_offset_ms: Some(-80),
                ..Default::default()
            },
        })))
    );
    assert_eq!(
        cmd("qrec clip trim c7f3 --start -2s --end -1f"),
        Cmd::Clip(ClipCmd::Trim(ClipTrimArgs {
            clip: clip("c7f3"),
            edges: TrimArgs {
                start: Some("-2s".into()),
                end: Some("-1f".into())
            },
            timeline: false,
        }))
    );
    assert_eq!(
        cmd("qrec autocut --threshold-db -35"),
        Cmd::Autocut(AutocutArgs {
            command: None,
            scope: AutocutScopeArgs::default(),
            params: AutocutParamArgs {
                threshold_db: Some(-35.0),
                ..Default::default()
            },
            dry_run: false,
        })
    );
    assert_eq!(
        cmd("qrec autocut settings --threshold-db -40.5"),
        Cmd::Autocut(AutocutArgs {
            command: Some(AutocutCmd::Settings(AutocutSettingsArgs {
                threshold_db: Some(-40.5),
                ..Default::default()
            })),
            scope: AutocutScopeArgs::default(),
            params: AutocutParamArgs::default(),
            dry_run: false,
        })
    );
    assert_eq!(
        cmd("qrec scene layer set L1 --rect -100,-50,640,360"),
        Cmd::Scene(SceneCmd::Layer(LayerCmd::Set(LayerSetArgs {
            layer: layer("L1"),
            scene: SceneTargetArgs::default(),
            patch: LayerPatchArgs {
                rect: Some(Rect::new(-100, -50, 640, 360)),
                ..Default::default()
            },
        })))
    );
    assert_eq!(
        cmd("qrec config set autocut.threshold_db -40"),
        Cmd::Config(ConfigArgs {
            command: Some(ConfigCmd::Set(ConfigSetArgs {
                key: "autocut.threshold_db".into(),
                value: "-40".into()
            }))
        })
    );
    // The core rejects relative values here; the parser passes them through.
    assert_eq!(
        cmd("qrec frame save --at -1f"),
        Cmd::Frame(FrameCmd::Save(FrameSaveArgs {
            at: "-1f".into(),
            layer: None,
            out: None
        }))
    );
}

#[test]
fn parsed_values_are_typed() {
    assert_eq!(
        cmd("qrec scene layer add camera --device /dev/video0 --rect 1500,760,384,288 --name Face"),
        Cmd::Scene(SceneCmd::Layer(LayerCmd::Add(LayerAddCmd::Camera(
            CameraLayerArgs {
                device: "/dev/video0".into(),
                layout: LayerAddLayout {
                    rect: Some(Rect::new(1500, 760, 384, 288)),
                    name: Some("Face".into())
                },
            }
        ))))
    );
    assert_eq!(
        cmd(
            "qrec scene layer set L2 --take take-0003 --crop 1,2,3,4 --opacity 0.8 --visible false --fit cover --locked true"
        ),
        Cmd::Scene(SceneCmd::Layer(LayerCmd::Set(LayerSetArgs {
            layer: layer("L2"),
            scene: SceneTargetArgs {
                take: Some(take("take-0003"))
            },
            patch: LayerPatchArgs {
                crop: Some(Crop {
                    top: 1,
                    right: 2,
                    bottom: 3,
                    left: 4
                }),
                opacity: Some(0.8),
                visible: Some(false),
                fit: Some(FitArg::Cover),
                locked: Some(true),
                ..Default::default()
            },
        })))
    );
    assert_eq!(
        cmd("qrec scene audio add system --source alsa_output.fake.monitor"),
        Cmd::Scene(SceneCmd::Audio(AudioCmd::Add(AudioAddArgs {
            role: AudioRoleArg::System,
            source: Some("alsa_output.fake.monitor".into()),
            label: None,
        })))
    );
    assert_eq!(
        cmd("qrec scene apply --from-take take-0001 --to-all-takes"),
        Cmd::Scene(SceneCmd::Apply(SceneApplyArgs {
            take: vec![],
            to_all_takes: true,
            from_take: Some(take("take-0001")),
        }))
    );
    assert_eq!(
        cmd("qrec record start --over c7f3 c8a1 --countdown 3 --wait"),
        Cmd::Record(RecordCmd::Start(RecordStartArgs {
            over: vec![clip("c7f3"), clip("c8a1")],
            over_selection: false,
            countdown: Some(3),
            wait: WaitArgs { wait: true },
        }))
    );
    assert_eq!(
        cmd("qrec clip split --timeline --at 00:00:13:20"),
        Cmd::Clip(ClipCmd::Split(ClipSplitArgs {
            clip: None,
            at: "00:00:13:20".into(),
            timeline: true
        }))
    );
    assert_eq!(
        cmd("qrec clip add take-0002 --after c7f3"),
        Cmd::Clip(ClipCmd::Add(ClipAddArgs {
            take: take("take-0002"),
            at: InsertAtArgs {
                after: Some(clip("c7f3")),
                ..Default::default()
            },
        }))
    );
    assert_eq!(
        cmd("qrec take purge take-0003,take-0004"),
        Cmd::Take(TakeCmd::Purge(TakePurgeArgs {
            takes: vec![take("take-0003"), take("take-0004")]
        }))
    );
    assert_eq!(cmd("qrec undo"), Cmd::Undo(StepsArgs { steps: 1 }));
    assert_eq!(cmd("qrec redo 4"), Cmd::Redo(StepsArgs { steps: 4 }));
    assert_eq!(
        cmd("qrec history --limit 20"),
        Cmd::History(HistoryArgs {
            command: None,
            limit: Some(20)
        })
    );
    assert_eq!(
        cmd("qrec history restore 12"),
        Cmd::History(HistoryArgs {
            command: Some(HistoryCmd::Restore(HistoryRestoreArgs { seq: 12 })),
            limit: None
        })
    );
    assert_eq!(
        cmd("qrec export"),
        Cmd::Export(ExportArgs {
            command: None,
            format: FormatArg::Mp4,
            quality: QualityArg::Standard,
            codec: None,
            out: None,
        })
    );
    assert_eq!(
        cmd("qrec export --format webm --quality high --codec av1 --out /tmp/x.webm"),
        Cmd::Export(ExportArgs {
            command: None,
            format: FormatArg::Webm,
            quality: QualityArg::High,
            codec: Some(CodecArg::Av1),
            out: Some(PathBuf::from("/tmp/x.webm")),
        })
    );
    assert!(matches!(
        cmd("qrec export cancel"),
        Cmd::Export(ExportArgs {
            command: Some(ExportCmd::Cancel),
            ..
        })
    ));
    assert_eq!(
        cmd("qrec seek --edit next"),
        Cmd::Seek(SeekArgs {
            edit: Some(EditArg::Next),
            ..Default::default()
        })
    );
    assert_eq!(
        cmd(
            "qrec project set --vaapi-device auto --encoder x264 --background #12170f --countdown 3"
        ),
        Cmd::Project(ProjectCmd::Set(ProjectSetArgs {
            name: None,
            canvas: None,
            background: Some("#12170F".parse().unwrap()),
            countdown: Some(3),
            encoder: Some(EncoderArg::X264),
            vaapi_device: Some(AutoOr::Auto),
        }))
    );
    assert_eq!(
        cmd("qrec autocut settings --track A2 --auto-run true"),
        Cmd::Autocut(AutocutArgs {
            command: Some(AutocutCmd::Settings(AutocutSettingsArgs {
                auto_run: Some(true),
                track: Some(AutoOr::Value(track("A2"))),
                ..Default::default()
            })),
            scope: AutocutScopeArgs::default(),
            params: AutocutParamArgs::default(),
            dry_run: false,
        })
    );
    assert_eq!(
        cmd("qrec completions zsh"),
        Cmd::Completions(CompletionsArgs {
            shell: clap_complete::Shell::Zsh
        })
    );
    assert_eq!(
        cmd("qrec host --headless --lock-fd 3 --ready-fd 4"),
        Cmd::Host(HostArgs {
            headless: true,
            lock_fd: Some(3),
            ready_fd: Some(4)
        })
    );
}

#[test]
fn global_flags_go_anywhere() {
    let before = ok("qrec --json --yes --project /tmp/p -v take purge take-0003");
    let after = ok("qrec take purge take-0003 --json --yes --project /tmp/p -v");
    let between = ok("qrec take --json --yes purge --project /tmp/p -v take-0003");
    assert_eq!(before, after);
    assert_eq!(before, between);
    assert_eq!(
        before.global,
        Global {
            project: Some("/tmp/p".into()),
            json: true,
            yes: true,
            quiet: false,
            verbose: 1
        }
    );
    // A global flag between a command and its subcommand doesn't count as one of its options.
    assert_eq!(
        cmd("qrec history --yes clear"),
        Cmd::History(HistoryArgs {
            command: Some(HistoryCmd::Clear),
            limit: None
        })
    );
    assert_eq!(ok("qrec -vv status").global.verbose, 2);
    assert!(ok("qrec status -q").global.quiet);
    assert_eq!(ok("qrec").command, None);
    assert_eq!(ok("qrec --json").global.json, true);
}

// ------------------------------------------------------------- rejected lines

#[test]
fn malformed_lines_are_usage_errors() {
    use ErrorKind::*;
    let cases: &[(&str, ErrorKind)] = &[
        ("qrec frobnicate", InvalidSubcommand),
        ("qrec status extra", UnknownArgument),
        ("qrec clip split c7f3", MissingRequiredArgument),
        ("qrec clip split --at 13f", MissingRequiredArgument),
        ("qrec clip move c7f3", MissingRequiredArgument),
        ("qrec clip move --by -1", MissingRequiredArgument),
        ("qrec clip move c7f3 --to 1 --by 2", ArgumentConflict),
        ("qrec clip trim c7f3", MissingRequiredArgument),
        (
            "qrec clip add take-0001 --at 1 --after c7f3",
            ArgumentConflict,
        ),
        ("qrec clip remove", MissingRequiredArgument),
        (
            "qrec record start --over c7f3 --over-selection",
            ArgumentConflict,
        ),
        ("qrec record start --countdown 11", ValueValidation),
        ("qrec export --format avi", InvalidValue),
        ("qrec export --codec h265", InvalidValue),
        ("qrec export --quality ultra", InvalidValue),
        ("qrec export --format webm cancel", ArgumentConflict),
        ("qrec scene layer set L1", MissingRequiredArgument),
        ("qrec scene layer set L1 --opacity 1.5", ValueValidation),
        ("qrec scene layer set L1 --visible yes", InvalidValue),
        ("qrec scene layer set L1 --rect 0,0,1920", ValueValidation),
        ("qrec scene layer set L1 --fit stretch", InvalidValue),
        ("qrec scene layer set \"L 1\" --opacity 1", ValueValidation),
        ("qrec scene layer add screen", MissingRequiredArgument),
        (
            "qrec scene layer add window --output DP-4",
            InvalidSubcommand,
        ),
        ("qrec scene audio add speaker", InvalidValue),
        ("qrec scene audio set A1", MissingRequiredArgument),
        ("qrec scene apply", MissingRequiredArgument),
        (
            "qrec scene apply --take take-0001 --to-all-takes",
            ArgumentConflict,
        ),
        ("qrec take show take-1", ValueValidation),
        ("qrec take show", MissingRequiredArgument),
        ("qrec take purge", MissingRequiredArgument),
        ("qrec undo 0", ValueValidation),
        ("qrec undo two", ValueValidation),
        ("qrec seek", MissingRequiredArgument),
        ("qrec seek --to 1f --by 1f", ArgumentConflict),
        ("qrec seek --edit forward", InvalidValue),
        ("qrec -q -v status", ArgumentConflict),
        ("qrec project set", MissingRequiredArgument),
        ("qrec project set --background red", ValueValidation),
        (
            "qrec project set --vaapi-device renderD129",
            ValueValidation,
        ),
        ("qrec project set --encoder nvenc", InvalidValue),
        ("qrec autocut --clips c7f3 --all", ArgumentConflict),
        ("qrec autocut settings", MissingRequiredArgument),
        ("qrec autocut settings --auto-run", InvalidValue),
        (
            "qrec autocut --dry-run settings --track A1",
            ArgumentConflict,
        ),
        ("qrec history restore", MissingRequiredArgument),
        ("qrec history --limit 3 clear", ArgumentConflict),
        ("qrec config set theme", MissingRequiredArgument),
        ("qrec completions tcsh", InvalidValue),
        ("qrec host", MissingRequiredArgument),
    ];
    let mut wrong = Vec::new();
    for (line, expected) in cases {
        let kind = usage_error(line);
        if kind != *expected {
            wrong.push(format!("`{line}`: expected {expected:?}, got {kind:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    // A bare command group shows its help, still as a usage error.
    assert_eq!(parse("qrec scene").unwrap_err().exit_code(), 2);
    assert_eq!(parse("qrec clip").unwrap_err().exit_code(), 2);
}

// ---------------------------------------------------------------- run_with

#[test]
fn no_command_says_the_gui_is_not_built() {
    assert_eq!(
        GUI_NOT_BUILT,
        "The Qrec GUI is not built yet: the screen designs are waiting for product-owner \
         approval (SPEC §15.1). Use the CLI: qrec --help"
    );
    let (code, out, err) = run("qrec");
    assert_eq!((code, out.as_str()), (1, ""));
    assert_eq!(err, format!("{GUI_NOT_BUILT}\n"));

    let (code, out, _) = run("qrec --json");
    assert_eq!(code, 1);
    let reply: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        reply,
        json!({"ok": false, "error": {"code": "unavailable", "message": GUI_NOT_BUILT}})
    );
}

#[test]
fn json_usage_errors_are_one_object_on_stdout() {
    for line in [
        "qrec --json clip split c7f3",
        "qrec clip split c7f3 --json",
        "qrec --json frobnicate",
        "qrec --json export --format avi",
        "qrec --json export --format webm cancel",
        "qrec --json scene",
        "qrec --json undo 0",
    ] {
        let (code, out, err) = run(line);
        assert_eq!(code, 2, "{line}");
        assert_eq!(err, "", "{line}");
        assert_eq!(out.lines().count(), 1, "{line}: {out}");
        assert!(
            out.starts_with(r#"{"ok":false,"error":{"code":"usage","message":""#),
            "{line}: {out}"
        );
        let reply: Value = serde_json::from_str(&out).unwrap();
        let error = reply["error"].as_object().unwrap();
        assert_eq!(
            error.keys().collect::<Vec<_>>(),
            ["code", "message"],
            "{line}"
        );
        let message = error["message"].as_str().unwrap();
        assert!(
            !message.is_empty() && !message.contains('\n'),
            "{line}: {message:?}"
        );
        assert!(
            !message.contains("\u{1b}["),
            "{line}: colors in {message:?}"
        );
    }
    let message =
        |line| serde_json::from_str::<Value>(&run(line).1).unwrap()["error"]["message"].clone();
    assert_eq!(
        message("qrec --json scene"),
        "'qrec scene' requires a subcommand: layer, audio, apply"
    );
    assert_eq!(
        message("qrec --json scene layer"),
        "'qrec scene layer' requires a subcommand: add, set, order, remove"
    );
    let missing = message("qrec --json clip split c7f3");
    assert!(missing.as_str().unwrap().contains("--at <T>"), "{missing}");
    assert!(
        missing.as_str().unwrap().contains("usage: qrec clip split"),
        "{missing}"
    );
}

#[test]
fn human_usage_errors_go_to_stderr() {
    let (code, out, err) = run("qrec clip split c7f3");
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.starts_with("error: "), "{err}");
    assert!(err.contains("--at <T>"), "{err}");
}

#[test]
fn help_and_version_print_normally() {
    let (code, out, err) = run("qrec --help");
    assert_eq!((code, err.as_str()), (0, ""));
    for needle in [
        "PROJECT FOLDER",
        NOT_MY_SWAMP,
        "--json",
        "EXIT CODES",
        "4 confirmation required",
        "agent-guide",
    ] {
        assert!(out.contains(needle), "--help lacks {needle:?}:\n{out}");
    }
    assert!(
        out.lines()
            .any(|l| l.trim_start().starts_with("agent-guide ")),
        "{out}"
    );
    assert!(
        !out.lines().any(|l| l.trim_start().starts_with("host ")),
        "the hidden host command is listed:\n{out}"
    );

    let (code, json_help, _) = run("qrec --json --help");
    assert_eq!((code, json_help), (0, out));

    let version = format!("qrec {}\n", env!("CARGO_PKG_VERSION"));
    assert_eq!(run("qrec --version"), (0, version.clone(), String::new()));
    assert_eq!(run("qrec version"), (0, version, String::new()));
    let (code, out, _) = run("qrec version --json");
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "{{\"ok\":true,\"data\":{{\"version\":\"{}\"}}}}\n",
            env!("CARGO_PKG_VERSION")
        )
    );

    let (code, out, _) = run("qrec clip split --help");
    assert_eq!(code, 0);
    assert!(out.contains("Usage: qrec clip split"), "{out}");
}

#[test]
fn completions_print_a_script() {
    let (code, out, err) = run("qrec completions bash");
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(out.contains("_qrec"), "{out}");
    let (code, out, _) = run("qrec completions fish --json");
    assert_eq!(code, 0);
    let reply: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(reply["data"]["shell"], "fish");
    assert!(
        reply["data"]["script"]
            .as_str()
            .unwrap()
            .contains("complete -c qrec")
    );
}

#[test]
fn project_commands_need_the_shell() {
    // Without qrec-shell (`Standalone`), an empty folder still answers its
    // queries, and changes report that this build can't run them.
    let dir = std::env::temp_dir().join(format!("qrec-parse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let env = RunEnv {
        cwd: Some(dir.clone()),
        err_tty: false,
    };
    let mut backend = Standalone::default();
    let mut run_in = |line: &str| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with_backend(words(line), &mut backend, &env, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let (code, out, err) = run_in("qrec clip split c7f3 --at 13f");
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.starts_with(
            "project commands can't run: this qrec build doesn't include its shell (qrec-shell)\n"
        ),
        "{err}"
    );

    let (code, out, _) = run_in("qrec --json status");
    assert_eq!(code, 0);
    let reply: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(reply["data"]["initialized"], json!(false));
    std::fs::remove_dir_all(&dir).unwrap();
}

// --------------------------------------------------------------- the tree

#[test]
fn command_tree_is_consistent() {
    Cli::command().debug_assert();
}

#[test]
fn every_command_and_argument_has_help() {
    fn walk(command: &clap::Command, path: &str, missing: &mut Vec<String>) {
        for arg in command.get_arguments() {
            let id = arg.get_id().as_str();
            if matches!(id, "help" | "version") {
                continue;
            }
            if arg
                .get_help()
                .is_none_or(|h| h.to_string().trim().is_empty())
            {
                missing.push(format!("{path} {id}"));
            }
        }
        for sub in command.get_subcommands() {
            let sub_path = format!("{path} {}", sub.get_name());
            if sub
                .get_about()
                .is_none_or(|a| a.to_string().trim().is_empty())
            {
                missing.push(sub_path.clone());
            }
            walk(sub, &sub_path, missing);
        }
    }
    let mut missing = Vec::new();
    walk(&Cli::command(), "qrec", &mut missing);
    assert!(missing.is_empty(), "no help text: {missing:?}");
}
