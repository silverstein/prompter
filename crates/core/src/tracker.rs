//! Script position tracker.
//!
//! Turns a parsed [`Script`] into a flat, indexable timeline and drives an
//! [`AlignmentEngine`] from a provider-agnostic stream of [`SpeechUpdate`]s.
//! This is the canonical, platform-independent replacement for the ad-hoc
//! substring matcher that lived in the UI: alignment is decided in Rust, in one
//! tested place, and the app only renders the resulting position + state.
//!
//! Rules that drive it (see `docs/UPGRADE-2026.md`, D5):
//!
//! 1. **De-flicker.** Partial/volatile hypotheses move only a forward,
//!    non-committing *preview* cursor (via [`AlignmentEngine::peek`]); finals
//!    commit (via [`AlignmentEngine::align`]). The preview is monotonic between
//!    commits and a partial never state-downgrades a committed pause/branch.
//!
//! 2. **Evidence.** Every [`TrackUpdate`] carries `matched`: whether this update
//!    is real alignment evidence (a confident match / a branch selection / a
//!    branch return). A final that does not confidently match still reports a
//!    committed update (so the UI stays put) but with `matched == false`, so
//!    downstream consumers (the session recorder) do not count it as delivered.
//!
//! 3. **Branches are a tree, not a line.** Only the main-line sentences go into
//!    the windowed aligner. Branch options are mutually exclusive, so a small
//!    state machine -- Linear -> AwaitingBranch -> InBranch -> Linear -- scores
//!    each branch's options in isolation and detects the return to the main
//!    line. A branch selection carries its [`BranchChoice`] (question + label)
//!    so attribution needs no external state.

use crate::align::{self, AlignmentEngine};
use crate::script::{Directive, Element, Script};
use crate::speech::SpeechUpdate;
use std::collections::HashMap;

/// Minimum margin by which the best branch option must beat the runner-up to be
/// selected (on top of clearing [`align::MATCH_THRESHOLD`]).
const BRANCH_MARGIN: f32 = 0.10;

/// One step in the flattened script timeline. Indices into the slice returned by
/// [`ScriptTracker::timeline`] are stable for the life of the tracker.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineStep {
    /// A section heading boundary.
    Section { name: String },
    /// A main-line sentence. `sentence_index` is its index in the aligner list.
    Sentence { text: String, sentence_index: usize },
    /// A pause point: the teleprompter waits for the other party.
    Pause { prompt: String },
    /// A branch marker: one of several labelled paths is taken.
    Branch {
        question: String,
        options: Vec<String>,
    },
    /// A sentence belonging to one branch option (rendered, not in the aligner).
    BranchSentence { option_label: String, text: String },
}

/// What the tracker believes is happening right now.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackState {
    /// Reading along normally on the main line.
    Speaking,
    /// The cursor reached the last main sentence before a pause point.
    AtPause { prompt: String },
    /// The cursor reached a branch and is deciding which path to take.
    AtBranch {
        question: String,
        options: Vec<String>,
    },
    /// A branch option was selected; reading its text.
    InBranch { option_label: String },
    /// No confident match for a sustained run -- speaker may be off-script.
    AdLibbing,
}

/// A selected branch path, carried on the selecting update so attribution needs
/// no external state.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchChoice {
    pub question: String,
    pub option_label: String,
}

/// The result of observing one [`SpeechUpdate`].
#[derive(Debug, Clone, PartialEq)]
pub struct TrackUpdate {
    /// Best main-line sentence index (preview during partials, committed on
    /// final). While in a branch this stays at the pre-branch sentence.
    pub sentence_index: usize,
    /// Timeline index to highlight (a main sentence, or the selected branch
    /// option's first sentence while in a branch).
    pub timeline_index: usize,
    /// True when this came from a final (vs a partial preview).
    pub committed: bool,
    /// True when this update is real alignment evidence (confident match,
    /// branch selection, or branch return). Unmatched finals are `false`.
    pub matched: bool,
    /// Match confidence (0.0-1.0).
    pub confidence: f32,
    /// Current state (pause/branch only escalate on committed updates).
    pub state: TrackState,
    /// Set on the committed update that selects a branch option.
    pub branch_choice: Option<BranchChoice>,
}

