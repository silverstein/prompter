// ──────────────────────────────────────────────────────────────
// Alignment engine — matches recognized speech to script position.
//
//   ASR text (noisy, streaming) ──▶ AlignmentEngine ──▶ script position
//
// WORD-LEVEL alignment. The reference script is stored as a flat WORD
// sequence with a word→sentence map. Each update, the recent recognized
// words are aligned against a BOUNDED band of reference words around the
// current cursor using a local, monotonic dynamic-programming alignment
// (Smith-Waterman style): a match/substitution advances the diagonal, a
// skipped reference word is a deletion, an extra spoken word is an
// insertion. The reference word under the LAST matched query word becomes
// the cursor; its sentence index is what the tracker follows.
//
// This replaces an earlier char-bigram-over-sentences matcher whose Dice
// score was dominated by sentence length, so a short recognized fragment
// could score HIGHER on a coincidental distant sentence than on the true
// long one it came from -- and, being forward-only, jump there and stick.
// Word-level runs are discriminative: a coincidence yields isolated single
// word hits that local alignment resets to zero, while genuine reading
// yields a contiguous run. This is the reading-tutor / karaoke-alignment
// standard; see docs/SOTA-2026-tracking.md.
// ──────────────────────────────────────────────────────────────

/// Result of an alignment attempt.
#[derive(Debug, Clone)]
pub struct AlignResult {
    /// Whether a confident match was found.
    pub matched: bool,
    /// Best matching position (sentence index in the flat list).
    pub position: usize,
    /// Confidence score (0.0–1.0): match density along the aligned path.
    pub confidence: f32,
    /// Whether the speaker appears to be ad-libbing (no match found).
    pub ad_libbing: bool,
}

/// Fuzzy per-word match bar: two words count as the "same" word when their
/// character-bigram Dice similarity clears this (tolerates ASR errors and
/// plurals, e.g. "supplements" vs "supplement"). Exact matches score 1.0.
const WORD_MATCH: f32 = 0.7;
/// Score charged for aligning two non-matching words on the diagonal. Strongly
/// negative so the path prefers a gap (or a local restart) over pairing up
/// unrelated words.
const MISMATCH_PENALTY: f32 = -1.0;
/// Score charged for an insertion (extra spoken word) or deletion (skipped
/// reference word). Cheap enough to bridge a couple of skipped/ad-libbed words
/// inside a run, dear enough that scattered coincidental hits don't add up.
const GAP_PENALTY: f32 = -0.5;
/// Minimum matched words on the best path to accept a match at all (one
/// coincidental word is never enough; require a short run).
const MIN_MATCH_WORDS: usize = 2;
/// Minimum match density (matched words / path length) to accept a match.
const COVERAGE_THRESHOLD: f32 = 0.5;

/// Internal result of a windowed search: the reference word the alignment
/// ends on, its sentence, a confidence, and whether it clears the bar.
struct SearchHit {
    end_word: usize,
    sentence: usize,
    confidence: f32,
    matched: bool,
}

/// Tracks the speaker's position in the script via word-level text alignment.
pub struct AlignmentEngine {
    /// Flat list of normalized reference words.
    words: Vec<String>,
    /// Sentence index for each reference word.
    word_sentence: Vec<usize>,
    /// First reference-word index of each sentence (for cursor mapping).
    sentence_first_word: Vec<usize>,
    /// Number of sentences (some may contribute zero words).
    sentence_count: usize,
    /// Current estimated position, in reference-word units.
    cursor_word: usize,
    /// How many sentences to search around the cursor.
    window_radius: usize,
    /// Number of consecutive failed matches (for ad-lib detection).
    miss_count: u32,
}

impl AlignmentEngine {
    /// Create a new alignment engine from script sentences.
    pub fn new(sentences: Vec<String>) -> Self {
        let mut words = Vec::new();
        let mut word_sentence = Vec::new();
        let mut sentence_first_word = Vec::with_capacity(sentences.len());
        for (si, s) in sentences.iter().enumerate() {
            sentence_first_word.push(words.len());
            for w in normalize(s).split_whitespace() {
                words.push(w.to_string());
                word_sentence.push(si);
            }
        }
        Self {
            sentence_count: sentences.len(),
            words,
            word_sentence,
            sentence_first_word,
            cursor_word: 0,
            window_radius: 15, // Search ±15 sentences (~30 sentence window)
            miss_count: 0,
        }
    }

    /// Get the current estimated position (sentence index).
    pub fn position(&self) -> usize {
        self.word_sentence.get(self.cursor_word).copied().unwrap_or(0)
    }

