# Live Script Assist (design proposal)

Status: proposal, 2026-09-23. Owner: Mat. Nothing here is built yet.

## Summary

Prompter follows the speaker through a fixed script. Real conversations drift: the other person answers something early, raises a topic from a later section, or gets confused. Live Script Assist adds a model that listens to the conversation, compares it with the script and the speaker's position, and helps the script adapt to the conversation.

It ships in three stages, each useful on its own:

1. **Watch** (read-only): a live checklist of what's been covered, open questions from the other person, and a one-line way back to the script when the talk drifts.
2. **Suggest**: proposed edits to the not-yet-spoken script (skip, bridge, reorder, rephrase), applied only when the speaker accepts one with a key.
3. **Auto** (experimental, opt-in): accepted-by-default edits, under strict guardrails.

The engine is the Minutes Coach runtime, extended to know about scripts. Prompter stays the tracker and the display.

## Why not silently rewrite the script

Rewriting the unread script automatically, as the conversation happens, is the obvious version of this idea, and it fails in five ways:

- **Reading.** Teleprompter readers look one or two lines ahead. Text that changes in that zone makes the speaker stumble on camera, which is the failure Prompter exists to prevent.
- **Tracking.** Voice tracking works because the script is known and stable. Every rewrite forces a tracker rebuild, and frequent rewrites make the cursor unstable.
- **Latency.** A good rewrite takes seconds; by the time it lands, the moment in the conversation has often passed.
- **Compliance.** For an MTM consult the script is the compliance artifact (CMR required elements, OBRA-90 counseling). A model that rephrases can drop a required element, and a report measured against a moving script proves nothing.
- **Clinical safety and privacy.** Generated text must never add clinical claims. Consult audio or transcripts may only reach a cloud model under a signed BAA (see D3 in `UPGRADE-2026.md`).

Every principle below follows from one of these.

## Principles

