// ──────────────────────────────────────────────────────────────
// Alignment engine — matches recognized speech to script position.
//
//   Whisper output (noisy text) ──▶ AlignmentEngine ──▶ script position
//
// The engine maintains a "cursor" position in the script and searches
// a sliding window around it for the best fuzzy match against the
// recognized text. This corrects drift from VAD-only tracking.
//
// Algorithm:
//   1. Normalize both texts (lowercase, strip punctuation)
//   2. Extract sliding windows from the script around the cursor
//   3. Score each window against the recognized text using bigram overlap
//   4. If best score exceeds threshold, update cursor to match position
//   5. If no good match, assume ad-lib / off-script — don't move
// ──────────────────────────────────────────────────────────────

/// Result of an alignment attempt.
#[derive(Debug, Clone)]
pub struct AlignResult {
    /// Whether a confident match was found.
    pub matched: bool,
    /// Best matching position (sentence index in the flat list).
    pub position: usize,
    /// Confidence score (0.0–1.0).
    pub confidence: f32,
    /// Whether the speaker appears to be ad-libbing (no match found).
    pub ad_libbing: bool,
}

/// Tracks the speaker's position in the script via text alignment.
pub struct AlignmentEngine {
    /// Flat list of normalized script sentences for matching.
    sentences: Vec<String>,
    /// Current estimated position (sentence index).
    cursor: usize,
    /// How many sentences to search around the cursor.
    window_radius: usize,
    /// Minimum confidence to accept a match.
    threshold: f32,
    /// Number of consecutive failed matches (for ad-lib detection).
    miss_count: u32,
}

impl AlignmentEngine {
    /// Create a new alignment engine from script sentences.
    pub fn new(sentences: Vec<String>) -> Self {
        Self {
            sentences,
            cursor: 0,
            window_radius: 15, // Search ±15 sentences (~30 sentence window)
            threshold: 0.35,   // 35% bigram overlap required
            miss_count: 0,
        }
    }

    /// Get the current estimated position.
    pub fn position(&self) -> usize {
        self.cursor
    }

    /// Set the cursor position (e.g., when user manually navigates).
    pub fn set_position(&mut self, pos: usize) {
        self.cursor = pos.min(self.sentences.len().saturating_sub(1));
        self.miss_count = 0;
    }

    /// Narrow or widen the search window (sentences each side of the cursor).
    /// A tighter window suits live ASR following (prevents a spurious match far
    /// from the cursor); the wider default suits batch/chunk alignment.
    pub fn set_window_radius(&mut self, radius: usize) {
        self.window_radius = radius.max(1);
    }

    /// Search the window around the cursor for the best fuzzy match.
    ///
    /// Pure / non-mutating. Returns `(best_pos, best_score)`, or `None` when the
    /// input is too short or has no usable bigrams to match. Shared by `align`
    /// (which commits) and `peek` (which does not).
    fn search(&self, recognized: &str) -> Option<(usize, f32)> {
        let recognized = normalize(recognized);
        if recognized.len() < 10 {
            // Too short to match reliably.
            return None;
        }

        let rec_bigrams = bigrams(&recognized);
        if rec_bigrams.is_empty() {
            return None;
        }

        let n = self.sentences.len();
        let start = self.cursor.saturating_sub(self.window_radius);
        let end = (self.cursor + self.window_radius + 1).min(n);

        let mut best_adjusted: f32 = f32::MIN;
        let mut best_pos = self.cursor;
        let mut best_raw: f32 = 0.0;

        for i in start..end {
            // Best RAW bigram score for a match starting at i: a single sentence
            // or a 2-3 sentence window (one recognized chunk may span a couple of
            // short script sentences).
            let mut raw = bigram_similarity(&rec_bigrams, &self.sentences[i]);
            if i + 1 < n {
                let combined = format!("{} {}", self.sentences[i], self.sentences[i + 1]);
                raw = raw.max(bigram_similarity(&rec_bigrams, &combined));
            }
            if i + 2 < n {
                let combined = format!(
                    "{} {} {}",
                    self.sentences[i],
                    self.sentences[i + 1],
                    self.sentences[i + 2]
                );
                raw = raw.max(bigram_similarity(&rec_bigrams, &combined));
            }

            // Locality bias. A short or ambiguous recognized fragment can score
            // HIGHER on a coincidental distant sentence (shared common character
            // bigrams) than on the true, long sentence it actually came from --
            // char-bigram Dice is dominated by length. Without a distance cost the
            // cursor jumps to that far match and, being forward-only, sticks
            // there (observed: "Between your supplements that" scored 0.354 on a
            // sentence 3 ahead vs 0.280 on the true one). The penalty makes a
            // distant candidate win only when it is CLEARLY stronger, not merely
            // coincidentally higher. It biases SELECTION only; the acceptance
            // threshold below uses `best_raw`, so a genuine local advance that
            // sits just above threshold is never penalized out (and an ambiguous
            // fragment simply keeps the cursor put instead of flinging it away).
            let dist = (i as isize - self.cursor as isize).unsigned_abs() as f32;
            let adjusted = raw - LOCALITY_PENALTY * dist;
            if adjusted > best_adjusted {
                best_adjusted = adjusted;
                best_pos = i;
                best_raw = raw;
            }
        }

        Some((best_pos, best_raw))
    }

