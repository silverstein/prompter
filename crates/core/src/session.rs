//! Session recorder: speech-verified coverage + transcript.
//!
//! Consumes the same [`TrackUpdate`] stream the UI renders (plus the recognized
//! text) and accumulates *evidence* of what was actually said, so the compliance
//! report reflects delivery rather than cursor position. The old path derived
//! coverage from the raw cursor index, so a single forward jump marked every
//! earlier sentence "covered"; here a main sentence counts as covered only when
//! a committed match actually landed on it. Also captures the branch path taken,
//! which pause points were reached, and the recognized transcript (which the
//! prior pipeline never persisted). See `docs/UPGRADE-2026.md`, D6.

use crate::compliance::ComplianceReport;
use crate::script::{Directive, Element, Script};
use crate::tracker::{TrackState, TrackUpdate};
use std::collections::{HashMap, HashSet};

/// One recognized, committed line of the session transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptLine {
    /// Main-line sentence index the matcher committed to.
    pub sentence_index: usize,
    /// The recognized text that produced this commit.
    pub recognized: String,
    /// Whether this line entered a branch option (and which).
    pub branch_option: Option<String>,
}

/// Accumulates speech evidence across a session.
pub struct SessionRecorder {
    script_title: String,
    script_version: Option<String>,
    /// Section index for each main sentence.
    sentence_section: Vec<usize>,
    /// Word count for each main sentence.
    sentence_words: Vec<usize>,
    section_names: Vec<String>,
    pause_total: usize,
    total_words: usize,

    /// Whether each main sentence has committed evidence of being spoken.
    covered: Vec<bool>,
    /// Distinct pause points reached, keyed by the preceding main sentence.
    pauses_reached: HashSet<usize>,
    /// Branch question -> chosen option label.
    branches_taken: HashMap<String, String>,
    transcript: Vec<TranscriptLine>,
}

impl SessionRecorder {
    /// Build a recorder for a script.
    pub fn new(script: &Script) -> Self {
        let mut sentence_section = Vec::new();
        let mut sentence_words = Vec::new();
        let mut section_names = Vec::new();
        let mut pause_total = 0usize;
        let mut total_words = 0usize;

        for (section_index, section) in script.sections.iter().enumerate() {
            section_names.push(section.name.clone());
            // A section heading breaks adjacency, mirroring the tracker: a pause
            // is "reachable" only when a main sentence immediately precedes it.
            let mut prev_was_main = false;
            for element in &section.elements {
                match element {
                    Element::Text(sentences) => {
                        for sentence in sentences {
                            sentence_section.push(section_index);
                            sentence_words.push(sentence.word_count);
                            total_words += sentence.word_count;
                        }
                        prev_was_main = !sentences.is_empty();
                    }
                    // Count only pauses the tracker can actually reach (those
                    // immediately after a main sentence), so reached/total is an
                    // honest ratio. Consecutive or branch-adjacent pauses are a
                    // known limitation (see docs/UPGRADE-2026.md).
                    Element::Directive(Directive::Pause { .. }) => {
                        if prev_was_main {
                            pause_total += 1;
                        }
                        prev_was_main = false;
                    }
                    // Branch option words are optional paths and are not counted
                    // in main-line adherence (consistent with the tracker, which
                    // keeps branch sentences out of the main aligner).
                    Element::Directive(Directive::Branch { .. }) => {
                        prev_was_main = false;
                    }
                }
            }
        }

        let covered = vec![false; sentence_section.len()];
        Self {
            script_title: script.frontmatter.title.clone(),
            script_version: script.frontmatter.version.clone(),
            sentence_section,
            sentence_words,
            section_names,
            pause_total,
            total_words,
            covered,
            pauses_reached: HashSet::new(),
            branches_taken: HashMap::new(),
            transcript: Vec::new(),
        }
    }

