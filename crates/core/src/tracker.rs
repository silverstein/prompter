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
//!
//! Branches (Phase 2): each branch option's sentences are flattened into the
//! same timeline and aligner sentence list, tagged with their option. The
//! forward-biased windowed aligner therefore tracks *which* path the speaker
//! takes for free: reading option A advances the cursor into A's sentences;
//! resuming the main line afterwards skips the unread options (they sit inside
//! the search window). The tracker reports an [`TrackState::InBranch`] and a
//! one-shot `selected_branch_option` on entry. Caveat: the post-branch line must
//! lie within the aligner's window (~15 sentences) of the last branch sentence,
//! which holds for the short guidance branches the DSL is designed for; very
//! long options would need a larger window or explicit branch return points.

use crate::align::AlignmentEngine;
use crate::script::{Directive, Element, Script};
use crate::speech::SpeechUpdate;

/// One step in the flattened script timeline. Indices into the slice returned by
/// [`ScriptTracker::timeline`] are stable for the life of the tracker, so the UI
/// can map a step to a rendered element.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineStep {
    /// A section heading boundary.
    Section { name: String },
    /// A spoken sentence on the main line -- an alignment unit. `sentence_index`
    /// is its index in the flat sentence list handed to the aligner.
    Sentence { text: String, sentence_index: usize },
    /// A pause point: the teleprompter waits for the other party.
    Pause { prompt: String },
    /// A branch marker: one of several labelled paths is taken.
    Branch {
        question: String,
        options: Vec<String>,
    },
    /// A sentence that belongs to one branch option (also an alignment unit).
    BranchSentence {
        option_label: String,
        text: String,
        sentence_index: usize,
    },
}

/// Whether a flat sentence is on the main line or inside a branch option.
#[derive(Debug, Clone, PartialEq)]
enum SentenceKind {
    Main,
    Branch { option_label: String },
}

/// What the tracker believes is happening right now.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackState {
    /// Reading along normally on the main line.
    Speaking,
    /// The cursor reached the last sentence before a pause point.
    AtPause { prompt: String },
    /// The cursor reached the last sentence before a branch (deciding a path).
    AtBranch {
        question: String,
        options: Vec<String>,
    },
    /// The cursor is inside a selected branch option.
    InBranch { option_label: String },
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
    /// Set (once) on the committed update that first enters a branch option.
    pub selected_branch_option: Option<String>,
}

/// Tracks the speaker's position in a script from a stream of speech updates.
pub struct ScriptTracker {
    timeline: Vec<TimelineStep>,
    /// Maps a flat sentence index to its index in `timeline`.
    sentence_to_timeline: Vec<usize>,
    /// Maps a flat sentence index to whether it is on the main line or a branch.
    sentence_kinds: Vec<SentenceKind>,
    engine: AlignmentEngine,
    /// Committed sentence index (last confident final match).
    committed: usize,
    /// Preview sentence index from the latest partial (>= committed).
    preview: usize,
    /// The branch option label the committed cursor was last inside, if any.
    /// Used to fire `selected_branch_option` only on the entering update.
    current_option: Option<String>,
}

