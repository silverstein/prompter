//! Script position tracker.
//!
//! Turns a parsed [`Script`] into a flat, indexable timeline and drives an
//! [`AlignmentEngine`] from a provider-agnostic stream of [`SpeechUpdate`]s.
//! This is the canonical, platform-independent replacement for the ad-hoc
//! substring matcher that lived in the UI: alignment is decided in Rust, in one
//! tested place, and the app only renders the resulting position + state.
//!
//! De-flicker rule (see `docs/UPGRADE-2026.md`, D5): partial/volatile
//! hypotheses move only a *preview* cursor (forward, non-committing) via
//! [`AlignmentEngine::peek`]; stabilized/final hypotheses commit via
//! [`AlignmentEngine::align`]. Pause/branch transitions are reported only on
//! committed updates so a revised partial cannot trigger a spurious pause.

use crate::align::AlignmentEngine;
use crate::script::{Directive, Element, Script};
use crate::speech::SpeechUpdate;

/// One step in the flattened script timeline. Indices into the `Vec` returned
/// by [`ScriptTracker::timeline`] are stable for the life of the tracker, so the
/// UI can map a step to a rendered element.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineStep {
    /// A section heading boundary.
    Section { name: String },
    /// A spoken sentence -- the alignment unit. `sentence_index` is its index in
    /// the flat sentence list handed to the aligner.
    Sentence { text: String, sentence_index: usize },
    /// A pause point: the teleprompter waits for the other party.
    Pause { prompt: String },
    /// A branch: one of several labelled paths is taken. (Speech-driven
    /// selection is Phase 2; here we surface that a branch was reached and its
    /// option labels.)
    Branch {
        question: String,
        options: Vec<String>,
    },
}

/// What the tracker believes is happening right now.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackState {
    /// Reading along normally.
    Speaking,
    /// The cursor reached the last sentence before a pause point.
    AtPause { prompt: String },
    /// The cursor reached the last sentence before a branch.
    AtBranch {
        question: String,
        options: Vec<String>,
    },
    /// No confident match for a sustained run -- speaker may be off-script.
    AdLibbing,
}

/// The result of observing one [`SpeechUpdate`].
#[derive(Debug, Clone, PartialEq)]
pub struct TrackUpdate {
    /// Current best sentence index (preview during partials, committed on final).
    pub sentence_index: usize,
    /// Timeline index of that sentence, for UI rendering.
    pub timeline_index: usize,
    /// True when this came from a committed (final) match; false for a preview.
    pub committed: bool,
    /// Match confidence (0.0-1.0).
    pub confidence: f32,
    /// Current state (pause/branch only escalate on committed updates).
    pub state: TrackState,
}

/// Tracks the speaker's position in a script from a stream of speech updates.
pub struct ScriptTracker {
    timeline: Vec<TimelineStep>,
    /// Maps a flat sentence index to its index in `timeline`.
    sentence_to_timeline: Vec<usize>,
    engine: AlignmentEngine,
    /// Committed sentence index (last confident final match).
    committed: usize,
    /// Preview sentence index from the latest partial (>= committed).
    preview: usize,
}

impl ScriptTracker {
    /// Build a tracker from a parsed script.
    pub fn new(script: &Script) -> Self {
        let mut timeline = Vec::new();
        let mut flat_sentences = Vec::new();
        let mut sentence_to_timeline = Vec::new();

        for section in &script.sections {
            timeline.push(TimelineStep::Section {
                name: section.name.clone(),
            });
            for element in &section.elements {
                match element {
                    Element::Text(sentences) => {
                        for sentence in sentences {
                            let sentence_index = flat_sentences.len();
                            sentence_to_timeline.push(timeline.len());
                            timeline.push(TimelineStep::Sentence {
                                text: sentence.text.clone(),
                                sentence_index,
                            });
                            flat_sentences.push(sentence.text.clone());
                        }
                    }
                    Element::Directive(Directive::Pause { prompt }) => {
                        timeline.push(TimelineStep::Pause {
                            prompt: prompt.clone(),
                        });
                    }
                    Element::Directive(Directive::Branch { question, options }) => {
                        timeline.push(TimelineStep::Branch {
                            question: question.clone(),
                            options: options.iter().map(|o| o.label.clone()).collect(),
                        });
                    }
                }
            }
        }

        let engine = AlignmentEngine::new(flat_sentences);
        Self {
            timeline,
            sentence_to_timeline,
            engine,
            committed: 0,
            preview: 0,
        }
    }

    /// The flattened timeline (stable indices for rendering).
    pub fn timeline(&self) -> &[TimelineStep] {
        &self.timeline
    }

    /// Number of alignable sentences.
    pub fn sentence_count(&self) -> usize {
        self.sentence_to_timeline.len()
    }

    /// Committed sentence index.
    pub fn position(&self) -> usize {
        self.committed
    }

    /// Preview (tentative) sentence index from the latest partial.
    pub fn preview_position(&self) -> usize {
        self.preview
    }

    /// Manually move the cursor (e.g. the operator scrolls/clicks).
    pub fn set_position(&mut self, sentence_index: usize) {
        self.engine.set_position(sentence_index);
        self.committed = self.engine.position();
        self.preview = self.committed;
    }

    /// Reset to the start of the script.
    pub fn reset(&mut self) {
        self.engine.set_position(0);
        self.committed = 0;
        self.preview = 0;
    }