    /// Set the cursor position by sentence (e.g., manual navigation, or the
    /// tracker sliding the search window during partials).
    pub fn set_position(&mut self, sentence: usize) {
        let s = sentence.min(self.sentence_count.saturating_sub(1));
        let w = self.sentence_first_word.get(s).copied().unwrap_or(0);
        self.cursor_word = w.min(self.words.len().saturating_sub(1));
        self.miss_count = 0;
    }

    /// Narrow or widen the search window (sentences each side of the cursor).
    /// A tighter window suits live ASR following (prevents a spurious match far
    /// from the cursor); the wider default suits batch/chunk alignment.
    pub fn set_window_radius(&mut self, radius: usize) {
        self.window_radius = radius.max(1);
    }

    /// Align the recent recognized words against a bounded band of reference
    /// words around the cursor with a local, monotonic DP. Pure / non-mutating.
    /// Shared by `align` (which commits) and `peek` (which does not).
    fn search(&self, recognized: &str) -> Option<SearchHit> {
        if self.words.is_empty() {
            return None;
        }
        let norm = normalize(recognized);
        let q: Vec<&str> = norm.split_whitespace().collect();
        if q.len() < 2 {
            // Need at least a two-word run to align reliably.
            return None;
        }

        // Reference band: window_radius sentences each side of the cursor, in
        // words. Bounding the band is what makes a distant coincidence not even
        // a candidate.
        let cur_sent = self.position();
        let lo = cur_sent.saturating_sub(self.window_radius);
        let hi = (cur_sent + self.window_radius).min(self.sentence_count.saturating_sub(1));
        let r_start = self.sentence_first_word[lo];
        let r_end = if hi + 1 < self.sentence_count {
            self.sentence_first_word[hi + 1]
        } else {
            self.words.len()
        };
        let r = &self.words[r_start..r_end];
        if r.is_empty() {
            return None;
        }

        let m = q.len();
        let n = r.len();
        // h = running score, mc = matched-word count, pl = path length (steps
        // since the last local restart) -- all for the best path to each cell.
        let mut h = vec![vec![0.0f32; n + 1]; m + 1];
        let mut mc = vec![vec![0usize; n + 1]; m + 1];
        let mut pl = vec![vec![0usize; n + 1]; m + 1];

        let cursor_local = self.cursor_word.saturating_sub(r_start) as isize;
        let mut best = 0.0f32;
        let mut best_j = 0usize;
        let mut best_mc = 0usize;
        let mut best_pl = 0usize;

        for i in 1..=m {
            for j in 1..=n {
                let sim = word_sim(q[i - 1], &r[j - 1]);
                let is_match = sim >= WORD_MATCH;
                let diag = h[i - 1][j - 1] + if is_match { sim } else { MISMATCH_PENALTY };
                let up = h[i - 1][j] + GAP_PENALTY; // extra spoken word (insertion)
                let left = h[i][j - 1] + GAP_PENALTY; // skipped ref word (deletion)

                // Local alignment: a running score never drops below zero (a bad
                // stretch restarts the path rather than dragging it negative).
                let mut score = 0.0f32;
                let mut count = 0usize;
                let mut plen = 0usize;
                if diag > score {
                    score = diag;
                    count = mc[i - 1][j - 1] + usize::from(is_match);
                    plen = pl[i - 1][j - 1] + 1;
                }
                if up > score {
                    score = up;
                    count = mc[i - 1][j];
                    plen = pl[i - 1][j] + 1;
                }
                if left > score {
                    score = left;
                    count = mc[i][j - 1];
                    plen = pl[i][j - 1] + 1;
                }
                h[i][j] = score;
                mc[i][j] = count;
                pl[i][j] = plen;

                // Track the best endpoint. Prefer higher score; on a near-tie
                // prefer the endpoint nearest the cursor, so a phrase that
                // repeats in the script resolves to the occurrence we're at.
                if count > 0 {
                    let take = score > best + 1e-4
                        || ((score - best).abs() <= 1e-4
                            && (j as isize - cursor_local).abs()
                                < (best_j as isize - cursor_local).abs());
                    if take {
                        best = score;
                        best_j = j;
                        best_mc = count;
                        best_pl = plen;
                    }
                }
            }
        }

        if best_mc == 0 || best_pl == 0 {
            // Evaluated, but nothing aligned: a real miss (ad-lib / off-script),
            // which must count toward ad-lib detection. This is distinct from
            // "couldn't evaluate" (too short / empty band) returned as None
            // above, which is not a miss.
            return Some(SearchHit {
                end_word: self.cursor_word,
                sentence: self.position(),
                confidence: 0.0,
                matched: false,
            });
        }
        let end_word = r_start + best_j - 1;
        let sentence = self.word_sentence[end_word];
        // Confidence = match density along the aligned path (1.0 = every step
        // was a matched word; lower when the run needed gaps to bridge).
        let confidence = (best_mc as f32 / best_pl as f32).clamp(0.0, 1.0);
        let matched = best_mc >= MIN_MATCH_WORDS && confidence >= COVERAGE_THRESHOLD;
        Some(SearchHit {
            end_word,
            sentence,
            confidence,
            matched,
        })
    }