    /// Attempt to align recognized text against the script and COMMIT the
    /// result: on a confident match the cursor advances (forward, or a small
    /// back-jump for re-reading) and miss-state resets; on a miss the miss
    /// counter increments (feeding ad-lib detection). Use this for stabilized /
    /// final hypotheses.
    pub fn align(&mut self, recognized: &str) -> AlignResult {
        let Some((best_pos, best_score)) = self.search(recognized) else {
            return AlignResult {
                matched: false,
                position: self.cursor,
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            };
        };

        if best_score >= self.threshold {
            // Good match — update cursor.
            // Only advance forward or allow small backward jumps (re-reading).
            if best_pos >= self.cursor || self.cursor - best_pos <= 3 {
                self.cursor = best_pos;
            }
            self.miss_count = 0;
            AlignResult {
                matched: true,
                position: best_pos,
                confidence: best_score,
                ad_libbing: false,
            }
        } else {
            self.miss_count += 1;
            AlignResult {
                matched: false,
                position: self.cursor,
                confidence: best_score,
                ad_libbing: self.miss_count > 5,
            }
        }
    }

    /// Non-mutating alignment: compute the best match WITHOUT moving the cursor
    /// or touching miss-state. Use this for partial / volatile hypotheses so a
    /// revised-before-final recognition ("flicker") cannot commit a wrong jump.
    /// On a confident match `position` is the matched index; otherwise it stays
    /// at the current cursor.
    pub fn peek(&self, recognized: &str) -> AlignResult {
        match self.search(recognized) {
            Some((best_pos, best_score)) if best_score >= self.threshold => AlignResult {
                matched: true,
                position: best_pos,
                confidence: best_score,
                ad_libbing: false,
            },
            Some((_, best_score)) => AlignResult {
                matched: false,
                position: self.cursor,
                confidence: best_score,
                ad_libbing: self.miss_count > 5,
            },
            None => AlignResult {
                matched: false,
                position: self.cursor,
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            },
        }
    }
}

/// Normalize text for comparison: lowercase, strip punctuation, collapse whitespace.
fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract character bigrams from text.
fn bigrams(text: &str) -> Vec<[u8; 2]> {
    let bytes = text.as_bytes();
    if bytes.len() < 2 {
        return Vec::new();
    }
    bytes.windows(2).map(|w| [w[0], w[1]]).collect()
}

/// Compute bigram similarity between recognized text bigrams and a reference string.
/// Returns 0.0–1.0 (Dice coefficient of bigram sets).
fn bigram_similarity(rec_bigrams: &[[u8; 2]], reference: &str) -> f32 {
    let ref_normalized = normalize(reference);
    let ref_bigrams = bigrams(&ref_normalized);

    if rec_bigrams.is_empty() || ref_bigrams.is_empty() {
        return 0.0;
    }

    let mut matches = 0u32;
    let mut used = vec![false; ref_bigrams.len()];

    for rb in rec_bigrams {
        for (j, refb) in ref_bigrams.iter().enumerate() {
            if !used[j] && rb == refb {
                matches += 1;
                used[j] = true;
                break;
            }
        }
    }

    // Dice coefficient
    2.0 * matches as f32 / (rec_bigrams.len() + ref_bigrams.len()) as f32
}