struct BranchData {
    question: String,
    option_labels: Vec<String>,
    options: Vec<BranchOptionData>,
    /// First main-line sentence after the branch (return point). `None` if last.
    post_main: Option<usize>,
}

struct BranchOptionData {
    label: String,
    joined: String,
    first_timeline: Option<usize>,
}

/// The tracker's position in the branch tree.
#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Linear,
    AwaitingBranch { branch: usize },
    InBranch { branch: usize, option: usize },
}

/// Tracks the speaker's position in a script from a stream of speech updates.
pub struct ScriptTracker {
    timeline: Vec<TimelineStep>,
    main_sentences: Vec<String>,
    main_to_timeline: Vec<usize>,
    branches: Vec<BranchData>,
    /// Main sentence index -> branch that immediately follows it.
    branch_after: HashMap<usize, usize>,
    engine: AlignmentEngine,
    mode: Mode,
    committed: usize,
    preview: usize,
    committed_timeline: usize,
    committed_state: TrackState,
}

impl ScriptTracker {
    /// Build a tracker from a parsed script.
    pub fn new(script: &Script) -> Self {
        let mut timeline = Vec::new();
        let mut main_sentences = Vec::new();
        let mut main_to_timeline = Vec::new();
        let mut branches: Vec<BranchData> = Vec::new();
        let mut branch_after = HashMap::new();
        let mut pending_post: Vec<usize> = Vec::new();
        let mut last_main: Option<usize> = None;

        for section in &script.sections {
            timeline.push(TimelineStep::Section {
                name: section.name.clone(),
            });
            for element in &section.elements {
                match element {
                    Element::Text(sentences) => {
                        for sentence in sentences {
                            let main_index = main_sentences.len();
                            for branch in pending_post.drain(..) {
                                branches[branch].post_main = Some(main_index);
                            }
                            main_to_timeline.push(timeline.len());
                            timeline.push(TimelineStep::Sentence {
                                text: sentence.text.clone(),
                                sentence_index: main_index,
                            });
                            main_sentences.push(sentence.text.clone());
                            last_main = Some(main_index);
                        }
                    }
                    Element::Directive(Directive::Pause { prompt }) => {
                        timeline.push(TimelineStep::Pause {
                            prompt: prompt.clone(),
                        });
                    }
                    Element::Directive(Directive::Branch { question, options }) => {
                        let branch_id = branches.len();
                        timeline.push(TimelineStep::Branch {
                            question: question.clone(),
                            options: options.iter().map(|o| o.label.clone()).collect(),
                        });
                        let mut option_data = Vec::new();
                        for option in options {
                            let mut first_timeline = None;
                            for sentence in &option.sentences {
                                if first_timeline.is_none() {
                                    first_timeline = Some(timeline.len());
                                }
                                timeline.push(TimelineStep::BranchSentence {
                                    option_label: option.label.clone(),
                                    text: sentence.text.clone(),
                                });
                            }
                            option_data.push(BranchOptionData {
                                label: option.label.clone(),
                                joined: option
                                    .sentences
                                    .iter()
                                    .map(|s| s.text.clone())
                                    .collect::<Vec<_>>()
                                    .join(" "),
                                first_timeline,
                            });
                        }
                        branches.push(BranchData {
                            question: question.clone(),
                            option_labels: options.iter().map(|o| o.label.clone()).collect(),
                            options: option_data,
                            post_main: None,
                        });
                        if let Some(trigger) = last_main {
                            branch_after.insert(trigger, branch_id);
                        }
                        pending_post.push(branch_id);
                    }
                }
            }
        }

        let committed_timeline = main_to_timeline.first().copied().unwrap_or(0);
        let engine = AlignmentEngine::new(main_sentences.clone());
        Self {
            timeline,
            main_sentences,
            main_to_timeline,
            branches,
            branch_after,
            engine,
            mode: Mode::Linear,
            committed: 0,
            preview: 0,
            committed_timeline,
            committed_state: TrackState::Speaking,
        }
    }

