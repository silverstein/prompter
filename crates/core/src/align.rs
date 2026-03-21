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
            window_radius: 15,  // Search ±15 sentences (~30 sentence window)
            threshold: 0.35,    // 35% bigram overlap required
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

    /// Attempt to align recognized text against the script.
    /// Returns the alignment result with the best matching position.
    pub fn align(&mut self, recognized: &str) -> AlignResult {
        let recognized = normalize(recognized);

        if recognized.len() < 10 {
            // Too short to match reliably
            return AlignResult {
                matched: false,
                position: self.cursor,
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            };
        }

        let rec_bigrams = bigrams(&recognized);
        if rec_bigrams.is_empty() {
            return AlignResult {
                matched: false,
                position: self.cursor,
                confidence: 0.0,
                ad_libbing: self.miss_count > 5,
            };
        }

        let n = self.sentences.len();
        let start = self.cursor.saturating_sub(self.window_radius);
        let end = (self.cursor + self.window_radius + 1).min(n);

        let mut best_score: f32 = 0.0;
        let mut best_pos = self.cursor;

        // Try matching against individual sentences and 2-3 sentence windows
        for i in start..end {
            // Single sentence
            let score = bigram_similarity(&rec_bigrams, &self.sentences[i]);
            if score > best_score {
                best_score = score;
                best_pos = i;
            }

            // Two-sentence window (current + next)
            if i + 1 < n {
                let combined = format!("{} {}", self.sentences[i], self.sentences[i + 1]);
                let score = bigram_similarity(&rec_bigrams, &combined);
                if score > best_score {
                    best_score = score;
                    best_pos = i;
                }
            }

            // Three-sentence window
            if i + 2 < n {
                let combined = format!(
                    "{} {} {}",
                    self.sentences[i],
                    self.sentences[i + 1],
                    self.sentences[i + 2]
                );
                let score = bigram_similarity(&rec_bigrams, &combined);
                if score > best_score {
                    best_score = score;
                    best_pos = i;
                }
            }
        }

        if best_score >= self.threshold {
            // Good match — update cursor
            // Only advance forward or allow small backward jumps (re-reading)
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
    fn no_match_for_adlib() {
        let mut eng = make_engine();
        // Use text with zero overlap with pharmacy script vocabulary
        let r = eng.align("jupyter notebook crashed during pytorch backpropagation gradient descent");
        assert!(!r.matched, "unrelated tech text should not match pharmacy script, confidence: {}", r.confidence);
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
        assert_eq!(normalize("You're taking Warfarin."), "you re taking warfarin");
    }

    #[test]
    fn bigram_similarity_identical() {
        let bg = bigrams("hello world");
        let score = bigram_similarity(&bg, "hello world");
        assert!((score - 1.0).abs() < 0.01, "identical text should score ~1.0, got {}", score);
    }

    #[test]
    fn bigram_similarity_different() {
        let bg = bigrams("hello world");
        let score = bigram_similarity(&bg, "goodbye universe");
        assert!(score < 0.3, "different text should score low, got {}", score);
    }
}
