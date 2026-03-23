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

# Manual build (if build.sh gives trouble)
export CXXFLAGS="-I$(xcrun --show-sdk-path)/usr/include/c++/v1"
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

The `CXXFLAGS` export is needed for whisper-rs C++ compilation on macOS. The `.cargo/config.toml` sets `MACOSX_DEPLOYMENT_TARGET=13.0`.

## Architecture

Cargo workspace with two crates:

### `prompter-core` (library)
Pure-logic crate with no Tauri dependency. Feature-gated audio:
- **`script.rs`** — Parses `.script.md` format (YAML frontmatter + annotated markdown). Splits body into `Section > Element > Sentence`. Handles variable substitution, PAUSE/BRANCH directives.
- **`align.rs`** — Fuzzy text alignment engine. Matches Whisper transcription output against script sentences using bigram similarity (Dice coefficient) over a sliding window around the cursor position.
- **`compliance.rs`** — Generates post-session reports (sections covered, time per section, adherence %, branch decisions). Writes markdown files to `~/meetings/consults/` with 0600 permissions.
- **`coaching.rs`** — Data-driven delivery analysis (no LLM). Analyzes pacing, coverage, pause discipline, section balance. Produces severity-ranked insights.
- **`transcribe.rs`** — Streaming Whisper wrapper (Tier 2). Keeps WhisperContext alive across audio chunks to avoid model reload penalty. Feature-gated behind `whisper`.
- **`error.rs`** — Error types (`ParseError`, `PrompterError`).

Audio capture and VAD are re-exported from `minutes-core` (sibling repo at `../minutes/crates/core`), not implemented here. The `audio` feature enables `minutes-core` streaming; the `whisper` feature adds `whisper-rs`.

### `prompter-app` (Tauri v2 binary)
Desktop shell. Converts core types to JSON-serializable structs for the frontend, manages the audio thread, and exposes Tauri commands:
- `load_script` / `parse_script_text` — Parse from file or clipboard text
- `start_audio` / `stop_audio` — Spawn/kill dedicated audio thread (cpal stream is `!Send`)
- `save_compliance` / `get_coaching` — Post-session reporting
- `load_settings` / `save_settings` — Persist to `~/.prompter/settings.json`
- `list_available_scripts` — Watch `~/meetings/scripts/` directory
- `set_always_on_top` — Window management
- Deep link handling (`prompter://open?file=...` or `prompter://open?consultation_id=...`)

The audio thread runs a loop: cpal → VAD (Tier 1, ~10Hz) → optional Whisper transcription on silence boundaries (Tier 2) → alignment correction. Events emitted to frontend: `vad`, `align`, `tier2-ready`, `audio-started`, `vad-error`.

### UI (`crates/app/ui/index.html`)
Single-file vanilla JS/HTML/CSS. No framework, no build step. Communicates with Rust via `window.__TAURI__.core.invoke()` and `window.__TAURI__.event.listen()`.

## Key conventions

- **Feature gates**: `audio` and `whisper` are Cargo features. The app crate enables both; core can be used without audio for parsing/testing.
- **minutes-core dependency**: Referenced as a local path (`../../../minutes/crates/core`). The Minutes repo must be cloned as a sibling for audio features to compile.
- **Two-tier tracking**: Tier 1 (VAD-only) always works. Tier 2 (Whisper alignment) activates automatically if `~/.config/minutes/models/ggml-tiny.bin` exists.
- **Content protection**: `contentProtected: true` in tauri.conf.json prevents screen capture of the window.
- **Script format**: `.script.md` — annotated markdown with optional YAML frontmatter, `> PAUSE:` and `> BRANCH:` directives. See SPEC.md.
- **File paths**: Scripts in `~/meetings/scripts/`, compliance reports in `~/meetings/consults/`, settings in `~/.prompter/settings.json`.
- **Test fixtures**: `crates/core/tests/fixtures/mtm-consultation.script.md` — real MTM consultation script used in integration tests.