    /// The flattened timeline (stable indices for rendering).
    pub fn timeline(&self) -> &[TimelineStep] {
        &self.timeline
    }

    /// Number of alignable main-line sentences.
    pub fn sentence_count(&self) -> usize {
        self.main_sentences.len()
    }

    /// Committed main-line sentence index.
    pub fn position(&self) -> usize {
        self.committed
    }

    /// Preview (tentative) main-line sentence index from the latest partial.
    pub fn preview_position(&self) -> usize {
        self.preview
    }

    /// Manually move the cursor; re-derives linear/awaiting-branch mode + state.
    pub fn set_position(&mut self, sentence_index: usize) {
        self.engine.set_position(sentence_index);
        self.commit_linear();
    }

    /// Reset to the start of the script.
    pub fn reset(&mut self) {
        self.engine.set_position(0);
        self.committed = 0;
        self.preview = 0;
        self.mode = Mode::Linear;
        self.committed_state = TrackState::Speaking;
        self.committed_timeline = self.main_to_timeline.first().copied().unwrap_or(0);
    }

    /// Observe one recognition update and return the resulting position/state.
    pub fn observe(&mut self, update: &SpeechUpdate) -> TrackUpdate {
        if self.main_sentences.is_empty() {
            return TrackUpdate {
                sentence_index: 0,
                timeline_index: 0,
                committed: update.is_final,
                matched: false,
                confidence: 0.0,
                state: TrackState::Speaking,
                branch_choice: None,
            };
        }
        if update.is_final {
            self.observe_final(&update.text)
        } else {
            self.observe_partial(&update.text)
        }
    }

    fn observe_final(&mut self, text: &str) -> TrackUpdate {
        // Inside a branch: watch for the return to the main line.
        if let Mode::InBranch { branch, option } = self.mode {
            if let Some(pm) = self.branches[branch].post_main {
                if align::similarity(text, &self.main_sentences[pm]) >= align::MATCH_THRESHOLD {
                    self.engine.set_position(pm);
                    self.commit_linear();
                    return self.committed_update(true, None);
                }
            }
            // Still on the option: it is evidence only if it matches the option.
            let opt = &self.branches[branch].options[option];
            let matched = align::similarity(text, &opt.joined) >= align::MATCH_THRESHOLD;
            let label = opt.label.clone();
            let ti = opt.first_timeline.unwrap_or(self.committed_timeline);
            self.committed_timeline = ti;
            self.committed_state = TrackState::InBranch {
                option_label: label,
            };
            return self.committed_update(matched, None);
        }

        // Awaiting a branch decision: try to pick an option.
        if let Mode::AwaitingBranch { branch } = self.mode {
            if let Some(selected) = self.try_select(branch, text) {
                return selected;
            }
            // No option chosen -> fall through to main alignment.
        }

        // Normal main-line alignment.
        let result = self.engine.align(text);
        self.committed = self.engine.position();
        self.preview = self.committed;
        if result.ad_libbing {
            self.committed_state = TrackState::AdLibbing;
            return self.committed_update(false, None);
        }
        self.commit_linear();
        // Same-update selection: if this utterance both reached a branch and
        // already names an option, select it now rather than waiting for the
        // next final.
        if let Mode::AwaitingBranch { branch } = self.mode {
            if let Some(selected) = self.try_select(branch, text) {
                return selected;
            }
        }
        self.committed_update(result.matched, None)
    }

    fn observe_partial(&mut self, text: &str) -> TrackUpdate {
        if matches!(self.mode, Mode::InBranch { .. }) {
            return TrackUpdate {
                sentence_index: self.committed,
                timeline_index: self.committed_timeline,
                committed: false,
                matched: false,
                confidence: 0.0,
                state: self.committed_state.clone(),
                branch_choice: None,
            };
        }
        let result = self.engine.peek(text);
        if result.matched && result.position > self.preview {
            self.preview = result.position; // monotonic forward between commits
        }
        let timeline_index = self
            .main_to_timeline
            .get(self.preview)
            .copied()
            .unwrap_or(self.committed_timeline);
        TrackUpdate {
            sentence_index: self.preview,
            timeline_index,
            committed: false,
            matched: false,
            confidence: result.confidence,
            state: self.committed_state.clone(),
            branch_choice: None,
        }
    }

