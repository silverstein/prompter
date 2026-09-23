# Prompter

Live script guidance for high-stakes conversations. A desktop teleprompter that tracks your voice against a known script, scrolling intelligently, pausing when the other person speaks, and producing a compliance report when you're done.

Built for pharmacists delivering MTM consultations. Works for any structured conversation: sales calls, presentations, onboarding scripts.

## Install

Download the latest `.zip` from [Releases](https://github.com/silverstein/prompter/releases), unzip it, and move `Prompter.app` to Applications. It needs macOS 13 or later on Apple silicon. The app isn't notarized yet, so macOS blocks the first launch: right-click the app and choose Open.

## How it works

1. Load a script (open file, paste from clipboard, or drag-and-drop).
2. Put the reading line just under your camera (`[` `]`), and set the column width (`,` `.`).
3. Hit **Start Session** (or press Space) and start speaking. The teleprompter follows your voice and holds the line you're reading at the reading line.
4. It waits at check-in points, and at branch points it follows whichever answer you read (or click one).
5. When you're done, click **End**. You get a compliance report and coaching insights, re-checked against the full recording.

## Voice tracking

Speech recognition runs on-device with Apple's recognizer, given a custom language model built from each script's own lines. Recognized words are aligned to the script word by word, with sound-alike matching for drug names. It follows skips, going back to re-read, and branch answers. Audio never leaves the machine. After the session, the recording is re-transcribed and re-aligned so the report reflects what was actually said, and then it's deleted.

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
| [ / ] | Move the reading line up / down |
| , / . | Narrower / wider text column |
| + / - | Font size |
| Up / Down | Previous / Next sentence |
| Click a line | Make it the current line |
| 1-9 | Jump to section |
| Tab / Enter | Choose a branch answer by hand |
| Cmd+O | Open file |
| Cmd+V | Paste script |
| H | Help and session options |
| Esc | End session |

## Build

Requires Rust, the Tauri CLI (`cargo install tauri-cli`), and Xcode command line tools (for the Swift speech helper).

```bash
# Build the app (Swift helper + Tauri bundle), and optionally install it
./scripts/build.sh --install

# Run tests (the core crate also builds and tests on Linux)
cargo test
```

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
- Lines not delivered, restarts and off-script stretches
- Coaching insights (pacing, coverage, pause discipline)

Reports use 0600 file permissions (PII-safe). Prompter is hidden from screenshots and screen sharing by default; you can change that in Session options (`H`).

## Architecture

```
prompter/
├── crates/
│   ├── core/               # Rust library (pure logic, no audio)
│   │   ├── script.rs       # .script.md parser
│   │   ├── align.rs        # Word-level alignment engine
│   │   ├── tracker.rs      # Live position, relocator, pauses and branches
│   │   ├── session.rs      # Speech-verified coverage and transcript
│   │   ├── realign.rs      # Post-session re-alignment of the recording
│   │   ├── compliance.rs   # Session report generator
│   │   └── coaching.rs     # Data-driven delivery analysis
│   └── app/                # Tauri v2 desktop app
│       ├── src/main.rs     # Tauri commands, speech helper, verification
│       └── ui/index.html   # Teleprompter UI (vanilla JS)
├── scripts/
│   ├── speech-recognizer.swift  # On-device speech helper (Apple Speech)
│   └── build.sh            # Build + install script
├── SPEC.md                 # .script.md format specification
└── INTEGRATION.md          # Integration guide for script sources
```

No API keys required.

## License

MIT
