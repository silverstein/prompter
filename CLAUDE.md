# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is this

Prompter is a macOS desktop teleprompter that tracks the speaker's voice against a known script. It scrolls intelligently, pauses when the other person speaks, supports conditional branching, and produces compliance reports with coaching insights after each session. Built for pharmacists doing MTM consultations; works for any structured conversation.

## Build and run

Requires Rust, Cargo, and Tauri CLI (`cargo install tauri-cli`).

```bash
# Build the macOS .app bundle (from repo root)
./scripts/build.sh

# Build and install to /Applications
./scripts/build.sh --install

# Manual build: compile the Swift helper to
# crates/app/binaries/speech-recognizer-<host triple> first (Tauri bundles it
# as an external binary via tauri.macos.conf.json), then:
cd crates/app && cargo tauri build --bundles app

# Dev mode (hot reload)
cd crates/app && cargo tauri dev

# Run all tests
cargo test

# Run a single test
cargo test -p prompter-core test_name

# Run integration tests (uses fixture script)
cargo test -p prompter-core --test real_script_test
```

The `.cargo/config.toml` sets `MACOSX_DEPLOYMENT_TARGET=13.0`. The core crate has no platform dependencies, so `cargo test -p prompter-core` also runs on Linux.

## Architecture

Cargo workspace with two crates:

### `prompter-core` (library)
Pure-logic crate with no Tauri dependency. Feature-gated audio:
- **`script.rs`** — Parses `.script.md` format (YAML frontmatter + annotated markdown). Splits body into `Section > Element > Sentence`. Handles variable substitution, PAUSE/BRANCH directives.
- **`align.rs`** — Word-level alignment engine: a bounded, monotonic local DP over words around the cursor, with sound-alike matching for misheard drug names ("metro pro law" → metoprolol) and split/joined words. `locate_global` searches the whole script for the tracker's relocator.
- **`tracker.rs`** — `ScriptTracker`: drives the aligner from partial/final speech updates, handles pause/branch state, and runs the whole-script relocator (moves the cursor far only after 3 progressing agreements).
- **`session.rs`** — `SessionRecorder`: speech-verified coverage, transcript, repeats and off-script stretches; accepts the post-session re-alignment.
- **`realign.rs`** — Post-session pass: global word alignment of the full-recording transcript against the whole script (per-sentence delivered / omitted, off-script words).
- **`compliance.rs`** — Generates post-session reports (sections covered, time per section, adherence %, branch decisions). Writes markdown files to `~/meetings/consults/` with 0600 permissions.
- **`coaching.rs`** — Data-driven delivery analysis (no LLM). Analyzes pacing, coverage, pause discipline, section balance. Produces severity-ranked insights.
- **`error.rs`** — Error types (`ParseError`, `PrompterError`).

The core crate does no audio: speech comes from the app's Swift helper (below). The optional `sherpa` feature is the planned portable (Windows/Linux) speech engine.

### `prompter-app` (Tauri v2 binary)
Desktop shell. Converts core types to JSON-serializable structs for the frontend, runs the speech helper, and exposes Tauri commands:
- `load_script` / `parse_script_text` — Parse from file or clipboard text
- `init_tracking` / `finish_tracking` / `clear_tracking` — Session lifecycle (tracker, recorder, report, post-session check)
- `start_speech` / `stop_speech` — Spawn/stop the Swift speech helper
- `set_tracking_position` / `choose_branch` — Manual re-anchoring (arrows, clicks, branch buttons)
- `save_compliance` / `get_coaching` — Post-session reporting
- `load_settings` / `save_settings` — Persist to `~/.prompter/settings.json`
- `list_available_scripts` — Watch `~/meetings/scripts/` directory
- `set_always_on_top` / `set_hide_from_screen_share` — Window management (the latter synced with the tray item)
- Deep link handling (`prompter://open?file=...` or `prompter://open?consultation_id=...`)

Live speech comes from the Swift helper `scripts/speech-recognizer.swift` (Apple on-device Speech), bundled next to the app binary. Per session it gets `--script` (builds a custom language model from the script's spoken lines, macOS 14+), `--record` (16 kHz CAF for verification), and optionally `--system-audio` (ScreenCaptureKit call audio → `{"other": bool}`). After the session, `finish_tracking` writes the live report, then a background pass runs the helper in `--file` mode on the recording, re-aligns it (`realign.rs`), rewrites the report, deletes the audio (unless `keep_session_audio`), and emits `verification-complete`. Events: `speech`, `track-update`, `other-party`, `speech-status`, `speech-error` (fatal → timer fallback), `speech-warning` (non-fatal), `verification-complete` / `verification-failed`.

The helper only has speech permission when launched by Prompter.app; running it from a terminal reports `speech_auth_not_determined`.

### UI (`crates/app/ui/index.html`)
Single-file vanilla JS/HTML/CSS. No framework, no build step. Communicates with Rust via `window.__TAURI__.core.invoke()` and `window.__TAURI__.event.listen()`.

## Key conventions

- **No sibling repos**: Prompter builds on its own (it no longer depends on `minutes-core`).
- **Diagnostics**: `open -n -W -a Prompter --args --transcribe-file <audio> --script <md> --out <json>` runs the post-session check on any recording (speech access belongs to the app, so it can't run from a shell). Each live run's recognizer stream is in `~/.prompter/recording.jsonl`; replay it with `cargo run -p prompter-core --example replay`.
- **Content protection**: `contentProtected: true` in tauri.conf.json prevents screen capture of the window.
- **Script format**: `.script.md` — annotated markdown with optional YAML frontmatter, `> PAUSE:` and `> BRANCH:` directives. See SPEC.md.
- **File paths**: Scripts in `~/meetings/scripts/`, compliance reports in `~/meetings/consults/`, settings in `~/.prompter/settings.json`.
- **Test fixtures**: `crates/core/tests/fixtures/mtm-consultation.script.md` — real MTM consultation script used in integration tests.