/// Minimum bigram-Dice similarity for a confident match (the engine's internal
/// alignment threshold). Exposed so callers that score outside an
/// [`AlignmentEngine`] (e.g. choosing among branch options) use the same bar.
pub const MATCH_THRESHOLD: f32 = 0.35;

/// Per-sentence distance cost used to bias windowed search toward the current
/// cursor (locality / forward bias). Subtracted from a candidate's raw score
/// per sentence of distance when SELECTING the best match, so a coincidental
/// far match must beat a near one by this much per sentence to win. Small
/// enough not to block genuine local advances (which score well above their
/// neighbours), large enough to reject an ambiguous fragment's far coincidence
/// (0.03/sentence => a match 3 ahead must be >0.09 stronger to jump).
const LOCALITY_PENALTY: f32 = 0.03;

/// Bigram-Dice similarity (0.0-1.0) between recognized text and a reference
/// string, applying the same normalization the engine uses. A standalone scorer
/// for one-off comparisons (branch-option selection, return-to-main detection)
/// that do not warrant a full windowed [`AlignmentEngine`] pass.
pub fn similarity(recognized: &str, reference: &str) -> f32 {
    let normalized = normalize(recognized);
    let rec_bigrams = bigrams(&normalized);
    bigram_similarity(&rec_bigrams, reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_engine() -> AlignmentEngine {
        let sentences = vec![
            "hi thanks for meeting with me today".into(),
            "my goal is simple to make sure every medication youre taking is safe".into(),
            "ill walk you through everything step by step".into(),
            "i understand youve been feeling dizzy lately".into(),
            "does that sound helpful to you".into(),
            "let me walk you through exactly what i see happening".into(),
            "you are currently taking warfarin and metformin".into(),
            "does this make sense so far".into(),
        ];
        AlignmentEngine::new(sentences)
    }

    #[test]
    fn exact_match() {
        let mut eng = make_engine();
        let r = eng.align("Hi, thanks for meeting with me today.");
        assert!(r.matched, "should match first sentence");
        assert_eq!(r.position, 0);
        assert!(r.confidence > 0.5);
    }

    #[test]
    fn fuzzy_match_with_whisper_errors() {
        let mut eng = make_engine();
        // Whisper might produce slightly different text
        let r = eng.align("hi thanks for meeting me today my goal is simple");
        assert!(r.matched, "should fuzzy match despite whisper errors");
        assert!(r.position <= 1, "should be near the start");
    }

    #[test]
    fn match_advances_cursor() {
        let mut eng = make_engine();
        // Match first sentence
        eng.align("hi thanks for meeting with me today");
        assert_eq!(eng.position(), 0);

        // Match later sentence — cursor should advance
        let r = eng.align("you are currently taking warfarin and metformin");
        assert!(r.matched);
        assert_eq!(r.position, 6);
        assert_eq!(eng.position(), 6);
    }

    #[test]
    fn peek_does_not_move_cursor() {
        let mut eng = make_engine();
        eng.align("hi thanks for meeting with me today");
        assert_eq!(eng.position(), 0);
        // Peeking a later sentence reports the match but must not advance.
        let r = eng.peek("you are currently taking warfarin and metformin");
        assert!(r.matched);
        assert_eq!(r.position, 6);
        assert_eq!(eng.position(), 0, "peek must not move the committed cursor");
    }

    #[test]
    fn locality_bias_keeps_ambiguous_fragment_from_jumping_forward() {
        // Real regression from a recorded read. While reading the long sentence
        // at idx 3, the recognizer re-segmented down to the short fragment
        // "between your supplements that". That fragment scores HIGHER (char
        // bigrams) on the coincidental short sentence at idx 6 (~0.354) than on
        // the true long sentence at idx 3 (~0.280) that literally contains those
        // words -- Dice is dominated by length. Without locality bias the cursor
        // jumps +3 to the coincidence and, being forward-only, sticks (the
        // reported "it jumped to another part of the script"). Locality bias must
        // keep it put.
        let sentences = vec![
            "hi thanks for meeting with me today".into(),
            "it sounds like these symptoms have been really worrying you".into(),
            "and to be completely honest they caught my attention right away".into(),
            "ive reviewed your profile and found a few critical things going on between your prescriptions and your supplements that we need to handle today to keep you safe".into(),
            "ive flagged this so im fixing it by putting together a clear simple plan".into(),
            "i know this can feel like a lot thats why im here to guide you through it".into(),
            "let me walk you through exactly what i see happening".into(),
            "the most urgent thing i found today relates to your symptoms".into(),
        ];
        let mut eng = AlignmentEngine::new(sentences);
        eng.set_window_radius(10);
        eng.set_position(3);
        let r = eng.peek("between your supplements that");
        assert!(
            r.position <= 3,
            "ambiguous fragment jumped forward to idx {} (expected <= 3)",
            r.position
        );
    }

    #[test]
    fn peek_then_align_commits() {
        let mut eng = make_engine();
        let p = eng.peek("you are currently taking warfarin and metformin");
        assert!(p.matched);
        assert_eq!(eng.position(), 0, "peek leaves the cursor put");
        let a = eng.align("you are currently taking warfarin and metformin");
        assert!(a.matched);
        assert_eq!(eng.position(), 6, "align commits the move");
    }

    #[test]
    fn no_match_for_adlib() {
        let mut eng = make_engine();
        // Use text with zero overlap with pharmacy script vocabulary
        let r =
            eng.align("jupyter notebook crashed during pytorch backpropagation gradient descent");
        assert!(
            !r.matched,
            "unrelated tech text should not match pharmacy script, confidence: {}",
            r.confidence
        );
    }

    #[test]
    fn ad_lib_detection_after_many_misses() {
        let mut eng = make_engine();
        let gibberish = [
            "kubernetes pod crashed during horizontal autoscaling",
            "webpack bundle optimization tree shaking configuration",
            "postgresql vacuum autovacuum bloat index fragmentation",
            "docker container orchestration swarm cluster deployment",
            "terraform provider plugin registry configuration syntax",
            "rustfmt clippy cargo workspace dependency resolution",
        ];
        for g in &gibberish {
            eng.align(g);
        }
        let r = eng.align("nginx reverse proxy upstream timeout configuration");
        assert!(r.ad_libbing, "should detect ad-libbing after many misses");
    }

    #[test]
    fn short_text_no_match() {
        let mut eng = make_engine();
        let r = eng.align("hi");
        assert!(!r.matched, "too short to match");
    }

    #[test]
    fn normalize_strips_punctuation() {
        assert_eq!(normalize("Hello, World!"), "hello world");
        assert_eq!(
            normalize("You're taking Warfarin."),
            "you re taking warfarin"
        );
    }

    #[test]
    fn bigram_similarity_identical() {
        let bg = bigrams("hello world");
        let score = bigram_similarity(&bg, "hello world");
        assert!(
            (score - 1.0).abs() < 0.01,
            "identical text should score ~1.0, got {}",
            score
        );
    }

    #[test]
    fn bigram_similarity_different() {
        let bg = bigrams("hello world");
        let score = bigram_similarity(&bg, "goodbye universe");
        assert!(
            score < 0.3,
            "different text should score low, got {}",
            score
        );
    }

    #[test]
    fn similarity_helper_matches_and_separates() {
        // A near-exact read clears the threshold; the other option does not, and
        // the matching option scores strictly higher (margin for selection).
        let yes = similarity(
            "great lets discuss your concerns",
            "great lets discuss your concerns in detail",
        );
        let no = similarity(
            "great lets discuss your concerns",
            "okay then lets keep moving along",
        );
        assert!(
            yes >= MATCH_THRESHOLD,
            "matching option should clear bar: {yes}"
        );
        assert!(
            yes > no,
            "matching option must beat the other: {yes} vs {no}"
        );
    }
}