1. **The reading zone is off limits.** No change is ever applied to the line being read, anything above it, or the next few lines below it (default: 3 lines, and at least 10 seconds of reading at the current pace). Edits land below that zone.
2. **Suggest by default.** The speaker accepts or dismisses each edit. Auto-apply is a separate, labelled, opt-in mode.
3. **Locked text never changes.** Script lines marked as required (see Script format) can't be edited, skipped or reordered by the model. The speaker can still skip them by hand, and the report shows it.
4. **Rephrase, reorder, bridge or skip; never new facts.** In clinical scripts the model may not add drug names, doses, interactions or recommendations that aren't already in the script.
5. **Everything is recorded.** The report compares delivery against the original script and lists each accepted edit with its time and rationale.
6. **On-device by default.** The fast lane is a local model (the Coach's Ollama provider). A cloud provider is used only when configured and marked as covered by a BAA.
7. **Failure-isolated.** If the assistant is slow, wrong or down, Prompter behaves exactly as it does today. Assist never blocks tracking, scrolling or recording.

## Stage 1: Watch

A read-only side channel. It shows:

- **A required-elements checklist**, ticked live as each element is heard (from the tracker's speech-verified coverage, plus the model for paraphrases the aligner misses).
- **Open questions**: things the other person asked that haven't been answered yet.
- **Drift and a way back**: when the conversation has been off-script for a while, one suggested bridge line back to where the speaker is in the script.

Placement: a slim rail beside the text column, or a line above the status bar. It never overlaps the reading area and is covered by the same screen-share protection as the window. Items fade when they're no longer relevant (the Nudge TTL and supersession rules in Minutes RFC 0004 already cover this).

This stage is the live checklist planned as D6 in `UPGRADE-2026.md`. Live required-element tracking is open whitespace for MTM (ambient scribes only document afterwards), so Stage 1 is valuable even if the later stages never ship.

## Stage 2: Suggest

The model proposes edits to the unread script. Edit kinds:

| Kind | Example | Effect |
|---|---|---|
| skip | "They already listed their medications. Skip the med-list section?" | Marks a span as not needed |
| bridge | "Good question. Let's come back to that after we go over your plan." | Inserts one line below the reading zone |
| reorder | They asked about B12, which is covered two sections later | Moves that section up, right after the reading zone |
| rephrase | They seemed confused by "tug-of-war" | Offers a plainer version of an upcoming sentence |

A suggestion appears as a marked preview below the reading zone. `A` accepts it and `D` dismisses it (both keys are free today). A suggestion expires when the speaker reads past its target, or when newer evidence replaces it.

## Stage 3: Auto (experimental)

Edits apply unless dismissed within a short window. On top of every principle above:

- It's off by default, enabled per script (`assist: auto` in frontmatter), and flagged in the status bar and the report.
- It's limited to bridge and rephrase at first; skips and reorders stay suggestions.
- It's limited to non-clinical scripts until the eval (below) shows zero locked-text edits and zero reading-zone edits over a meaningful corpus.

## Script format additions

Two additions to `.script.md` (see `SPEC.md`), both optional and backwards compatible:

```markdown
---
title: MTM Consultation
assist: suggest            # off | watch | suggest | auto (default: off)
goal: Complete a CMR and get agreement on the plan
---

> REQUIRED: med-review Reviewed every current medication
Let me walk you through each medication you're taking...
```

- `assist` and `goal` in the frontmatter. `goal` is the Coach goal: what the speaker is trying to achieve.
- `> REQUIRED: <id> <label>` marks the paragraph that follows as a required element. Its text is locked, it becomes a checklist item, and the compliance report counts it. It generalizes the MTM required-elements idea to any script (a sales disclosure, a consent statement).

## Architecture

```
 Prompter (tracker + display)                      Minutes (engine)
 ┌─────────────────────────────┐   local socket   ┌──────────────────────────────┐
 │ speech helper (Apple, mic)  │──transcript────▶ │ Coach runtime, script-aware   │
 │ ScriptTracker (position,    │──prompter.state─▶│  fast lane: Ollama (local)    │
 │   coverage, locked set)     │                  │  policy: principles 1-6       │
 │ UI: rail, previews, A / D   │◀─suggestions──── │ ScriptSuggestion v1           │
 └─────────────────────────────┘                  └──────────────────────────────┘
          ▲ or, when Minutes is already recording: transcript from the capture relay
```

**Transcript source.** Two options, both local:
- **Prompter's own recognizer**, already running during a session. Simplest, and needs no Minutes recording. It only hears the speaker's mic, plus the patient's voice if they're in the room or on speaker.
- **The Minutes capture relay** (`crates/core/src/copilot/relay.rs`). It lets another process attach to live evidence without opening a second microphone, and can carry call audio, so both sides of a call are heard.

Start with Prompter's own transcript for Stage 1, and add the relay for the full Minutes blend.

**Prompter publishes `prompter.state`** (v1, local only): the script hash, the outline (section and sentence ids with text), the current position, coverage, the locked set, and the current reading-zone boundary. It publishes on position change and at most a few times a second.

**Minutes returns `ScriptSuggestion`** (v1, a sibling of `Nudge`): `id`, `kind` (checklist, question, bridge, skip, reorder, rephrase), the target span in original-script ids, replacement text (if any), a short rationale, evidence revision, `ttl_ms` and `supersedes`. As with `NudgeDraft`, the model fills only the content fields; ids, evidence, TTLs and supersession are applied by policy code, never trusted to the model. The same policy code rejects any suggestion that touches locked text, targets the reading zone, or (for clinical scripts) introduces terms not already in the script.

**Applying an edit in Prompter:**
1. Splice the change into the script after the reading-zone boundary.
2. Rebuild the tracker for the new main-line sentences, carrying the committed position across.
3. Keep an edit log that maps new sentence ids back to the originals.

The tracker already rebuilds cheaply (each update takes about 1 ms), so this is mostly bookkeeping plus tests.

**Why this split.** Minutes already has the live-assistance machinery: failure isolation, bounded queues, one model request at a time with newer evidence cancelling older advice, prompt-injection-safe request encoding, and a synthetic eval harness. Rebuilding that inside Prompter would duplicate it and drift. Prompter contributes what only it has: the script, the position, and the reading line.

## Compliance and the report

- Delivery is measured against the original script. Accepted edits are listed with time, kind and rationale. Skipped required elements always show as not delivered, whoever skipped them.
- The Stage 1 checklist and the post-session recording check feed the same required-element list.

## Privacy

- Local by default: the transcript and script state stay on the machine, carried over a local socket.
- A cloud provider needs explicit configuration marked as BAA-covered, and the status bar says when one is in use.
- Suggestions are ephemeral. Only accepted edits (script text plus rationale) go into the report, never transcript text.
- The Assist rail inherits the window's screen-share protection.

## Evaluation

Nothing ships past Stage 1 without an eval.

- **Corpus:** synthetic conversations written against real scripts (no patient data), plus replays of Prompter's recorded sessions (`recording.jsonl`). It follows the Minutes copilot eval pattern: a versioned synthetic fixture corpus, run accelerated.
- **Hard gates** (must be zero): edits to locked text, edits inside the reading zone, new clinical terms in clinical scripts.
- **Quality:** checklist accuracy against human labels, how useful suggestions are, false skips, and latency (suggestion ready before the speaker reaches its target).

## First prototype

On a non-clinical script (an X1 advisor call or a product demo), where a bad suggestion costs little and it can be used daily:

1. `> REQUIRED:` parsing and a live checklist from the tracker. Pure Rust, tested with replays.
2. `prompter.state` export over a local socket.
3. A script-aware Coach prompt that produces `ScriptSuggestion` (bridge and skip only), on the local fast lane.
4. The Prompter rail with previews and `A` / `D`.
5. Replay eval with the hard gates above.

## Open questions

- Is a local model fast enough for bridge suggestions to arrive before they're needed? What's the fallback when it isn't?
- In-person consults without call audio: is the speaker's side plus whatever the room mic picks up enough to spot open questions?
- Should Prompter work with Assist when Minutes isn't installed (a thin direct Ollama client for Stage 1), or require Minutes?
- Which keys are least likely to be hit by accident mid-read: `A` / `D`, or a modifier?