    /// Try to select an option of `branch` from `text`; on success transition to
    /// `InBranch` and return the selecting update.
    fn try_select(&mut self, branch: usize, text: &str) -> Option<TrackUpdate> {
        let option = self.choose_option(branch, text)?;
        let opt = &self.branches[branch].options[option];
        let label = opt.label.clone();
        let ti = opt.first_timeline.unwrap_or(self.committed_timeline);
        let question = self.branches[branch].question.clone();
        self.mode = Mode::InBranch { branch, option };
        self.committed_timeline = ti;
        self.committed_state = TrackState::InBranch {
            option_label: label.clone(),
        };
        Some(self.committed_update(
            true,
            Some(BranchChoice {
                question,
                option_label: label,
            }),
        ))
    }

    fn commit_linear(&mut self) {
        self.committed = self.engine.position();
        self.preview = self.committed;
        self.committed_timeline = self
            .main_to_timeline
            .get(self.committed)
            .copied()
            .unwrap_or(0);
        self.committed_state = self.linear_state_at(self.committed);
        self.mode = match self.branch_after.get(&self.committed) {
            Some(&branch) => Mode::AwaitingBranch { branch },
            None => Mode::Linear,
        };
    }

    fn choose_option(&self, branch: usize, text: &str) -> Option<usize> {
        let mut scored: Vec<(usize, f32)> = self.branches[branch]
            .options
            .iter()
            .enumerate()
            .filter(|(_, o)| !o.joined.is_empty())
            .map(|(i, o)| (i, align::similarity(text, &o.joined)))
            .collect();
        if scored.is_empty() {
            return None;
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let (best_i, best_s) = scored[0];
        let second_s = scored.get(1).map(|x| x.1).unwrap_or(0.0);
        if best_s >= align::MATCH_THRESHOLD && best_s - second_s >= BRANCH_MARGIN {
            Some(best_i)
        } else {
            None
        }
    }

    fn linear_state_at(&self, sentence_index: usize) -> TrackState {
        if let Some(&branch) = self.branch_after.get(&sentence_index) {
            return TrackState::AtBranch {
                question: self.branches[branch].question.clone(),
                options: self.branches[branch].option_labels.clone(),
            };
        }
        let Some(&tl) = self.main_to_timeline.get(sentence_index) else {
            return TrackState::Speaking;
        };
        match self.timeline.get(tl + 1) {
            Some(TimelineStep::Pause { prompt }) => TrackState::AtPause {
                prompt: prompt.clone(),
            },
            _ => TrackState::Speaking,
        }
    }

    fn committed_update(&self, matched: bool, branch_choice: Option<BranchChoice>) -> TrackUpdate {
        TrackUpdate {
            sentence_index: self.committed,
            timeline_index: self.committed_timeline,
            committed: true,
            matched,
            confidence: if matched { 1.0 } else { 0.0 },
            state: self.committed_state.clone(),
            branch_choice,
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

    fn script_from(sections: Vec<Section>) -> Script {
        Script {
            frontmatter: Frontmatter {
                title: "Test".into(),
                r#type: None,
                version: None,
                variables: Default::default(),
                estimated_duration: None,
            },
            sections,
            word_count: 0,
        }
    }

    fn sample_script() -> Script {
        script_from(vec![Section {
            name: "Intro".into(),
            word_count: 0,
            elements: vec![
                Element::Text(vec![
                    sent("hi thanks for meeting with me today"),
                    sent("my goal is simple to make sure every medication youre taking is safe"),
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
                            sentences: vec![sent("okay then lets keep moving along nicely")],
                        },
                    ],
                }),
                Element::Text(vec![sent("does this make sense so far for you")]),
            ],
        }])
    }

    #[test]
    fn timeline_flattening_counts() {
        let t = ScriptTracker::new(&sample_script());
        assert_eq!(t.timeline().len(), 9);
        assert_eq!(t.sentence_count(), 4);
        assert!(matches!(t.timeline()[3], TimelineStep::Pause { .. }));
        assert!(matches!(t.timeline()[5], TimelineStep::Branch { .. }));
        assert!(matches!(
            t.timeline()[6],
            TimelineStep::BranchSentence { .. }
        ));
    }

    #[test]
    fn matched_final_commits_and_advances() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        assert!(u.committed && u.matched);
        assert_eq!(u.sentence_index, 2);
    }