    /// Record one committed update and the text that produced it. Partial
    /// (preview) updates carry no evidence and are ignored.
    pub fn record(&mut self, update: &TrackUpdate, recognized: &str) {
        if !update.committed {
            return;
        }

        // Coverage / pause evidence counts ONLY on a real match. An unmatched
        // committed final (off-script speech, a short utterance) must not mark a
        // sentence delivered -- that was the over-count the cursor-based path had.
        if update.matched {
            match &update.state {
                TrackState::Speaking | TrackState::AtBranch { .. } => {
                    if update.sentence_index < self.covered.len() {
                        self.covered[update.sentence_index] = true;
                    }
                }
                TrackState::AtPause { .. } => {
                    if update.sentence_index < self.covered.len() {
                        self.covered[update.sentence_index] = true;
                        self.pauses_reached.insert(update.sentence_index);
                    }
                }
                // In-branch / ad-lib updates do not mark a main sentence covered.
                TrackState::InBranch { .. } | TrackState::AdLibbing => {}
            }
        }

        // Branch attribution comes straight from the selecting update -- no
        // stateful reconstruction, so set_position-entered or same-update
        // selections are not missed and stale questions cannot mis-attribute.
        if let Some(choice) = &update.branch_choice {
            self.branches_taken
                .insert(choice.question.clone(), choice.option_label.clone());
        }

        // The transcript records every committed final (including off-script
        // speech) for a complete record; coverage above is what is gated.
        self.transcript.push(TranscriptLine {
            sentence_index: update.sentence_index,
            recognized: recognized.to_string(),
            branch_option: update
                .branch_choice
                .as_ref()
                .map(|c| c.option_label.clone()),
        });
    }

    /// Words with committed evidence of delivery.
    pub fn words_delivered(&self) -> usize {
        self.sentence_words
            .iter()
            .zip(&self.covered)
            .filter(|(_, &c)| c)
            .map(|(w, _)| *w)
            .sum()
    }

    /// Sections with at least one covered sentence, and those with none.
    fn section_split(&self) -> (Vec<String>, Vec<String>) {
        let mut any_covered = vec![false; self.section_names.len()];
        for (sentence_idx, &section_idx) in self.sentence_section.iter().enumerate() {
            if self.covered[sentence_idx] {
                any_covered[section_idx] = true;
            }
        }
        let mut covered = Vec::new();
        let mut skipped = Vec::new();
        for (i, name) in self.section_names.iter().enumerate() {
            if any_covered[i] {
                covered.push(name.clone());
            } else {
                skipped.push(name.clone());
            }
        }
        (covered, skipped)
    }

    /// Build a speech-verified compliance report. `duration_secs` comes from the
    /// caller (core has no clock).
    pub fn build_report(&self, duration_secs: u64) -> ComplianceReport {
        let (sections_covered, sections_skipped) = self.section_split();
        ComplianceReport {
            script_title: self.script_title.clone(),
            script_version: self.script_version.clone(),
            sections_covered,
            sections_skipped,
            duration_secs,
            section_times: HashMap::new(),
            pause_points_reached: self.pauses_reached.len(),
            pause_points_total: self.pause_total,
            branches_taken: self.branches_taken.clone(),
            total_words: self.total_words,
            words_delivered: self.words_delivered(),
        }
    }

    /// Render the recognized transcript as a portable markdown artifact.
    pub fn transcript_markdown(&self) -> String {
        let mut out = format!("# Transcript — {}\n\n", self.script_title);
        if self.transcript.is_empty() {
            out.push_str("_No recognized speech recorded._\n");
            return out;
        }
        for line in &self.transcript {
            if let Some(label) = &line.branch_option {
                out.push_str(&format!("- [branch: {label}] {}\n", line.recognized));
            } else {
                out.push_str(&format!("- {}\n", line.recognized));
            }
        }
        out
    }

