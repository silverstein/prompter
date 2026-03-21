# Prompter

Live script guidance for high-stakes conversations. A desktop teleprompter that tracks your voice against a known script, scrolling intelligently, pausing when the other person speaks, and producing a compliance report when you're done.

Built for pharmacists delivering MTM consultations. Works for any structured conversation: sales calls, presentations, onboarding scripts.

## How it works

1. Load a script (open file, paste from clipboard, or drag-and-drop)
2. Hit **Start Session** (or press Space)
3. Start speaking — the teleprompter follows your voice
4. It pauses at check-in points and waits for you to continue
5. At branch points, tap YES/NO to choose a path
6. When done, get a compliance report + coaching insights

## Tracking tiers

| Tier | What | How |
|------|------|-----|
| **Tier 1 (VAD)** | Scrolls when you speak, pauses on silence | Energy-based voice activity detection at ~10Hz |
| **Tier 2 (Whisper)** | Corrects scroll position using speech recognition | Transcribes on silence boundaries, fuzzy-matches against script. Activates automatically if the whisper model exists. |

## Script format

Scripts are annotated markdown (`.script.md`). Frontmatter is optional — paste plain text and it works.

```markdown
---
title: MTM Consultation
type: pharmacy-consultation
estimated_duration: 18min
---

# Intro

Hi, thanks for meeting with me today.

> PAUSE: Does that sound helpful to you?

# Findings

Let me walk you through what I found.

> BRANCH: Would you like me to get this plan started?
>> YES
Great. Let me get that organized for you.
>> NO
No problem. May I share with your doctor?
```

See [SPEC.md](SPEC.md) for the full format specification.

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| Space | Start / Pause / Resume |
| Up / Down | Previous / Next sentence |
| 1-9 | Jump to section |
| Tab / Enter | Cycle / select branch options |
| + / - | Font size |
| Cmd+O | Open file |
| Cmd+V | Paste script |
| H | Help overlay |
| Esc | End session |

## Build

Requires Rust, Cargo, and the Tauri CLI (`cargo install tauri-cli`).

```bash
# Build and install
./scripts/build.sh --install

# Or manually
export CXXFLAGS="-I$(xcrun --show-sdk-path)/usr/include/c++/v1"
cargo tauri build --bundles app
cp -Rf target/release/bundle/macos/Prompter.app /Applications/

# Run tests
cargo test
```

### Whisper model (optional, for Tier 2 tracking)

If you have [Minutes](https://github.com/anthropics/minutes) installed, Prompter uses the same whisper model automatically (`~/.config/minutes/models/ggml-tiny.bin`). Otherwise, Tier 1 VAD-only tracking works without any model.

## RxVIP integration

The RxVIP pharmacy ecosystem can export consultations directly to Prompter:

- **Watched folder**: Scripts saved to `~/meetings/scripts/` appear in Prompter's list
- **URL scheme**: `prompter://open?consultation_id=abc-123` opens Prompter with the consultation loaded
- **Clipboard**: Copy markdown from the web app, Cmd+V in Prompter

See [INTEGRATION.md](INTEGRATION.md) for the full integration guide.

## After a session

Prompter saves a compliance report to `~/meetings/consults/` with:
- Sections covered / skipped
- Time per section
- Pause points reached
- Branch decisions taken
- Adherence percentage
- Coaching insights (pacing, coverage, pause discipline)

Reports use 0600 file permissions (PII-safe).

## Architecture

```
prompter/
├── crates/
│   ├── core/               # Rust library
│   │   ├── script.rs       # .script.md parser
│   │   ├── audio.rs        # Streaming audio capture (cpal, 16kHz)
│   │   ├── vad.rs          # Voice activity detection
│   │   ├── transcribe.rs   # Streaming whisper (whisper-rs)
│   │   ├── align.rs        # Fuzzy text alignment engine
│   │   ├── compliance.rs   # Session report generator
│   │   └── coaching.rs     # Data-driven delivery analysis
│   └── app/                # Tauri v2 desktop app
│       ├── src/main.rs     # Tauri commands + audio thread
│       └── ui/index.html   # Teleprompter UI (vanilla JS)
├── SPEC.md                 # .script.md format specification
├── INTEGRATION.md          # Integration guide for script sources
└── scripts/build.sh        # Build + install script
```

37 tests. 19MB app. No API keys required.

## License

MIT
