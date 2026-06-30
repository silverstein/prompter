//! Speech recognition abstraction.
//!
//! Prompter aligns recognized speech to a *known* script, so the rest of the
//! engine only needs a provider-agnostic stream of recognized text (plus
//! optional word timings). Concrete providers -- Apple `SpeechAnalyzer` (macOS
//! fast-path), a portable sherpa-onnx engine (cross-platform baseline), or a
//! BAA-gated cloud API -- implement [`SpeechProvider`]; the tracker and UI never
//! see provider details. See `docs/UPGRADE-2026.md` (decisions D1, D4).

use std::collections::VecDeque;

/// The last `n` whitespace-separated words of `text`.
///
/// Cumulative streaming recognizers (Apple `SFSpeechRecognizer`) emit the whole
/// growing utterance; the windowed aligner only matches ~1-3 sentences, so the
/// full string stops matching a few sentences in. Feeding only the leading edge
/// keeps the match local. Shared by the app's live feed and the offline replay
/// harness so both behave identically.
pub fn recent_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let start = words.len().saturating_sub(n);
    words[start..].join(" ")
}

/// A single recognized word with optional timing/confidence, when the provider
/// supplies them. `start_ms` / `end_ms` are offsets from session start.
///
/// Word timings are an *optional* anchor for sub-sentence alignment: some
/// providers expose them (Apple `audioTimeRange`, Parakeet word timestamps),
/// others do not (energy-only / segment-only engines). The tracker degrades to
/// text-only matching when they are absent.
#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedWord {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub confidence: Option<f32>,
}

impl RecognizedWord {
    /// A word with no timing/confidence metadata.
    pub fn bare(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            start_ms: None,
            end_ms: None,
            confidence: None,
        }
    }
}

/// One recognition update from a provider.
///
/// `is_final == false` is a *volatile / partial* hypothesis that may be revised
/// before it stabilizes; the tracker previews a tentative cursor from it but
/// does not commit. `is_final == true` is stabilized text that is safe to
/// commit. Driving the cursor from partials (and committing on finals) is what
/// keeps latency low without letting recognition "flicker" jump the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechUpdate {
    /// Full recognized text for this hypothesis (provider-normalized or raw).
    pub text: String,
    /// Per-word breakdown when available; may be empty.
    pub words: Vec<RecognizedWord>,
    /// Whether this hypothesis is stabilized (safe to commit) vs volatile.
    pub is_final: bool,
}

impl SpeechUpdate {
    /// A volatile/partial hypothesis (preview only).
    pub fn partial(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            words: Vec::new(),
            is_final: false,
        }
    }

    /// A stabilized hypothesis (safe to commit).
    pub fn finalized(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            words: Vec::new(),
            is_final: true,
        }
    }

    /// Attach per-word timing/confidence.
    pub fn with_words(mut self, words: Vec<RecognizedWord>) -> Self {
        self.words = words;
        self
    }

    /// Whether any word in this update carries timing information.
    pub fn has_word_timings(&self) -> bool {
        self.words.iter().any(|w| w.start_ms.is_some())
    }
}

/// A source of streaming speech recognition.
///
/// Implementations capture and decode audio out of band (a Swift sidecar's
/// stdout, an ONNX inference loop, a cloud websocket) and buffer
/// [`SpeechUpdate`]s for the tracker to drain. `poll` is non-blocking so the
/// tracker can run on its own cadence.
pub trait SpeechProvider {
    /// Stable identifier for logging/telemetry, e.g. `"apple-speechanalyzer"`.
    fn id(&self) -> &str;

    /// Return the next buffered update, or `None` when nothing new is ready.
    fn poll(&mut self) -> Option<SpeechUpdate>;

    /// Reset recognition state (e.g. between sessions).
    fn reset(&mut self);
}

/// In-memory provider that replays a fixed sequence of updates.
///
/// Used by tests and by offline replay/dev tooling so the tracker and the
/// rest of the pipeline can be exercised with no audio device and no macOS /
/// model dependency.
#[derive(Debug, Default)]
pub struct MockSpeechProvider {
    queue: VecDeque<SpeechUpdate>,
}

impl MockSpeechProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from a sequence of pre-made updates.
    pub fn from_updates(updates: impl IntoIterator<Item = SpeechUpdate>) -> Self {
        Self {
            queue: updates.into_iter().collect(),
        }
    }

    /// Queue one update.
    pub fn push(&mut self, update: SpeechUpdate) {
        self.queue.push_back(update);
    }

    /// Queue a sequence of stabilized (final) updates from plain strings.
    pub fn push_finals<I, S>(&mut self, texts: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for t in texts {
            self.queue.push_back(SpeechUpdate::finalized(t));
        }
    }

    /// Number of updates still queued.
    pub fn remaining(&self) -> usize {
        self.queue.len()
    }
}

impl SpeechProvider for MockSpeechProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn poll(&mut self) -> Option<SpeechUpdate> {
        self.queue.pop_front()
    }

    fn reset(&mut self) {
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_vs_final() {
        let p = SpeechUpdate::partial("you are taking");
        assert!(!p.is_final);
        let f = SpeechUpdate::finalized("you are taking warfarin");
        assert!(f.is_final);
    }

    #[test]
    fn word_timings_detected() {
        let bare = SpeechUpdate::finalized("warfarin");
        assert!(!bare.has_word_timings());

        let timed = SpeechUpdate::finalized("warfarin").with_words(vec![RecognizedWord {
            text: "warfarin".into(),
            start_ms: Some(1200),
            end_ms: Some(1800),
            confidence: Some(0.91),
        }]);
        assert!(timed.has_word_timings());
    }

    #[test]
    fn mock_provider_replays_in_order() {
        let mut p = MockSpeechProvider::new();
        p.push_finals(["one", "two"]);
        p.push(SpeechUpdate::partial("thr"));
        assert_eq!(p.remaining(), 3);
        assert_eq!(p.poll().unwrap().text, "one");
        assert_eq!(p.poll().unwrap().text, "two");
        let last = p.poll().unwrap();
        assert_eq!(last.text, "thr");
        assert!(!last.is_final);
        assert!(p.poll().is_none());
    }

    #[test]
    fn mock_provider_reset_clears() {
        let mut p = MockSpeechProvider::from_updates([SpeechUpdate::finalized("x")]);
        assert_eq!(p.id(), "mock");
        p.reset();
        assert!(p.poll().is_none());
    }
}
