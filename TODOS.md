# TODOS.md — Prompter

## P2: AI Coaching Post-Session
**What:** After a consultation, AI analyzes the transcript against the script for delivery quality feedback ("You rushed through the safety disclosure in section 2", "Patient asked a question at 8:30 you didn't fully address").
**Why:** Turns Prompter from a delivery tool into a coaching platform. High-value differentiation for RxVIP — pharmacy managers want to improve consultation quality across their team.
**Pros:** Unique feature no generic teleprompter offers. Uses data already captured (transcript + compliance + script alignment).
**Cons:** LLM quality varies. Needs careful prompt engineering to be actionable not generic. Sensitive — pharmacists may resist being "graded."
**Context:** Deferred from CEO review (2026-03-20). Ship v1 first, collect real consultation recordings, then build coaching on validated data.
**Effort:** M (human: ~1 week / CC: ~30 min)
**Priority:** P2
**Depends on:** v1 shipping + real consultation recordings to test against.

## P2: Speaker Diarization (Pyannote Integration)
**What:** Replace VAD-only pause detection with full speaker diarization — automatically distinguish pharmacist from patient.
**Why:** VAD-only can't tell who is speaking. If the pharmacist pauses to think (silence), it's treated the same as patient speaking. Diarization would let the scroll behavior differ: keep scrolling during pharmacist pauses, only pause when patient speaks.
**Pros:** More intelligent scroll behavior. Better compliance tracking (can attribute time to pharmacist vs patient).
**Cons:** Requires Python + pyannote dependency — the messiest part of the stack. Needs packaging solution (PyInstaller/PyOxidizer or alternative Rust diarization library).
**Context:** Deferred from CEO review (2026-03-20). V1 ships with VAD-only (no Python dependency). Blocked by solving the packaging problem.
**Effort:** M (human: ~1 week / CC: ~30 min)
**Priority:** P2
**Depends on:** Solving Python dependency packaging. Consider: whisper-diarize (Rust native) or sherpa-onnx diarization as alternatives to pyannote.

## P3: Pharmacist Confidence Indicator
**What:** Subtle color signal in the status bar (green = on script, yellow = drifting, red = off script / ad-libbing) showing how well the speech tracking matches the script.
**Why:** Helps new pharmacists self-correct back to the script without external feedback. Training tool value.
**Pros:** Simple UI element on top of existing alignment engine data (Tier 2). Low implementation effort.
**Cons:** Could be distracting during high-stakes consultations. Needs user testing with real pharmacists before shipping.
**Context:** Deferred from CEO review (2026-03-20). Depends on Tier 2 alignment engine. Test with pharmacists first.
**Effort:** S (human: ~3 hours / CC: ~15 min)
**Priority:** P3
**Depends on:** Tier 2 alignment engine shipping + pharmacist user testing.