    /// Observe one recognition update and return the resulting position/state.
    ///
    /// Final updates commit (cursor advances, ad-lib state updates, pause/branch
    /// may escalate). Partial updates only move the forward preview cursor.
    pub fn observe(&mut self, update: &SpeechUpdate) -> TrackUpdate {
        if update.is_final {
            let result = self.engine.align(&update.text);
            self.committed = self.engine.position();
            self.preview = self.committed;
            let state = if result.ad_libbing {
                TrackState::AdLibbing
            } else {
                self.state_after(self.committed)
            };
            self.make_update(self.committed, true, result.confidence, state)
        } else {
            let result = self.engine.peek(&update.text);
            // Forward-only preview: never pull the cursor backward on a volatile
            // hypothesis, and never commit it.
            self.preview = if result.matched && result.position > self.committed {
                result.position
            } else {
                self.committed
            };
            let state = if result.ad_libbing {
                TrackState::AdLibbing
            } else {
                TrackState::Speaking
            };
            self.make_update(self.preview, false, result.confidence, state)
        }
    }

    /// Derive pause/branch state from the step immediately after a sentence.
    fn state_after(&self, sentence_index: usize) -> TrackState {
        let Some(&tl) = self.sentence_to_timeline.get(sentence_index) else {
            return TrackState::Speaking;
        };
        match self.timeline.get(tl + 1) {
            Some(TimelineStep::Pause { prompt }) => TrackState::AtPause {
                prompt: prompt.clone(),
            },
            Some(TimelineStep::Branch { question, options }) => TrackState::AtBranch {
                question: question.clone(),
                options: options.clone(),
            },
            _ => TrackState::Speaking,
        }
    }

    fn make_update(
        &self,
        sentence_index: usize,
        committed: bool,
        confidence: f32,
        state: TrackState,
    ) -> TrackUpdate {
        let timeline_index = self
            .sentence_to_timeline
            .get(sentence_index)
            .copied()
            .unwrap_or(0);
        TrackUpdate {
            sentence_index,
            timeline_index,
            committed,
            confidence,
            state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::{BranchOption, Frontmatter, Section, Sentence};

    fn sent(text: &str) -> Sentence {
        Sentence {
            text: text.into(),
            word_count: text.split_whitespace().count(),
        }
    }

    /// Intro section: 2 sentences, a pause, 1 sentence, a branch, 1 sentence.
    fn sample_script() -> Script {
        Script {
            frontmatter: Frontmatter {
                title: "Test".into(),
                r#type: None,
                version: None,
                variables: Default::default(),
                estimated_duration: None,
            },
            sections: vec![Section {
                name: "Intro".into(),
                word_count: 0,
                elements: vec![
                    Element::Text(vec![
                        sent("hi thanks for meeting with me today"),
                        sent(
                            "my goal is simple to make sure every medication youre taking is safe",
                        ),
                    ]),
                    Element::Directive(Directive::Pause {
                        prompt: "wait for response".into(),
                    }),
                    Element::Text(vec![sent(
                        "you are currently taking warfarin and metformin",
                    )]),
                    Element::Directive(Directive::Branch {
                        question: "any questions".into(),
                        options: vec![
                            BranchOption {
                                label: "YES".into(),
                                sentences: vec![sent("great lets discuss")],
                            },
                            BranchOption {
                                label: "NO".into(),
                                sentences: vec![sent("okay moving on")],
                            },
                        ],
                    }),
                    Element::Text(vec![sent("does this make sense so far")]),
                ],
            }],
            word_count: 0,
        }
    }

    #[test]
    fn flattens_timeline_and_sentences() {
        let t = ScriptTracker::new(&sample_script());
        // Section, 2 sentences, Pause, 1 sentence, Branch, 1 sentence = 7 steps.
        assert_eq!(t.timeline().len(), 7);
        assert_eq!(t.sentence_count(), 4);
        assert!(matches!(t.timeline()[0], TimelineStep::Section { .. }));
        assert!(matches!(t.timeline()[3], TimelineStep::Pause { .. }));
        assert!(matches!(t.timeline()[5], TimelineStep::Branch { .. }));
    }

    #[test]
    fn final_commits_and_advances() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        assert!(u.committed);
        assert_eq!(u.sentence_index, 2);
        assert_eq!(u.timeline_index, 4);
        assert_eq!(t.position(), 2);
    }

    #[test]
    fn partial_previews_without_committing() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::partial(
            "you are currently taking warfarin and metformin",
        ));
        assert!(!u.committed);
        assert_eq!(u.sentence_index, 2, "preview should jump forward");
        assert_eq!(
            t.position(),
            0,
            "committed cursor must not move on a partial"
        );
        assert_eq!(t.preview_position(), 2);

        // A following final on the same text commits it.
        let f = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        assert!(f.committed);
        assert_eq!(t.position(), 2);
    }

    #[test]
    fn reaching_sentence_before_pause_reports_pause() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "my goal is simple to make sure every medication youre taking is safe",
        ));
        assert_eq!(u.sentence_index, 1);
        match u.state {
            TrackState::AtPause { prompt } => assert_eq!(prompt, "wait for response"),
            other => panic!("expected AtPause, got {other:?}"),
        }
    }

    #[test]
    fn reaching_sentence_before_branch_reports_branch() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        match u.state {
            TrackState::AtBranch { question, options } => {
                assert_eq!(question, "any questions");
                assert_eq!(options, vec!["YES".to_string(), "NO".to_string()]);
            }
            other => panic!("expected AtBranch, got {other:?}"),
        }
    }

    #[test]
    fn partial_does_not_escalate_pause() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::partial(
            "my goal is simple to make sure every medication youre taking is safe",
        ));
        // Preview reaches the pre-pause sentence, but a volatile hypothesis must
        // not announce the pause.
        assert_eq!(u.state, TrackState::Speaking);
    }

    #[test]
    fn manual_set_position_and_reset() {
        let mut t = ScriptTracker::new(&sample_script());
        t.set_position(2);
        assert_eq!(t.position(), 2);
        t.reset();
        assert_eq!(t.position(), 0);
        assert_eq!(t.preview_position(), 0);
    }
}
