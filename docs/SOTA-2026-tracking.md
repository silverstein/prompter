# Prompter tracking: SOTA research (July 2026)

Written to answer a direct question: are we debugging things the world already
solved, are we doing something novel, and should the tracker use a local LLM?
Short version: the *tracking* is a solved problem with decades of prior art, our
general approach is right, our specific *matching primitive* is the weak link, and
a local LLM is the wrong tool for the real-time loop. What is genuinely ours is the
pharmacy integration around it. Recommendation at the bottom.

## 1. Is voice-tracked prompting a solved problem? Yes.

It is a shipping product category and an established research field. We are not
inventing the science; we are re-implementing it.

- **Commercial:** PromptSmart's patented **VoiceTrack** has followed a speaker's
  voice through a known script, on-device and offline, since ~2013 (now 15+
  languages). Speakflow, Teleprompter Premium, BIGVU and others ship the same
  capability. The product category is mature.
- **Academic (the direct analog):** "reading tutors that listen" follow a person
  reading a KNOWN text aloud and flag skips/repeats/mis-reads in real time. CMU's
  Project LISTEN worked on exactly this for ~20 years. There is a whole
  "reading miscue detection" literature and a recent end-to-end **pointer-network**
  reader-tracker (arXiv 2310.11486).
- **Adjacent (same math):** real-time karaoke / lyrics-to-audio alignment follows a
  live performance against a reference using **Online DTW (OLTW)**.

The bugs we have been fixing (cumulative partials, coincidental forward jumps) are
precisely the hard, unglamorous robustness problems those systems already solved.
We hit them one by one because we built the engine from scratch.

## 2. What the standard/SOTA algorithm actually is

Convergent across all three literatures:

1. **Align at the WORD level, not character-bigrams over whole sentences.** Match
   the stream of recognized *words* against the reference *word* sequence.
2. **Bounded, monotonic dynamic-programming alignment** (edit-distance / online-DTW
   over a local band around the current position). Monotonic = mostly forward;
   bounded = only look at a small window so a distant coincidence is never even a
   candidate; DP = skips, repeats, mis-reads and ad-libs are modeled as
   **insertion / deletion / substitution** edits instead of causing jumps.
3. **Per-word similarity = orthographic (string) match, optionally + phonetic**,
   with a threshold (reading-tutor systems use ~0.8 orthographic; some add a
   phonetic/acoustic score to tolerate ASR errors). A spoken word either matches the
   expected reference word or is an edit; the cursor advances along the DP path.

This is decades-old, well-trodden, and independent of PromptSmart's specific patent
(the general technique is prior art from the reading-tutor world).

## 3. Why OUR current matcher is the weak link

We use **character-bigram Dice similarity over 1-3 sentence windows**. Two structural
flaws, both observed in our own recordings (see `examples/replay`, `examples/score`):

- **Length bias.** Dice of a short recognized fragment against a long true sentence
  is low (the long sentence's many bigrams swamp the denominator). Measured: the
  fragment "between your supplements that" scored **0.28** on the true long sentence
  it literally came from, but **0.35** on a coincidental short sentence 3 lines down.
- **No word structure.** Character bigrams share generic English letter-pairs, so an
  unrelated short sentence can out-score the real one. The matcher then jumps to the
  coincidence and (being forward-only) sticks.

Our recent fixes (drive from partials, slide the window, locality bias) are the right
*patches*, but they are compensating for a matching primitive that the field abandoned
in favor of word-level DP alignment. Word-level alignment does not have the length-bias
or the coincidence problem, because it matches word-to-word and moves along a monotonic
path, not to "the globally best-looking sentence in a wide window."

## 4. Should the tracker use a local LLM? No, not in the real-time loop.

2026 local models are genuinely good, but wrong for this job:

- **Latency / jitter.** The cursor must update several times per second. The fastest
  small models on Apple Silicon run ~130-160 tok/s with ~12-13 ms/token (Gemma 4 E2B,
  Phi-4 Mini); a per-update prefill+decode costs tens to hundreds of ms with variance.
  A DP word-aligner runs in **microseconds**, deterministically. An LLM here would
  reproduce the exact "laggy" feeling we started with.
- **Nondeterminism breaks our safety net.** The record/replay harness (deterministic
  offline replay of a real read) is what let us find and fix every bug so far. An LLM
  in the loop makes tracking un-replayable and un-testable.
- **It fixes a part that isn't broken.** The ASR text was always accurate (the green
  live text). The failure is in *matching*, which is a string-alignment problem, not a
  reasoning problem.

Where a model DOES belong here (all async, none in the hot loop):
- Post-session coaching / checklist adjudication (we already have `LlmChecklistEvaluator`).
- Optional: a phonetic/acoustic similarity signal to tolerate ASR word errors (this is
  a small model as a *feature*, DTW-style, not an LLM deciding the cursor).
- Optional: a "re-sync, I'm lost" recovery action if alignment confidence stays low.

## 5. ASR: already fine; better signal available if we ever want it

Apple's on-device recognizer gives accurate text; the problem was never ASR quality.
If we later want acoustic anchoring (word timings to disambiguate repeats), on-device
options with word-level timestamps exist: **WhisperKit** (Whisper via Core ML, Swift)
and **NVIDIA Parakeet TDT** (RNN-Transducer, built for streaming, word timings). Not
required for the tracking fix; noted for completeness.

## 6. What is genuinely ours (and worth building)

None of the shipping products integrate with a consultation system: our scripts carry
**branches and pauses**, the app records **speech-verified compliance** and a
transcript, and it **deep-links from SynapseRx**. That wrapper is novel and has no
off-the-shelf equivalent. The right move is not "buy instead of build" -- it is "keep
the wrapper, upgrade the engine to the known-good algorithm."

## 7. Recommendation

Replace the matching primitive with a **word-level, bounded, monotonic DP aligner**:

- Keep everything around it: `ScriptTracker` state machine, `SessionRecorder`,
  the UI, deep links, and the record/replay harness (its recordings still replay).
- Change `align.rs`: represent the script as a word sequence with sentence offsets;
  align the recent recognized words against a local band with a monotonic DP that
  scores per-word orthographic similarity and charges insert/delete/substitute; map
  the DP head back to a sentence index for the cursor.
- Validate against the real recordings we already have (`recording.jsonl`,
  `recording2.jsonl`) before any live test; add both as regression fixtures.
- Estimated scope: a focused rewrite of ~150-200 lines in `align.rs` plus tests;
  the tracker/session/UI are unaffected in shape.

This stops the whack-a-mole (each fix so far bought one edge case) and adopts what the
reading-tutor and karaoke fields converged on years ago.

## References

- PromptSmart VoiceTrack (patented on-device voice-following prompting): promptsmart.com
- End-to-end real-time reading tracking with a pointer network: arXiv:2310.11486
- Reading Miscue Detection through ASR (substitution/deletion/insertion via DP alignment):
  arXiv:2406.07060
- CMU Project LISTEN, "Evaluating Tracking Accuracy of an Automatic Reading Tutor":
  cs.cmu.edu/~listen/pdfs/tracking-paper.pdf
- On-line audio-to-lyrics alignment (OLTW): arXiv:2107.14496
- Real-time lyrics alignment (chroma + phonetic features, OLTW): arXiv:2401.09200
- Local LLM on Apple Silicon throughput (2026): llmcheck.net/benchmarks, arXiv:2511.05502
- On-device streaming ASR with word timestamps: WhisperKit; NVIDIA Parakeet TDT