    /// Attempt to align recognized text and COMMIT: on a confident match the
    /// cursor advances (forward, or a small back-jump for re-reading) and
    /// miss-state resets; on a miss the miss counter increments (feeding ad-lib
    /// detection). Use this for stabilized / final hypotheses.
    pub fn align(&mut self, recognized: &str) -> AlignResult {
        let Some(hit) = self.search(recognized) else {
            return AlignResult {
                matched: false,
                position: self.position(),
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            };
        };
        if hit.matched {
            let cur = self.position();
            // Advance forward, or allow a small backward jump (re-reading).
            if hit.sentence >= cur || cur - hit.sentence <= 3 {
                self.cursor_word = hit.end_word;
            }
            self.miss_count = 0;
            AlignResult {
                matched: true,
                position: hit.sentence,
                confidence: hit.confidence,
                ad_libbing: false,
            }
        } else {
            self.miss_count += 1;
            AlignResult {
                matched: false,
                position: self.position(),
                confidence: hit.confidence,
                ad_libbing: self.miss_count > 5,
            }
        }
    }

    /// Non-mutating alignment: compute the best match WITHOUT moving the cursor
    /// or touching miss-state. Use this for partial / volatile hypotheses so a
    /// revised-before-final recognition ("flicker") cannot commit a wrong jump.
    /// On a confident match `position` is the matched sentence; otherwise it
    /// stays at the current cursor.
    pub fn peek(&self, recognized: &str) -> AlignResult {
        match self.search(recognized) {
            Some(hit) if hit.matched => AlignResult {
                matched: true,
                position: hit.sentence,
                confidence: hit.confidence,
                ad_libbing: false,
            },
            Some(hit) => AlignResult {
                matched: false,
                position: self.position(),
                confidence: hit.confidence,
                ad_libbing: self.miss_count > 5,
            },
            None => AlignResult {
                matched: false,
                position: self.position(),
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            },
        }
    }
}

/// Per-word similarity for alignment. Exact (normalized) words score 1.0; short
/// words (<=3 chars) match ONLY exactly, to avoid "the"~"she" style bigram
/// coincidences; longer words fuzzy-match via character-bigram Dice so ASR
/// errors and plurals still align.
fn word_sim(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    if a.len() <= 3 || b.len() <= 3 {
        return 0.0;
    }
    let ab = bigrams(a);
    bigram_similarity(&ab, b)
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

/// Compute bigram similarity between recognized text bigrams and a reference
/// string. Returns 0.0–1.0 (Dice coefficient of bigram sets).
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

/// Minimum bigram-Dice similarity for a confident match. Used by callers that
/// score whole strings outside an [`AlignmentEngine`] (e.g. choosing among
/// branch options, return-to-main detection), where a single Dice score over
/// the full option text is the right tool.
pub const MATCH_THRESHOLD: f32 = 0.35;

/// Bigram-Dice similarity (0.0-1.0) between recognized text and a reference
/// string, applying the same normalization the engine uses. A standalone scorer
/// for one-off whole-string comparisons (branch-option selection, return-to-main
/// detection) that do not warrant a full windowed [`AlignmentEngine`] pass.
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
    fn word_level_matches_fragment_on_the_true_long_sentence() {
        // Real regression from a recorded read. While reading the long sentence
        // at idx 3, the recognizer re-segmented to the short fragment "between
        // your supplements that". The OLD char-bigram matcher scored that
        // fragment HIGHER on a coincidental short sentence 3 lines down (idx 6,
        // ~0.354) than on the true long sentence that literally contains those
        // words (idx 3, ~0.280), so the cursor jumped +3 and stuck. Word-level
        // alignment matches the actual words (between/your ... supplements/that,
        // bridging the intervening reference words as a gap), so it lands on the
        // TRUE sentence, not the coincidence.
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
        assert!(r.matched, "fragment should match the true sentence");
        assert_eq!(r.position, 3, "must land on the true sentence, not jump ahead");
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
