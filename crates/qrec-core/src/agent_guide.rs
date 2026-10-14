//! `qrec agent-guide`: a concise usage guide for LLM agents. Owner: meta.
//!
//! Hand-written prose around tables generated from the code: exit codes
//! from [`ErrorCode`], the key ↔ CLI table from the default keymap, the
//! settings from `config::KEYS` and the compositor binds from
//! `keymap::NIRI_SNIPPET`, so the guide can't drift from them.

use std::fmt::Write as _;

use crate::config;
use crate::error::{ErrorCode, NOT_MY_SWAMP};
use crate::keymap;

/// Every error code, in exit-code order.
const CODES: [ErrorCode; 11] = [
    ErrorCode::Invalid,
    ErrorCode::CaptureFailed,
    ErrorCode::Unavailable,
    ErrorCode::Io,
    ErrorCode::Internal,
    ErrorCode::Usage,
    ErrorCode::NotProject,
    ErrorCode::ConfirmationRequired,
    ErrorCode::MissingDependency,
    ErrorCode::Busy,
    ErrorCode::NotFound,
];

/// What an agent should do about each code.
fn advice(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::Usage => "bad arguments; read `qrec <command> --help`",
        ErrorCode::NotProject => "not a project folder; see \"Project folder\"",
        ErrorCode::ConfirmationRequired => {
            "destructive; nothing happened. Read `error.details`, re-run with `--yes` only if the loss is intended"
        }
        ErrorCode::MissingDependency => "a tool or GStreamer element is missing; run `qrec doctor`",
        ErrorCode::Busy => {
            "recording, finalizing or exporting, or the owning host isn't responding; wait or `qrec record stop`"
        }
        ErrorCode::NotFound => "unknown id, output or device; list them again",
        ErrorCode::Invalid => "impossible edit or invalid project; the message says why",
        ErrorCode::CaptureFailed => "a source failed to start; the hint names it",
        ErrorCode::Unavailable => "needs the GUI (seek, play, pause) or a selection it holds",
        ErrorCode::Io => "a file or process failed; see the message and `.qrec/logs/`",
        ErrorCode::Internal => "a Qrec bug; report it with the message",
    }
}

/// The bindings the key ↔ CLI table shows: the hot loop and the edits.
const KEY_TABLE: &[&str] = &[
    "record.toggle",
    "record.over",
    "take.discard-last",
    "cancel",
    "clip.split",
    "clip.trim-start",
    "clip.trim-end",
    "clip.nudge-start",
    "clip.move-left",
    "clip.remove",
    "autocut.run",
    "autocut.preview",
    "history.undo",
    "history.redo",
    "frame.save",
    "export",
    "bin.toggle",
];

const INTRO: &str = "\
# Qrec agent guide

Qrec records screens, cameras, a mic and system audio as layered *takes*,
places each take on a one-track timeline as *clips*, edits them (split, trim,
move, remove, autocut silences) and exports video. The GUI and the CLI run
the same commands, and every edit is undoable. Always pass `--json`.

## Project folder

- A project is a folder with `qrec.json`. Commands use `--project DIR`, else
  the current directory, which must hold `qrec.json` or be strictly empty
  (hidden files such as `.git` count). An empty folder becomes a project with
  `qrec init` or on the first change.
- Inside a project's subfolder, commands exit 3 (`not_project`) and name the
  project: pass `--project DIR` or run them there.
- Elsewhere, without `--project`, commands use the single running Qrec (so
  compositor key binds work from `$HOME`; several running ones are a usage
  error that lists them). With none running they exit 3 with
";

const OUTPUT: &str = "\
- These work anywhere: `--help`, `version`, `doctor`, `sources list`, `schema`,
  `keys`, `agent-guide`, `config`, `completions`.
- Never write inside `takes/` or `.qrec/`. Read `qrec.json` freely (`qrec
  schema` prints its JSON Schema); change it with commands, which keep the
  undo history. Exports go to `exports/`, frames to `frames/`; nothing is
  ever overwritten without `--yes`.

## Output contract

- With `--json`, stdout ends with exactly one JSON object:
  `{\"ok\":true,\"data\":…}` or
  `{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\",\"details\"}}`.
- Long commands (`export`, `record … --wait`, autocut while it analyzes)
  may first print JSON lines such as
  `{\"event\":\"progress\",\"phase\":\"render\",\"done\":1234,\"total\":5678,\"etaSecs\":42}`.
  The last stdout line is the result. Notices go to stderr (as
  `{\"event\":\"notice\",…}` lines under `--json`); never parse stderr.