impl ScriptTracker {
    /// Build a tracker from a parsed script.
    pub fn new(script: &Script) -> Self {
        let mut timeline = Vec::new();
        let mut flat_sentences = Vec::new();
        let mut sentence_to_timeline = Vec::new();
        let mut sentence_kinds = Vec::new();

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
                            sentence_kinds.push(SentenceKind::Main);
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
                        for option in options {
                            for sentence in &option.sentences {
                                let sentence_index = flat_sentences.len();
                                sentence_to_timeline.push(timeline.len());
                                sentence_kinds.push(SentenceKind::Branch {
                                    option_label: option.label.clone(),
                                });
                                timeline.push(TimelineStep::BranchSentence {
                                    option_label: option.label.clone(),
                                    text: sentence.text.clone(),
                                    sentence_index,
                                });
                                flat_sentences.push(sentence.text.clone());
                            }
                        }
                    }
                }
            }
        }

        let engine = AlignmentEngine::new(flat_sentences);
        Self {
            timeline,
            sentence_to_timeline,
            sentence_kinds,
            engine,
            committed: 0,
            preview: 0,
            current_option: None,
        }
    }

    /// The flattened timeline (stable indices for rendering).
    pub fn timeline(&self) -> &[TimelineStep] {
        &self.timeline
    }

    /// Number of alignable sentences (main line + all branch options).
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
        self.current_option = self.option_at(self.committed);
    }

    /// Reset to the start of the script.
    pub fn reset(&mut self) {
        self.engine.set_position(0);
        self.committed = 0;
        self.preview = 0;
        self.current_option = None;
    }

    /// Observe one recognition update and return the resulting position/state.
    ///
    /// Final updates commit (cursor advances, ad-lib state updates, pause/branch
    /// may escalate, branch entry fires). Partial updates only move the forward
    /// preview cursor.
    pub fn observe(&mut self, update: &SpeechUpdate) -> TrackUpdate {
        if update.is_final {
            let result = self.engine.align(&update.text);
            self.committed = self.engine.position();
            self.preview = self.committed;

            // Detect entering a branch option (fire the one-shot selection).
            let option_now = self.option_at(self.committed);
            let selected = match (&self.current_option, &option_now) {
                (prev, Some(label)) if prev.as_ref() != Some(label) => Some(label.clone()),
                _ => None,
            };
            self.current_option = option_now;

            let state = if result.ad_libbing {
                TrackState::AdLibbing
            } else {
                self.state_at(self.committed)
            };
            let mut out = self.make_update(self.committed, true, result.confidence, state);
            out.selected_branch_option = selected;
            out
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

    /// The branch option label a flat sentence belongs to, or `None` for main.
    fn option_at(&self, sentence_index: usize) -> Option<String> {
        match self.sentence_kinds.get(sentence_index) {
            Some(SentenceKind::Branch { option_label }) => Some(option_label.clone()),
            _ => None,
        }
    }

    /// Derive state at a committed sentence: in-branch if it is a branch
    /// sentence, otherwise the pause/branch lookahead from the next timeline step.
    fn state_at(&self, sentence_index: usize) -> TrackState {
        if let Some(SentenceKind::Branch { option_label }) = self.sentence_kinds.get(sentence_index)
        {
            return TrackState::InBranch {
                option_label: option_label.clone(),
            };
        }
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
            selected_branch_option: None,
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

    /// Intro section: 2 sentences, a pause, 1 sentence, a branch (2 options,
    /// 1 sentence each), 1 closing sentence.
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
                                sentences: vec![sent("great lets discuss your concerns in detail")],
                            },
                            BranchOption {
                                label: "NO".into(),
                                sentences: vec![sent("okay then lets keep moving along")],
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
    fn flattens_main_and_branch_sentences() {
        let t = ScriptTracker::new(&sample_script());
        // Section, 2 sentences, Pause, 1 sentence, Branch marker, 2 branch
        // sentences, 1 closing sentence = 9 steps.
        assert_eq!(t.timeline().len(), 9);
        // 3 main + 2 branch + 1 closing = 6 alignable sentences.
        assert_eq!(t.sentence_count(), 6);
        assert!(matches!(t.timeline()[3], TimelineStep::Pause { .. }));
        assert!(matches!(t.timeline()[5], TimelineStep::Branch { .. }));
        assert!(matches!(
            t.timeline()[6],
            TimelineStep::BranchSentence { .. }
        ));
    }

    #[test]
    fn final_commits_and_advances() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        assert!(u.committed);
        assert_eq!(u.sentence_index, 2);
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
    }

    #[test]
    fn reaching_sentence_before_pause_reports_pause() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "my goal is simple to make sure every medication youre taking is safe",
        ));
        assert_eq!(u.sentence_index, 1);
        assert!(matches!(u.state, TrackState::AtPause { .. }));
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
    fn taking_a_branch_reports_selection_and_in_branch() {
        let mut t = ScriptTracker::new(&sample_script());
        // Walk up to the branch.
        t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        // Speaker reads the NO option.
        let u = t.observe(&SpeechUpdate::finalized("okay then lets keep moving along"));
        assert_eq!(
            u.selected_branch_option,
            Some("NO".to_string()),
            "entering a branch option should fire the selection once"
        );
        assert_eq!(
            u.state,
            TrackState::InBranch {
                option_label: "NO".into()
            }
        );
    }

    #[test]
    fn branch_selection_fires_once_then_returns_to_main() {
        let mut t = ScriptTracker::new(&sample_script());
        t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        let enter = t.observe(&SpeechUpdate::finalized(
            "great lets discuss your concerns in detail",
        ));
        assert_eq!(enter.selected_branch_option, Some("YES".to_string()));

        // Resuming the main line after the branch: no new selection, back to
        // Speaking, and the cursor leaves the branch.
        let resume = t.observe(&SpeechUpdate::finalized("does this make sense so far"));
        assert_eq!(resume.selected_branch_option, None);
        assert_eq!(resume.state, TrackState::Speaking);
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
