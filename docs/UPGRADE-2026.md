# Prompter Upgrade Plan (2026)

Status: active. Owner: Mat. Author of this draft: assessment + SOTA research run 2026-06-29.

This supersedes the speech-pipeline sections of `VISION.md`. It records where Prompter
stands after the `SFSpeechRecognizer` pivot, the 2026 state of the art for the technology
underneath it, the decisions taken, and a phased plan. The parser / compliance / coaching
core is sound and carried forward unchanged; the live tracking runtime is treated as a
greenfield rebuild behind a provider abstraction.

## 1. The reframe that drives every decision

Prompter aligns recognized speech to a **known script**. That inverts normal ASR priorities:

- We do **not** need best-in-class word-error-rate. The reference text collapses the problem
  to "where are we in this script", so high WER is tolerable. The recognizer does not need to
  spell "tirzepatide" correctly, only land near the script token so the cursor advances.
- The genuinely hard sub-problems are therefore **(1) low-latency streaming partial hypotheses**
  and **(2) "pause when the other party speaks"** (an audio-capture topology problem), not raw
  transcription accuracy.

Consequence: forced alignment (aeneas / MFA / WhisperX) is the wrong tool for the live loop
(batch-only, intolerant of paraphrase/skips/branches). The right tool is an online fuzzy
cursor-matcher, which is exactly what `crates/core/src/align.rs` already implements.

## 2. Current state (assessment summary)

Keep as-is (clean, tested, production-grade pure logic):
- `script.rs` parser + the `.script.md` DSL.
- `compliance.rs` (PII-aware, 0600 reports) and `coaching.rs` (heuristic floor).

Un-orphan and make canonical:
- `align.rs` (`AlignmentEngine`: bigram-Dice windowed match, forward bias + small back-jumps,
  ad-lib detection, 8 passing tests). It is currently **orphaned** -- the SFSpeechRecognizer
  pivot deleted its only caller and the live runtime now uses a naive "last 6 words" substring
  matcher in `crates/app/ui/index.html`. The Rust engine is the better algorithm and the
  SOTA-recommended shape; the JS matcher is a regression.

Rebuild (left transitional by the pivot):
- Live capture -> ASR -> alignment -> pause/branch -> compliance integration. The pivot made it
  macOS-only (`SFSpeechRecognizer` Swift sidecar), broke auto-pause and speech-driven branching
  under live tracking (the WPM timer that enforced pauses does not run in audio mode), made
  compliance cursor-position-based rather than speech-verified, and persists no transcript.
- Robustness bugs to fix in passing: Swift stderr is ignored (permission denial fails silently);
  a UTF-8 byte-slice panic risk in the stdout reader; non-prompt subprocess teardown; CSP disabled.

Drop or properly adopt:
- `minutes-core` is currently **vestigial** -- since the pivot, prompter consumes only its cpal
  capture + energy VAD and nothing routes through it at runtime, yet it drags the whisper-rs C++
  build and requires the minutes repo as a sibling clone. See decisions below.

## 3. Decisions

### D1. Cross-platform is a hard requirement -> provider abstraction, portable engine baseline
Pharmacy desktops are mostly Windows. Therefore no macOS-only code sits on the critical path.
All ASR goes behind a `SpeechProvider` trait. The baseline implementation is a **portable,
on-device streaming engine** (see D4). Apple `SpeechAnalyzer` is an *optional* macOS fast-path
behind the same trait, never load-bearing.

### D2. Call topology is unknown -> abstract it, do not bet on it
"Other party is speaking" detection goes behind its own abstraction (`OtherPartyDetector`).
Default implementation: **dual-stream capture** (operator mic + system-audio loopback), which
makes "other party speaking" a trivial VAD on stream B with no diarization model. This covers
VoIP softphones on all three OSes (macOS Core Audio taps / Windows WASAPI loopback / Linux
PipeWire monitor). Fallback for a physical handset/speakerphone in the room (patient audio
arrives acoustically through the same mic): single-stream VAD now, optional streaming
diarization later. This is a later phase and does not block the core, so the uncertainty costs
nothing now.