- Changes return `data.seq` (the new history position), `data.created` and
  `data.removed` (ids), `data.message` (one line) and `data.result` (a
  command-specific view, or null). Queries return their rows as `data`.

## Exit codes

| Exit | Code | Meaning and what to do |
| --- | --- | --- |
| 0 | — | ok |
";

const BODY: &str = "
## Ids, times and frames

- Takes `take-0001` (numbers are never reused), clips `c7f3`, layers `L1`,
  audio tracks `A1`. Never guess ids: read them from `qrec clip list --json`
  (`[{id, take, in, out, start, duration}]`), `qrec take list --json`
  (`[{id, status, recordedAt, durationFrames, clips, unused, error}]`) and
  `qrec status --json`. New ids arrive in `data.created`; a split keeps the
  left part's id.
- Positions are integer frames at the canvas fps. A clip is the half-open
  range `[in, out)` of its take; the timeline plays clips back to back
  (`start` = timeline frame of the clip's first frame).
- Time arguments: `412f`, `13.7s`, `00:00:13:21` (HH:MM:SS:FF at the nominal
  fps) or `00:00:13.700`. A bare number is a usage error. Signed values
  (`+1f`, `-2s`) move a trim edge or seek by a delta. Clip times count from
  the clip's start unless `--timeline` is given.

## Workflows

Set up (screens and devices come from `qrec sources list --json`):

```sh
qrec init --canvas 1920x1080@30
qrec scene layer add screen --output DP-4
qrec scene layer add camera --device /dev/video0
qrec scene audio add mic
qrec scene audio add system
```

`qrec scene layer set L2 --rect 1500,760,384,288` changes the project scene,
which new takes copy; add `--take take-0003` to change only that take's
layout, or run `qrec scene apply --to-all-takes` to copy the project layout
into every take.

Record. Without a running Qrec, `record start` spawns a background
recorder and returns once capture runs; `record stop`, from any shell,
finalizes the take and places it on the timeline:

```sh
qrec record start --json       # data.result: {take, mode, files}
qrec record stop --json        # data.result: {take, status, durationFrames, placed, clips}
qrec record cancel             # abort; the files stay and the take goes to the bin
qrec record start --over c7f3  # replace contiguous clips with the new take
qrec take discard-last         # take the newest take's clips off the timeline
```

`--wait` keeps `record start` attached until the take is finalized (stop it
from another shell), streaming progress lines.

Takes shorter than 10 frames become `tooShort` and go to the bin. For tests
set `QREC_CAPTURE_BACKEND=fake`: sources become `FAKE-1`, `FAKE-2`,
`/dev/fake0`, `alsa_input.fake` and `alsa_output.fake.monitor`, and no portal
dialog appears.

Edit:

```sh
qrec clip list --json
qrec clip split c7f3 --at 13.7s         # or: qrec clip split --timeline --at 00:01:02:10
qrec clip trim c7f3 --start 12f --end -1s
qrec clip move c8a1 --by -1             # or --to 0, --before c7f3, --after c7f3
qrec clip remove c9b2                   # its take stays (qrec take list --unused)
qrec clip add take-0003 --after c7f3
qrec undo                               # qrec redo, qrec history --json
```

Autocut silences. Preview first; applying is one undoable step:

```sh
qrec autocut --dry-run --json   # data.result: {clips: [{clip, kept, removedFrames, silences}], skipped}
qrec autocut                    # every clip; --clips c7f3,c8a1 for some
qrec autocut --track A1 --threshold-db -40 --padding-ms 150
```

Export and stills (canvas size and fps):

```sh
qrec export --json              # progress lines, then data.result: {path, bytes, frames}
qrec export --format webm --quality high --out ~/Videos/talk.webm
qrec frame save --at 00:00:13:21 --json   # data.result: {path}
```

## Dry runs and confirmation

`qrec autocut --dry-run` plans without changing anything. Destructive
commands are their own dry run: without `--yes` they do nothing, exit 4 with
`confirmation_required`, say what would be lost in `error.message` and list
it in `error.details` (files and sizes, history entries lost, clips moved).
Pass `--yes` only when that loss is intended:

- `qrec take purge <take>…` deletes unused takes' media (not undoable);
- `--out PATH` on `export` or `frame save` when PATH exists (overwrite);
- `qrec history clear`, `qrec history restore <seq>`;
- `qrec project set --canvas @<fps>` on a project with takes (clip bounds
  are re-quantized).

## Undo, busy and hosts

- Every change returns `data.seq`. `qrec undo [N]` / `qrec redo [N]` move
  through the history; `qrec history --json` lists `{seq, ts, label,
  current}`; `qrec history restore <seq> --yes` jumps (it also rebuilds a
  corrupt `qrec.json`). Undo never deletes media: undoing a recording
  removes its clip and the take goes to the bin.
- While a take records or finalizes, edits exit 6 (`busy`). One export runs
  at a time, and recording is refused while it runs. When the GUI or a
  background recorder owns the project, the CLI talks to it automatically.
  `seek`, `play` and `pause` need the GUI.
";

/// The guide text (Markdown).
pub fn agent_guide() -> String {
    let mut out = String::new();
    out.push_str(INTRO);
    let _ = writeln!(out, "  \"{NOT_MY_SWAMP}\".");
    out.push_str(OUTPUT);
    for code in CODES {
        let _ = writeln!(
            out,
            "| {} | `{}` | {} |",
            code.exit_code(),
            code.as_str(),
            advice(code)
        );
    }
    out.push_str(BODY);
    out.push_str(&key_table());
    out.push_str(&settings());
    out
}

/// "Keys and their CLI form", from the default keymap.
fn key_table() -> String {
    let map = keymap::resolve("");
    let mut out = String::from(
        "
## Keys and their CLI form

Every key in the app is a command; these have a CLI form (`qrec keys --json`
lists all of them, with ids for `~/.config/qrec/keys.toml`):

| Key | Action | CLI |
| --- | --- | --- |
",
    );
    for id in KEY_TABLE {
        if let Some(b) = map.binding(id) {
            let keys: Vec<String> = b.keys.iter().map(keymap::KeyChord::display).collect();
            let _ = writeln!(
                out,
                "| {} | {} | `{}` |",
                keys.join(" ").replace('|', "\\|"),
                b.label,
                b.cli.unwrap_or("—")
            );
        }
    }
    let _ = write!(
        out,
        "
Global binds for recording while another app has focus (niri; `qrec doctor`
prints them too):

```kdl
{}
```
",
        keymap::NIRI_SNIPPET
    );
    out
}

/// The settings list, from `config::KEYS`.
fn settings() -> String {
    let mut out = String::from(
        "
## Settings

`qrec config` lists `~/.config/qrec/config.toml`; `qrec config get KEY`
reads one value and `qrec config set KEY VALUE` writes it, keeping comments.
They are per user: defaults for new projects, the encoder and the GUI:

",
    );
    for key in config::KEYS {
        let _ = writeln!(out, "- `{}`: {}", key.key, key.values);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_code_is_listed_once() {
        let guide = agent_guide();
        for code in CODES {
            let row = format!("| {} | `{}` |", code.exit_code(), code.as_str());
            assert_eq!(guide.matches(&row).count(), 1, "{row}");
        }
        let unique: std::collections::BTreeSet<&str> = CODES.iter().map(|c| c.as_str()).collect();
        assert_eq!(unique.len(), CODES.len());
        // The exhaustive match in `advice` fails to compile when a code is
        // added; keep CODES in step with it.
        assert_eq!(CODES.len(), 11);
    }

    #[test]
    fn the_tables_come_from_the_code() {
        let guide = agent_guide();
        let map = keymap::resolve("");
        for id in KEY_TABLE {
            let b = map.binding(id).unwrap_or_else(|| panic!("no binding {id}"));
            assert!(guide.contains(b.cli.unwrap()), "{id}");
        }
        assert!(guide.contains("| r | Start / stop recording | `qrec record toggle` |"));
        assert!(guide.contains(keymap::NIRI_SNIPPET));
        for key in config::KEYS {
            assert!(guide.contains(&format!("`{}`", key.key)), "{}", key.key);
        }
        assert!(guide.contains(NOT_MY_SWAMP));
    }

    #[test]
    fn commands_in_the_guide_exist() {
        // Every `qrec <word>` names a top-level command of the CLI (A§10, SPEC §14).
        const COMMANDS: &[&str] = &[
            "init",
            "status",
            "doctor",
            "sources",
            "scene",
            "record",
            "take",
            "clip",
            "autocut",
            "undo",
            "redo",
            "history",
            "export",
            "frame",
            "seek",
            "play",
            "pause",
            "project",
            "config",
            "keys",
            "schema",
            "agent-guide",
            "version",
            "completions",
        ];
        let guide = agent_guide();
        for (i, _) in guide.match_indices("qrec ") {
            let word: String = guide[i + 5..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            if word.is_empty() {
                continue;
            }
            assert!(
                COMMANDS.contains(&word.as_str()),
                "unknown command `qrec {word}`"
            );
        }
    }

    #[test]
    fn snapshot_agent_guide() {
        insta::assert_snapshot!("agent_guide", agent_guide());
    }
}