    /// The recorded transcript lines.
    pub fn transcript(&self) -> &[TranscriptLine] {
        &self.transcript
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{BranchOption, Frontmatter, Section, Sentence};
    use crate::speech::SpeechUpdate;
    use crate::tracker::ScriptTracker;

    fn sent(text: &str) -> Sentence {
        Sentence {
            text: text.into(),
            word_count: text.split_whitespace().count(),
        }
    }

    fn sample_script() -> Script {
        Script {
            frontmatter: Frontmatter {
                title: "Consult".into(),
                r#type: None,
                version: Some("1".into()),
                variables: Default::default(),
                estimated_duration: None,
            },
            sections: vec![
                Section {
                    name: "Intro".into(),
                    word_count: 0,
                    elements: vec![
                        Element::Text(vec![
                            sent("hi thanks for meeting with me today"),
                            sent("my goal is simple to keep your medications safe"),
                        ]),
                        Element::Directive(Directive::Pause {
                            prompt: "wait".into(),
                        }),
                    ],
                },
                Section {
                    name: "Review".into(),
                    word_count: 0,
                    elements: vec![
                        Element::Text(vec![sent(
                            "you are currently taking warfarin and metformin",
                        )]),
                        Element::Directive(Directive::Branch {
                            question: "any questions".into(),
                            options: vec![
                                BranchOption {
                                    label: "YES".into(),
                                    sentences: vec![sent("great lets discuss your concerns now")],
                                },
                                BranchOption {
                                    label: "NO".into(),
                                    sentences: vec![sent("okay then lets keep moving along")],
                                },
                            ],
                        }),
                        Element::Text(vec![sent("does this all make sense so far")]),
                    ],
                },
            ],
            word_count: 0,
        }
    }

    /// Feed a sequence of finals through a tracker into a recorder.
    fn run(updates: &[&str]) -> (SessionRecorder, Script) {
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        for &text in updates {
            let u = tracker.observe(&SpeechUpdate::finalized(text));
            rec.record(&u, text);
        }
        (rec, script)
    }

    #[test]
    fn coverage_is_speech_verified_not_cursor_jump() {
        // Speaker reads only the first sentence, then jumps to the review line,
        // skipping sentence 2. Sentence 2 must NOT count as covered.
        let (rec, _) = run(&[
            "hi thanks for meeting with me today",
            "you are currently taking warfarin and metformin",
        ]);
        // Covered: sentence 0 (7 words) + sentence 2 (7 words) = 14. Sentence 1
        // (the skipped one, 9 words) is excluded.
        let report = rec.build_report(60);
        assert_eq!(report.words_delivered, 7 + 7);
        assert!(report.total_words > report.words_delivered);
    }

    #[test]
    fn sections_split_by_evidence() {
        let (rec, _) = run(&["hi thanks for meeting with me today"]);
        let report = rec.build_report(10);
        assert_eq!(report.sections_covered, vec!["Intro".to_string()]);
        assert_eq!(report.sections_skipped, vec!["Review".to_string()]);
    }

    #[test]
    fn pause_reached_is_counted() {
        let (rec, _) = run(&["my goal is simple to keep your medications safe"]);
        let report = rec.build_report(10);
        assert_eq!(report.pause_points_total, 1);
        assert_eq!(report.pause_points_reached, 1);
    }

    #[test]
    fn branch_path_is_recorded() {
        let (rec, _) = run(&[
            "you are currently taking warfarin and metformin",
            "okay then lets keep moving along",
        ]);
        let report = rec.build_report(20);
        assert_eq!(
            report.branches_taken.get("any questions"),
            Some(&"NO".to_string())
        );
    }

    #[test]
    fn transcript_is_persisted() {
        let (rec, _) = run(&[
            "you are currently taking warfarin and metformin",
            "great lets discuss your concerns now",
        ]);
        let md = rec.transcript_markdown();
        assert!(md.contains("warfarin and metformin"));
        assert!(md.contains("[branch: YES]"));
        assert_eq!(rec.transcript().len(), 2);
    }

    #[test]
    fn partials_carry_no_evidence() {
        let script = sample_script();
        let mut tracker = ScriptTracker::new(&script);
        let mut rec = SessionRecorder::new(&script);
        let u = tracker.observe(&SpeechUpdate::partial(
            "hi thanks for meeting with me today",
        ));
        rec.record(&u, "hi thanks for meeting with me today");
        assert_eq!(rec.build_report(5).words_delivered, 0);
        assert!(rec.transcript().is_empty());
    }
}
