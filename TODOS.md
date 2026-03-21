# TODOS.md — Prompter

## DONE (shipped 2026-03-20)

- [x] Script parser with optional frontmatter, sections, PAUSE/BRANCH, variables
- [x] Tauri v2 teleprompter UI with sentence-level highlighting
- [x] VAD-controlled scroll (Tier 1) — speak to scroll, silence to pause
- [x] Streaming whisper + fuzzy text alignment (Tier 2) — corrects scroll position
- [x] Compliance reports to ~/meetings/consults/ with 0600 permissions
- [x] SPEC.md — open protocol for .script.md format
- [x] Settings persistence (font, speed, pin state)
- [x] Recent scripts + watched folder (~/meetings/scripts/)
- [x] Always-on-top pin toggle
- [x] Drag-and-drop, clipboard paste, file picker
- [x] Keyboard shortcuts (Space, arrows, 1-9, Tab/Enter, +/-, H, Esc)
- [x] Branch tracking in compliance output
- [x] prompter:// URL scheme for RxVIP integration
- [x] Data-driven coaching insights (pacing, coverage, pause discipline)
- [x] Confidence indicator (mic bar: green/amber/red based on alignment)
- [x] Integration doc for RxVIP (updated per their CEO plan feedback)

## P2: LLM-Enhanced Coaching
**What:** Send compliance data + transcript to Claude or OpenAI for deeper narrative coaching feedback beyond the built-in data-driven analysis.
**Why:** Data-driven coaching catches pacing and coverage issues, but can't say "the patient sounded confused when you explained the drug interaction" or "your tone shifted when discussing the supplement risk."
**Pros:** Dramatically richer feedback. Differentiator no competitor has.
**Cons:** Requires API key or Ollama. Cost per session (~$0.01-0.05). Privacy — transcript contains patient conversation.
**Context:** Architecture is ready — coaching.rs can be extended. The built-in tier works without any LLM. Add API key field to settings, make it optional.
**Effort:** S (human: ~1 day / CC: ~20 min)
**Priority:** P2
**Depends on:** Settings UI for API key, privacy consent flow for sending transcript.

## P2: Speaker Diarization
**What:** Replace VAD-only pause detection with full speaker diarization — distinguish pharmacist from patient automatically.
**Why:** VAD can't tell who is speaking. Pharmacist thinking pauses look the same as patient speaking. Diarization would only pause scroll when the patient speaks.
**Pros:** More intelligent scroll behavior. Better compliance tracking (pharmacist vs patient time).
**Cons:** Requires Python + pyannote (packaging nightmare) or a Rust-native alternative (sherpa-onnx, whisper-diarize).
**Context:** V1 ships with VAD-only. Blocked by solving the Python packaging problem or finding a viable Rust alternative.
**Effort:** M (human: ~1 week / CC: ~30 min)
**Priority:** P2
**Depends on:** Evaluating sherpa-onnx vs pyannote packaging.

## P3: Team Analytics Dashboard
**What:** Manager dashboard showing compliance data across all pharmacists and consultations.
**Why:** Pharmacy managers want to see which pharmacists need coaching and which scripts perform best.
**Pros:** Turns individual compliance reports into team-level insights.
**Cons:** Needs a backend (currently everything is local files). Big architectural shift.
**Context:** Future — requires multi-user architecture beyond the current single-desktop model.
**Effort:** L (human: ~2 weeks / CC: ~2 hours)
**Priority:** P3
**Depends on:** Product decision on cloud vs local-only.

## P3: Proper App Icon
**What:** Design a real app icon instead of the current programmatic green square.
**Why:** The icon represents the product in the dock, Spotlight, and Finder.
**Context:** Current icon is a 256x256 programmatic PNG. Needs a proper design: teleprompter/script motif, green accent, works at all sizes (16px to 1024px).
**Effort:** S (design task)
**Priority:** P3