### D3. On-device first, cloud as an optional BAA-gated tier
On-device keeps consult audio out of any HIPAA Business Associate Agreement surface, and the
known-script reframe makes on-device accuracy sufficient. A cloud tier (Deepgram Nova-3 Medical
with drug-name keyterm boosting, word timestamps, self-host/VPC option) stays a pluggable
`SpeechProvider` implementation, built only if real consult audio proves on-device insufficient.
Never send consult audio to a provider without a signed BAA.

### D4. ASR engine selection
- Portable baseline (cross-platform, on-device): **sherpa-onnx streaming Zipformer transducer**
  via the first-party Apache-2.0 Rust crate. True ~320ms streaming, word-onset timestamps
  (pair with VAD for word ends), **hotword/contextual biasing toward the script's drug names**,
  CPU-only on Mac/Windows/Linux from one codebase, ~70MB int8.
- macOS fast-path (optional): **Apple `SpeechAnalyzer` / `SpeechTranscriber`** (macOS 26), the
  upgrade from the current `SFSpeechRecognizer`. On-device, long-form, low-latency, and crucially
  native word-level `audioTimeRange` with a volatile-then-final result split that maps directly
  onto the cursor. Costs: no custom-vocabulary API (mitigated by the known-script reframe) and
  Apple-only.
- `minutes-core` is the in-house alternative for whisper/parakeet engines; it is now CI-tested on
  Linux/Windows/macOS and exposes `StreamingWhisper::{feed,finalize,reset}` (matching the trait
  shape). We adopt it behind `SpeechProvider` only if we want its whisper/parakeet backends; we
  do not keep it as a vestigial dependency.

### D5. Alignment stays online fuzzy cursor-matching (not forced alignment)
Keep `AlignmentEngine`'s windowed bigram-Dice approach. Improvements: advance the cursor only on
**stabilized/final** tokens (de-flicker), use **word timestamps** as anchors when the provider
supplies them, add an **SBERT semantic fallback** for paraphrase, and a **gated small-LLM resync
at pauses** (the "return a verbatim 3-5 word quote, then string-match it back" trick) for hard
cases and branch reconciliation. Reserve real forced alignment (wav2vec2 / whisperX-style) for an
**offline** post-session word-timing map feeding the compliance artifact.

### D6. Compliance becomes speech-verified and checklist-driven
Adopt the contact-center copilot pattern (Balto AI-checklist / Sedric disclosure-detection as
templates): continuous cheap classifiers over the recognized stream, a **trigger-gated** LLM/RAG
call over a **prompt-cached** CMR/CMS Part D + OBRA-90 required-elements playbook, auto-checking a
checklist as elements are covered, producing an exam-ready report. Live clinical
required-element tracking is a genuine product whitespace (ambient scribes only document; no one
ships live MTM coverage tracking) and ties directly back to SynapseRx.

## 4. Target architecture

```
              SpeechProvider (trait)  feed(samples)->Option<SpeechUpdate>; finalize(); reset()
 mic (cpal) ->  - portable: sherpa-onnx streaming Zipformer (hotword-biased to script drug names)
              - macOS fast-path: Apple SpeechAnalyzer (Swift sidecar, word audioTimeRange)
              - optional cloud (BAA): Deepgram Nova-3 Medical
                         |  SpeechUpdate { text, words[], is_final }
                         v
   ScriptTracker (Rust) -- wraps AlignmentEngine; advance on final, peek on partial (de-flicker);
                           word-timestamp anchors; SBERT fallback; LLM resync at pauses
                         |  TrackUpdate { position, state(Speaking|AtPause|AtBranch), confidence }
                         v
   UI: scroll / highlight + PAUSE enforcement + speech-driven branch selection
                         |
   compliance: trigger-gated checklist over prompt-cached CMR/OBRA playbook -> exam-ready report
                         ^
 system loopback (stream B) -> VAD -> "other party speaking" -> auto-pause   (D2, topology-dependent)
```

Latency rule: drive the cursor from partial/volatile hypotheses, commit on final. Never wait
for `isFinal` to scroll. Model choice is the latency budget; alignment compute is negligible
(~1-10ms).