    #[test]
    fn unmatched_final_is_committed_but_not_matched() {
        let mut t = ScriptTracker::new(&sample_script());
        // Off-script text that does not align: committed update, but matched=false
        // and the cursor does not move.
        let u = t.observe(&SpeechUpdate::finalized(
            "kubernetes pod autoscaling webpack bundle configuration",
        ));
        assert!(u.committed);
        assert!(!u.matched, "off-script speech is not evidence");
        assert_eq!(t.position(), 0);
    }

    #[test]
    fn preview_is_monotonic_between_commits() {
        let mut t = ScriptTracker::new(&sample_script());
        t.observe(&SpeechUpdate::partial(
            "you are currently taking warfarin and metformin",
        ));
        assert_eq!(t.preview_position(), 2);
        t.observe(&SpeechUpdate::partial(
            "hi thanks for meeting with me today",
        ));
        assert_eq!(t.preview_position(), 2, "preview must not move backward");
    }

    #[test]
    fn reaching_pause_and_branch() {
        let mut t = ScriptTracker::new(&sample_script());
        let p = t.observe(&SpeechUpdate::finalized(
            "my goal is simple to make sure every medication youre taking is safe",
        ));
        assert!(matches!(p.state, TrackState::AtPause { .. }));
        let b = t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        assert!(matches!(b.state, TrackState::AtBranch { .. }));
    }

    #[test]
    fn selecting_a_branch_carries_choice_and_returns_to_main() {
        let mut t = ScriptTracker::new(&sample_script());
        t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        let pick = t.observe(&SpeechUpdate::finalized(
            "okay then lets keep moving along nicely",
        ));
        assert_eq!(
            pick.branch_choice,
            Some(BranchChoice {
                question: "any questions".into(),
                option_label: "NO".into(),
            })
        );
        assert!(pick.matched);

        // Re-reading the option does not re-fire the choice.
        let stay = t.observe(&SpeechUpdate::finalized(
            "okay then lets keep moving along nicely",
        ));
        assert!(stay.branch_choice.is_none());

        // Returning to the main line exits the branch.
        let back = t.observe(&SpeechUpdate::finalized(
            "does this make sense so far for you",
        ));
        assert!(back.branch_choice.is_none());
        assert_eq!(back.state, TrackState::Speaking);
        assert_eq!(t.position(), 3);
    }

    #[test]
    fn does_not_advance_into_a_future_branch_option() {
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "great lets discuss your concerns in detail",
        ));
        assert_eq!(t.position(), 0);
        assert!(u.branch_choice.is_none());
    }

    #[test]
    fn empty_script_is_neutral() {
        let mut t = ScriptTracker::new(&script_from(vec![Section {
            name: "Empty".into(),
            word_count: 0,
            elements: vec![],
        }]));
        assert_eq!(t.sentence_count(), 0);
        let u = t.observe(&SpeechUpdate::finalized("anything at all goes here"));
        assert!(!u.matched);
        assert!(u.branch_choice.is_none());
    }

    #[test]
    fn manual_set_position_and_reset() {
        let mut t = ScriptTracker::new(&sample_script());
        t.set_position(2);
        assert_eq!(t.position(), 2);
        assert!(matches!(t.committed_state, TrackState::AtBranch { .. }));
        t.reset();
        assert_eq!(t.position(), 0);
    }
}
