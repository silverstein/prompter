# Prompter — Vision, Architecture, and Positioning

*An open protocol and reference runtime for scripted professional conversations.*

*Audio is ephemeral. Text is eternal. Your AI remembers everything you were supposed to say.*

---

## The one-sentence positioning

**Prompter is live script guidance and audit-defensible documentation for regulated one-to-one conversations — pharmacist consults, clinical sessions, financial disclosures, research consents — with a single architectural promise: the audio never touches disk.**

---

## The problem

People have to say specific things in specific orders and prove they said them. Pharmacists deliver CMS-mandated MTM counseling. Speech-language pathologists run protocol-fidelity sessions. Financial advisors read suitability disclosures verbatim. IRB coordinators walk subjects through consent forms. Insurance agents deliver state-mandated warnings.

Today they do it with three unsatisfying options:

1. **A printed script and a documentation form they fill out after.** Low fidelity, high burden, easy to cut corners.
2. **A call-center agent-assist platform (Balto, Cresta, Observe.AI).** $40K–$150K/year, cloud-SaaS, requires a BAA, wrong buyer economics for one-to-one professional work. Built for contact-center scale, not a pharmacist at a counter.
3. **A creator-grade teleprompter (PromptSmart, Speakflow).** $10–20/month, no branching, no compliance report, no speaker awareness. Built for YouTubers reading a monologue, not a consult with a patient.

None of these solve the actual problem: **guide the live delivery, produce an audit-defensible artifact, and do it without creating a recording-retention liability.**

---

## The architectural promise (the moat)

**Prompter does not persist audio. Ever.**

- Microphone samples exist in a RAM ring buffer for the duration of one analysis window (seconds, not minutes). They are overwritten by the next chunk.
- Nothing in the runtime writes audio to disk. No `.wav`, no `.mp3`, no encrypted blob, no "temp file we clean up later."
- No audio traverses any network. Not for transcription, not for summarization, not for diagnostics.
- At the end of a session, the only artifacts that exist are **text**: a speaker-labeled transcript, a compliance report, and coaching insights.

This is an architectural constraint, not a marketing preference. The codebase actively refuses to add an "audio retention" feature even when users ask for it. The constraint is what makes the rest of the product possible.

### Why this is the moat

Every competitor in every adjacent category persists audio by design:

- Apple Recall, Windows Copilot, Teams, Zoom — persistence is the product
- Otter, Fireflies, Granola — the recording IS the value proposition
- Balto, Cresta, Observe.AI — call recordings feed their analytics
- LiveKit Agents — session recording is a primary feature
- PioneerRx Mobile Patient Counseling — eCare plan data retention

None of them can credibly claim "we don't keep the audio" without rebuilding their architecture. They won't. **"Audio never touches disk" is a claim only a product designed around it from day one can make.**

### Why this is legally powerful

Eleven US states require all-party consent before audio recording (CA, FL, IL, MD, MA, MI, MT, NH, OR, PA, WA — three of the five largest pharmacy markets). The moment you persist audio, you're in wiretap-statute territory. HIPAA layers on top: persisted audio becomes PHI at rest, triggering BAA requirements, breach notification, 6-year retention rules.

Prompter sidesteps the entire stack:

- No persisted audio means no wiretap claim to litigate.
- No persisted audio means no BAA negotiation.
- No persisted audio means the "we don't record" disclosure is literally true, not marketing gymnastics.
- The disclosure script becomes: *"This session produces a written record of what was discussed. Audio is not recorded or retained."* Simpler, more accurate, less scary than the industry-standard "this call may be recorded."

### What this constraint disallows

To hold the line, the runtime will never:

- Write audio to any disk path under any circumstance.
- Offer an "archive audio for training" mode, even opt-in.
- Accept a plugin that intercepts the audio stream and persists it.
- Send audio over the network (even to a local daemon that claims it won't save it).
- Generate post-hoc features (diarization, summarization, coaching) from audio — only from the text artifacts.

These are religious. They are the product.

---

## The architectural promise (the flywheel)

**Prompter produces rich text artifacts forever.**

The moat is on audio. The platform is on text. Every session produces:

1. `transcript.jsonl` — speaker-labeled, timestamped, line-delimited JSON. Human-readable, agent-readable, diff-able.
2. `compliance.md` — structured markdown report: sections covered, adherence percentages, branch decisions, pause-point coverage, duration.
3. `coaching.md` — insights derived from timing and coverage: "you rushed section 3," "you paused 14s at the patient response — good."

These text artifacts are the substrate of the entire platform. They feed:

- **Minutes vault indexing** — each consult becomes searchable memory.
- **Agent queries via MCP** — "what did I cover about warfarin this month?"
- **Coaching dashboards** — aggregate trends across sessions, derived from text.
- **Knowledge-graph enrichment** — entity extraction on transcripts builds professional memory.
- **Script adherence telemetry** — anonymized, aggregated, feeds back to script maintainers.

The flywheel runs entirely on text. No audio was needed for any of it.

---

## How it works — the pipeline

```
[Microphone]
     | 100ms chunks
     v
[Ring buffer in RAM] ──── audio lives here for seconds, then is overwritten.
     |                    Never touches disk.
     v
[VAD: Silero — via minutes-core]
     | "someone is speaking"
     v
[Voice embedding match — minutes-core voice module]
     | "is this the enrolled pharmacist? confidence score?"
     v
[Live ASR — platform-dispatched]
     | "Hi thanks for meeting with me today"
     |
     +---> [Alignment engine — prompter-core]
     |        | fuzzy bigram match against script sentences
     |        v
     |     [UI scrolls the script]
     |
     +---> [Append to transcript.jsonl — text only]
              {"t": 0.3, "speaker": "pharmacist", "confidence": 0.91,
               "text": "hi thanks for meeting with me today"}

[Audio bytes]  ← still in the ring buffer, about to be overwritten.
                 Never written to disk. GC reclaims them.
```

At session end, Prompter writes three text files and exits. No cleanup step for audio because no audio was ever created on disk to clean up.

---

## Platform-dispatched ASR

One trait, three backends, best-fit per platform:

```rust
pub trait SpeechProvider {
    fn feed(&mut self, samples: &[f32]) -> Option<Recognition>;
    fn finalize(&mut self) -> Option<Recognition>;
}
```

| Platform | Default backend | Alternative | Why |
|---|---|---|---|
| macOS Apple Silicon | SFSpeech (Swift sidecar) | Parakeet via Metal | SFSpeech: zero model download, excellent quality, free. Parakeet: OSS contributors who don't want the Swift dep. |
| Windows | Whisper streaming (`minutes-core::streaming_whisper`) | — | Only cross-platform streaming option. |
| Linux CPU | Whisper streaming | — | Same as Windows. |
| Linux + NVIDIA | Whisper CUDA | Parakeet NeMo sidecar | User choice; both run locally. |

The alignment engine consumes the trait, doesn't know or care which backend produced the text.

---

## Speaker awareness — honest scope

Voice enrollment is used for **live scroll quality**, not forensic attribution.

A pharmacist enrolls their voice once (30-second sample, stored as a single embedding vector in a local SQLite DB — minutes-core's `voice` module). During a session, each VAD-segmented utterance gets a live embedding-match score against the enrolled profile.

What the score is used for:

- **High confidence match** → this is the pharmacist speaking. The alignment engine tries to match their text against the script.
- **High confidence non-match** → this is the patient (or someone else). The alignment engine pauses, doesn't advance scroll.
- **Low confidence** → ambiguous. The alignment engine falls back to its existing fuzzy-match behavior (if the text matches the script, scroll; if not, hold).

What the score is NOT used for:

- Forensic speaker attribution in the compliance report. The compliance report derives adherence from **script-match evidence**, not from "we're 73% sure the pharmacist said X." Speaker labels in the transcript carry confidence scores so they're interpretable, but the compliance artifact doesn't depend on them.

This is a deliberate narrowing after adversarial review. Minutes itself uses voice enrollment post-hoc in a confidence-gated ladder because "wrong names are worse than anonymous." Prompter inherits the same discipline: speaker labels are a help, not a claim.

---

## What we use from Minutes

Prompter consumes minutes-core as a library dependency. We use:

| Capability | Module | Usage |
|---|---|---|
| Cross-platform audio capture | `streaming::AudioStream` | Read chunks from mic. Never write. |
| Voice activity detection | `vad::Vad` | "Is someone speaking?" at ~10Hz. |
| Voice enrollment primitives | `voice::{save_profile, match_embedding, cosine_similarity}` | Enroll once, match live. |
| Streaming Whisper | `streaming_whisper::StreamingWhisper` | Windows/Linux live ASR. |
| Streaming Parakeet (when landed) | `live_transcript::finalize_live_utterance` + Parakeet path | Alternative on Apple Silicon + NVIDIA. |
| Model download machinery | `config` patterns + `~/.config/minutes/models/` | First-run setup. |
| Markdown helpers | `markdown` | Format compliance report. |

We do NOT use:

- `capture` recording mode (writes WAV files). We only use `AudioStream`.
- `live_transcript` (optional WAV preservation). We built our own text-only loop.
- `pyannote-rs` diarization (requires audio file). Speaker labels come from live embedding match instead.
- `vault` auto-sync. Users copy text artifacts wherever they like.
- `screen`, `knowledge_extract`, `calendar`, `autoresearch`, `daily_notes` — different product, different scope.

**Why separate products, not a merged binary:**

- Different UX. Prompter is full-screen, content-protected, focused on one live moment. Minutes is menu-bar, background, ambient memory capture.
- Different failure modes. Prompter failing means a pharmacist is mid-consult without a script. Minutes failing means a meeting isn't being logged.
- Different target users. Prompter is a specialized professional tool. Minutes is horizontal for anyone with meetings.
- Different moat. Prompter's moat is never-persist. Minutes' moat is the memory flywheel (which requires persistence). Architecturally incompatible in one binary.

The integration point is the text artifact. Prompter writes markdown; Minutes indexes whatever markdown is in the vault. Users who run both get a compounding effect; users who run only Prompter still get full value.

---

## The open protocol — `.script.md`

The core asset is the script format. Open, readable, diffable:

```markdown
---
title: MTM Consultation — Jane Smith
type: pharmacy-consultation
version: "2.1"
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
estimated_duration: 18min
---

# Intro

Hi {{patient_name}}, thanks for meeting with me today.

> PAUSE: Does that sound helpful to you?

# Findings

You're currently taking {{medications}}.

> BRANCH: Would you like me to get this plan started?
>> YES
Great. Let me get that organized.
>> NO
No problem. May I share with your doctor?
```

The format is intentionally simple. Any text editor creates it. Any markdown renderer displays it. The `PAUSE` and `BRANCH` directives render as blockquotes for human readers and as structured control flow for teleprompter tools.

**Why this is strategic:** the format is published, permissive, and invites ecosystem. Anyone can build a compatible tool. Anyone can publish scripts. A health system can version-control its MTM scripts in Git. A pharmacy benefits manager can push CMS-compliant updates to its network. A researcher can share their IRB consent script as a gist. The format becomes to scripted conversations what Markdown is to documents.

The format spec lives in `SPEC.md`. It is versioned, will have a conformance test suite, and is under permissive license.

---

## The surfaces (what we build and ship)

Five layers, same core, different audiences:

### 1. Prompter Desktop — the OSS reference runtime

- Rust + Tauri, cross-platform (macOS, Windows, Linux).
- Never-persist runtime, platform-dispatched ASR, all the architecture above.
- License: permissive (MIT or Apache 2.0 — TBD).
- Ships via `brew install --cask silverstein/tap/prompter`, `winget`, `apt`/`dnf` repos.
- The thing HN and product-hunt launches. The thing contributors fork.

### 2. The `.script.md` protocol

- Spec + reference parser + conformance test suite.
- Script repositories on GitHub for community curation (MTM, SLP, KYC, IRB templates).
- Any tool that supports `.script.md` is a Prompter-compatible tool.

### 3. MCP server for Prompter

- Agents list scripts, trigger sessions, query past transcripts, read compliance reports.
- Published via `npx prompter-mcp` (matching the Minutes pattern).
- Works with Claude Code, Codex, Gemini CLI, OpenCode, Claude Desktop — inherits the agent ecosystem Minutes already cultivated.

### 4. Certified script marketplace (the commercial asset)

- Curated, versioned, legally-vetted scripts for regulated verticals.
- Subscription access for organizations: "get the latest CMS-compliant Warfarin consult script pushed quarterly."
- Revenue share with domain experts who maintain the scripts.
- This is the closed-source asset. The runtime is open; the certification process and the verified library are the IP.
- Adversarial review was explicit: open-sourcing everything is a trap. This is where the moat for commercial sustainability lives.

### 5. Prompter for Platforms (licensed SDK)

- Embed Prompter as a module in MTM platforms (Outcomes, Aspen, MedMe, Cureatr), pharmacy management systems (PioneerRx, QS/1), EHRs (Epic ambulatory, Cerner).
- White-label, revenue share, enterprise licensing.
- Distribution via the incumbent platforms' existing relationships, solving the "pharmacist isn't the buyer" problem identified in adversarial review.
- 5–10 logos globally, $50K–$500K ACV each. One-person go-to-market is viable.

---

## Commercial model summary

| Tier | Audience | Price | What it is |
|---|---|---|---|
| Desktop OSS | Individual pros, creators, contributors | Free | The runtime. Community flywheel. |
| Script subscriptions | Compliance-conscious orgs | $20–100/seat/mo | Curated, versioned, verified scripts for their vertical. |
| Prompter for Platforms | MTM platforms, EHRs, PMS vendors | $50K–$500K/year | SDK + white-label licensing. |

Revenue comes from scripts and platform licensing. The runtime stays free and open forever.

---

## Go-to-market sequence

1. **Ship the OSS runtime** to the existing community (Hacker News, Show HN, the minutes audience). Collect early feedback. Get 3–5 non-pharmacy power users (SLPs, interview rubric teams, sales coaches) to validate the broader thesis.
2. **Land the first regulated pilot.** Best-fit wedge per adversarial research: **IRB informed consent at a research university.** Recording is already normalized, universities already audit verbatim, the IRB office is a real budget-holder, and there's no incumbent. Pharmacy MTM is vertical #2.
3. **Publish the first certified script library** (IRB consent templates) alongside the pilot case study. Monetize the script subscription for organizations.
4. **Open conversations with MTM platforms** (Aspen RxHealth, MedMe, Outcomes) about white-label SDK licensing. Existing RxVIP relationships accelerate this.
5. **Release MCP server** — make Prompter queryable from every major agent framework. Turn every Claude Code user who writes scripts into a distribution channel.

---

## What we will not build

A list maintained to defend the architecture from feature-creep requests:

- **No audio retention, ever.** Not even opt-in. Not even for "training the model." Not even with "but users asked for it." This is the whole product.
- **No cloud ASR.** All speech recognition runs on-device, always.
- **No cloud LLM calls by default.** Summarization and coaching use local models (Ollama, minutes' agent CLI integration) or are disabled. If a user wants to query their text transcripts with a cloud LLM, that's their MCP client, not Prompter.
- **No integrated video recording.** Screen capture, camera recording — not Prompter's job. Separate concerns.
- **No built-in multi-user / server mode.** Prompter is a single-user desktop tool. Organization features happen at the script-subscription and SDK layers, not by turning Prompter into a SaaS.
- **No "Prompter Cloud."** The entire point is local. A cloud version would undermine the positioning. Organizations get the SDK; individuals get the desktop app.

---

## Open questions (acknowledged, not yet decided)

- **License choice** — MIT, Apache 2.0, or a source-available license with a commercial carve-out for the script marketplace. Leaning MIT for the runtime + proprietary for certified scripts.
- **Windows-first vs Mac-first for initial pharmacy pilots.** Pharmacy computers are Windows; Mat's personal dev environment is Mac. Likely: finish Mac runtime for personal validation, build Windows parity before first pharmacy contract.
- **IRB vs MTM as the first commercial wedge.** Adversarial research favors IRB (cleaner buyer, less incumbent capture). MTM is the original pilot context and has existing relationships through RxVIP. May pilot both in parallel, commercialize the one that moves faster.
- **How much coaching is derivable from text alone** without LLM. Timing, coverage, pause discipline, section balance — all available from the transcript. Tone, energy, empathy — not available from text. Be honest in the product about what's measurable.

---

## The story, in three sentences

Prompter is an open protocol and reference runtime for scripted professional conversations. It produces audit-defensible documentation without ever persisting audio — a claim no major competitor can credibly make. The text it produces becomes durable memory for your agents and your organization; the sound it heard stops existing the moment the session ends.

---

*Version 0.1 — initial vision draft. Revised after two rounds of market research and adversarial review. Commit when the team agrees this is the story.*