## 5. ASR options considered (for the record)

| Option | Streaming | Word timestamps | On-device (no DC GPU) | Rust | Cross-platform | License | Verdict |
|---|---|---|---|---|---|---|---|
| sherpa-onnx streaming Zipformer | true ~320ms | word-onset | CPU Mac/Win/Linux | first-party crate | yes | Apache-2.0 | **baseline** |
| Apple SpeechAnalyzer | volatile/final | native audioTimeRange | Mac ANE | Swift sidecar | Mac-only | proprietary (free) | **macOS fast-path** |
| Kyutai STT | true 0.5s | native + semantic VAD | MLX Mac / GPU | Rust/Candle | partial | CC-BY-4.0 | watch |
| Deepgram Nova-3 Medical | ~150-300ms | yes + confidence | no (cloud, BAA) | HTTP/WS | yes | commercial | **cloud tier** |
| Parakeet-TDT-0.6b-v2/v3 | no (full-utt) | richest native | onnx-asr/MLX | via ort | yes | CC-BY-4.0 | if chunked OK |
| Whisper family | no (re-decode) | weak (+-500ms) | yes | whisper-rs | yes | MIT | avoid for live |
| Moonshine v2 | true 50-258ms | none | excellent | none | yes | MIT | disqualified (no word ts) |

## 6. Phased plan

- **Phase 1 (pure Rust, this VM, no Mac needed): provider trait + un-orphan the aligner.**
  Add `speech.rs` (`SpeechProvider` trait, `SpeechUpdate`, `RecognizedWord`, a `MockSpeechProvider`)
  and `tracker.rs` (`ScriptTracker`: flatten a `Script` into an indexed timeline, wrap
  `AlignmentEngine`, advance on final + peek on partial to de-flicker, report position + state
  incl. reaching a Pause/Branch). Add a non-mutating `peek` to `AlignmentEngine`. All CI-testable.
- **Phase 2: speech-driven pause + branch.** Enforce PAUSE state in the tracker; score the opening
  lines of each branch option as parallel candidate windows; commit on margin; LLM resync for
  ambiguous branches.
- **Phase 3: portable engine.** Implement `SpeechProvider` over sherpa-onnx (hotword-biased to the
  script's drug list); wire cpal capture in the Tauri backend (in-process, no sidecar) on
  Win/Linux/Mac. Keep the Apple SpeechAnalyzer sidecar as the macOS fast-path behind the trait.
- **Phase 4: dual-stream audio + auto-pause.** `OtherPartyDetector` with system-loopback per-OS;
  pending real call-topology data (D2).
- **Phase 5: speech-verified compliance + checklist.** Persist the recognized transcript; derive
  coverage from alignment evidence, not cursor position; trigger-gated checklist over the
  CMR/OBRA playbook; exam-ready report.
- **Phase 6: hardening.** Provider error propagation, prompt teardown, offline forced-alignment
  word-timing map for the report, CSP, cross-platform CI.

## 7. References (selected, from the 2026 SOTA research)

- Open ASR Leaderboard: arXiv 2510.06961. Whisper is batch-by-construction: gladia.io/blog/what-is-openai-whisper.
- sherpa-onnx (k2-fsa) streaming transducer + Rust crate + hotword biasing: github.com/k2-fsa/sherpa-onnx, docs.rs/sherpa-onnx.
- Apple SpeechAnalyzer (WWDC25 session 277) word audioTimeRange: developer.apple.com/documentation/Speech/bringing-advanced-speech-to-text-capabilities-to-your-app.
- Live teleprompter fuzzy-cursor prior art: LiveKit Nemotron teleprompter blog; ComputelessComputer/autoprompter; reverentgeek/electron-teleprompter; jlecomte/voice-activated-teleprompter.
- Streaming-result de-flickering: arXiv 2006.01416.
- Compliance copilots: Balto real-time agent assist; Sedric real-time assistant.
- MTM required elements: NBMTM CMS CMR/TMR requirements; OBRA-90 counseling/DUR.
- Deepgram Nova-3 Medical (cloud tier): deepgram.com/learn/introducing-nova-3-medical-speech-to-text-api.
