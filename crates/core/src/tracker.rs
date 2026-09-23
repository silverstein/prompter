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
use crate::speech::{recent_words, SpeechUpdate};
use std::collections::HashMap;

/// Minimum margin by which the best branch option must beat the runner-up to be
/// selected (on top of clearing [`align::MATCH_THRESHOLD`]).
const BRANCH_MARGIN: f32 = 0.10;

/// Maximum sentences the cursor (committed or preview) may advance in a single
/// update. A spurious far match can't fling the cursor across the script; it
/// catches up gradually over subsequent updates instead.
const MAX_ADVANCE: usize = 4;

/// Relocator bar (the coarse whole-script tracker, borrowed from opera score
/// following). When the local aligner finds nothing, a whole-script search may
/// move the cursor far away -- a real skip, or going back to re-read -- but only
/// on strong, repeated evidence: at least this many matched words...
const RELOCATE_MIN_WORDS: usize = 5;
/// ...at this match density...
const RELOCATE_MIN_CONFIDENCE: f32 = 0.8;
/// ...agreed on by this many successive updates that keep reading forward in
/// the same passage. One coincidental phrase can never move the cursor.
const RELOCATE_AGREEMENT: u8 = 3;

/// Picking a branch option from live reading: the last few recognized words
/// are matched against each option (and the main line) ...
const BRANCH_TAIL_WORDS: usize = 5;
/// ...and an option is chosen once at least this many of them match it, more
/// than match any other option or the nearby main line.
const BRANCH_MIN_WORDS: usize = 3;

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
    /// True when the whole-script relocator moved the cursor (possibly
    /// backward). The UI must follow even if it is behind its own position.
    pub relocated: bool,
}

/// A far-away passage the relocator is considering.
#[derive(Debug, Clone, Copy)]
struct RelocateCandidate {
    sentence: usize,
    end_word: usize,
    hits: u8,
}

struct BranchData {
    /// Main sentence the branch follows (`None` if the script opens with it).
    trigger: Option<usize>,
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
    /// Timeline index of each of the option's sentences.
    timelines: Vec<usize>,
    /// Aligner over just this option's sentences (`None` if it has none): picks
    /// the option from live reading and follows the reader through it.
    engine: Option<AlignmentEngine>,
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
    relocate: Option<RelocateCandidate>,
    /// Sentence being read within the selected branch option.
    option_pos: usize,
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
                            let mut timelines = Vec::new();
                            for sentence in &option.sentences {
                                if first_timeline.is_none() {
                                    first_timeline = Some(timeline.len());
                                }
                                timelines.push(timeline.len());
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
                                timelines,
                                engine: (!option.sentences.is_empty()).then(|| {
                                    let mut e = AlignmentEngine::new(
                                        option.sentences.iter().map(|s| s.text.clone()).collect(),
                                    );
                                    e.set_window_radius(option.sentences.len());
                                    e
                                }),
                            });
                        }
                        branches.push(BranchData {
                            trigger: last_main,
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
            relocate: None,
            option_pos: 0,
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
        self.relocate = None;
        self.commit_linear();
    }

    /// Set the aligner's search window (sentences each side of the cursor).
    /// A tight window (e.g. 5-6) suits live ASR following.
    pub fn set_window_radius(&mut self, radius: usize) {
        self.engine.set_window_radius(radius);
    }

    /// Reset to the start of the script.
    pub fn reset(&mut self) {
        self.engine.set_position(0);
        self.committed = 0;
        self.preview = 0;
        self.mode = Mode::Linear;
        self.committed_state = TrackState::Speaking;
        self.committed_timeline = self.main_to_timeline.first().copied().unwrap_or(0);
        self.relocate = None;
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
                relocated: false,
            };
        }
        if update.is_final {
            self.observe_final(&update.text)
        } else {
            self.observe_partial(&update.text)
        }
    }

    fn observe_final(&mut self, text: &str) -> TrackUpdate {
        // Inside a branch: follow the option, watch for the return to the main line.
        if let Mode::InBranch { branch, option } = self.mode {
            return self.observe_in_branch(branch, option, text, true);
        }

        // Awaiting a branch decision: try to pick an option.
        if let Mode::AwaitingBranch { branch } = self.mode {
            if let Some(selected) = self.try_select(branch, text) {
                return selected;
            }
            // No option chosen -> fall through to main alignment.
        }
        for branch in self.upcoming_branches() {
            if let Some(selected) = self.try_select_by_reading(branch, text) {
                return selected;
            }
        }

        // Nothing local, or a match behind the cursor (going back to re-read):
        // give the whole-script relocator a look before the aligner records a
        // miss. The aligner only takes small back-jumps on its own.
        let peek = self.engine.peek(text);
        if !peek.matched || peek.position < self.committed.max(self.preview) {
            if let Some(u) = self.try_relocate(text) {
                return u;
            }
        } else {
            self.relocate = None;
        }

        // Normal main-line alignment. Floor the forward-jump cap at the live
        // cursor (`preview`), not just `committed`: during a run of partials the
        // preview has already advanced past the (stale) committed floor, and a
        // final must be free to confirm there instead of snapping back to
        // committed + MAX_ADVANCE.
        let prev = self.committed.max(self.preview);
        let result = self.engine.align(text);
        // Cap the forward jump so one false match can't fling the cursor far.
        if self.engine.position() > prev + MAX_ADVANCE {
            self.engine.set_position(prev + MAX_ADVANCE);
        }
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
        if let Mode::InBranch { branch, option } = self.mode {
            // Partials matter here as much as on the main line: a partials-only
            // provider (Apple) may never send a final inside the branch.
            return self.observe_in_branch(branch, option, text, false);
        }
        // Reached a branch point: pick the option the speaker starts reading.
        // Selection needs words unique to one option (see
        // `try_select_by_reading`), so reading on into the main line instead
        // (skipping the branch) never selects anything.
        // A branch coming up within a few sentences: the speaker may skip the
        // rest of the lead-in and go straight into an answer.
        for branch in self.upcoming_branches() {
            if let Some(selected) = self.try_select_by_reading(branch, text) {
                return selected;
            }
        }
        if let Mode::AwaitingBranch { branch } = self.mode {
            // Reading an answer we can't pin to one option yet (text several
            // options share): hold at the branch rather than letting a
            // coincidental main-line match carry the cursor past it.
            if self.reading_an_option(branch, text) {
                return TrackUpdate {
                    sentence_index: self.preview,
                    timeline_index: self.main_to_timeline.get(self.preview).copied().unwrap_or(0),
                    committed: false,
                    matched: false,
                    confidence: 0.0,
                    state: self.linear_state_at(self.preview),
                    branch_choice: None,
                    relocated: false,
                };
            }
        }
        let result = self.engine.peek(text);
        // A match behind the live cursor is going back to re-read. Partials
        // never move the cursor backward on their own (that would let a revised
        // hypothesis flicker it back), so the backward move goes through the
        // relocator's agreement gate instead. Without this, going back fewer
        // sentences than the window radius was followed by nothing: the peek
        // matched (so the relocator never ran) but partials are forward-only.
        if !result.matched || result.position < self.preview {
            if let Some(u) = self.try_relocate(text) {
                return u;
            }
        } else {
            self.relocate = None;
        }
        if result.matched {
            // Forward-only, monotonic, rate-limited: a partial can preview ahead
            // but at most MAX_ADVANCE past the *live cursor* (`preview`) per
            // update, so one noisy partial can't fling the cursor far.
            //
            // The cap is measured from `preview`, NOT `committed`: Apple's
            // recognizer only emits `isFinal` at long pauses, so during
            // continuous reading `committed` never advances. A committed-anchored
            // cap froze the cursor permanently at committed + MAX_ADVANCE (the
            // "prompter never budges" bug). See tracker tests + examples/replay.
            let capped = result.position.min(self.preview.saturating_add(MAX_ADVANCE));
            if capped > self.preview {
                self.preview = capped;
                // Slide the engine's search window forward to follow the reader.
                // The window is centered on the engine cursor, which otherwise
                // only moves on finals; without this, a read longer than the
                // window radius re-freezes at the window edge.
                self.engine.set_position(self.preview);
            }
        }
        // Arm (or disarm) branch selection from the live cursor: with partials
        // only, `commit_linear` (which sets this on finals) may never run.
        self.mode = match self.branch_after.get(&self.preview) {
            Some(&branch) => Mode::AwaitingBranch { branch },
            None => Mode::Linear,
        };
        let timeline_index = self
            .main_to_timeline
            .get(self.preview)
            .copied()
            .unwrap_or(self.committed_timeline);
        TrackUpdate {
            sentence_index: self.preview,
            timeline_index,
            committed: false,
            // Surface the peek's match result: a confident partial IS evidence
            // the sentence was spoken. With a partials-only provider this is the
            // ONLY evidence the recorder ever gets (finals never arrive during
            // continuous reading), so gating coverage on `committed` alone left
            // the compliance report empty. `committed` stays false (volatile),
            // so the recorder still distinguishes preview from confirmed.
            matched: result.matched,
            confidence: result.confidence,
            // Derive the pause/branch cue from the live cursor so the operator
            // sees "wait for response" / the branch question as they reach it,
            // not only after a final lands (which may never happen). Pure lookup.
            state: self.linear_state_at(self.preview),
            branch_choice: None,
            relocated: false,
        }
    }

    /// The coarse whole-script tracker. Called only when the local aligner has
    /// no match. Moves the cursor to a distant passage once
    /// [`RELOCATE_AGREEMENT`] successive updates agree on it, reading forward.
    fn try_relocate(&mut self, text: &str) -> Option<TrackUpdate> {
        let Some(hit) = self.engine.locate_global(text) else {
            self.relocate = None;
            return None;
        };
        // Repeated text is no evidence of WHICH copy is being read: hold until
        // the words past the repeat pick one out.
        if hit.ambiguous
            || hit.matched_words < RELOCATE_MIN_WORDS
            || hit.confidence < RELOCATE_MIN_CONFIDENCE
        {
            self.relocate = None;
            return None;
        }
        // Just ahead of the cursor the local aligner is in charge. Anything
        // behind it, however close, needs agreement: partials only move forward.
        let cur = self.committed.max(self.preview);
        if hit.sentence >= cur && hit.sentence <= cur + MAX_ADVANCE {
            self.relocate = None;
            return None;
        }
        let hits = match self.relocate {
            Some(c) if c.sentence.abs_diff(hit.sentence) <= 2 && hit.end_word > c.end_word => {
                c.hits + 1
            }
            // Same text again (a repeated partial) is not new evidence.
            Some(c) if c.sentence.abs_diff(hit.sentence) <= 2 && hit.end_word == c.end_word => {
                c.hits
            }
            _ => 1,
        };
        self.relocate = Some(RelocateCandidate {
            sentence: hit.sentence,
            end_word: hit.end_word,
            hits,
        });
        if hits < RELOCATE_AGREEMENT {
            return None;
        }
        self.relocate = None;
        self.engine.set_position(hit.sentence);
        self.mode = Mode::Linear;
        self.commit_linear();
        let mut u = self.committed_update(true, None);
        u.relocated = true;
        Some(u)
    }

    /// Try to select an option of `branch` from `text`; on success transition to
    /// `InBranch` and return the selecting update.
    /// Branches whose question comes at most [`MAX_ADVANCE`] sentences after the
    /// live cursor, nearest first (the one we're at, if any, comes first).
    fn upcoming_branches(&self) -> Vec<usize> {
        let cur = self.committed.max(self.preview);
        let mut v: Vec<(usize, usize)> = self
            .branches
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let t = b.trigger?;
                (t >= cur && t <= cur + MAX_ADVANCE).then_some((t, i))
            })
            .collect();
        v.sort();
        v.into_iter().map(|(_, i)| i).collect()
    }

    /// Choose a branch option by hand (the operator clicked it): the tracker
    /// follows the reader from the start of that option.
    pub fn choose_branch(&mut self, branch: usize, option: usize) {
        let Some(b) = self.branches.get_mut(branch) else { return };
        let Some(opt) = b.options.get_mut(option) else { return };
        if let Some(e) = opt.engine.as_mut() {
            e.set_position(0);
        }
        let ti = opt.timelines.first().copied();
        let label = opt.label.clone();
        if let Some(t) = b.trigger {
            self.engine.set_position(t);
            self.committed = t;
            self.preview = t;
        }
        self.relocate = None;
        self.mode = Mode::InBranch { branch, option };
        self.option_pos = 0;
        if let Some(ti) = ti {
            self.committed_timeline = ti;
        }
        self.committed_state = TrackState::InBranch { option_label: label };
    }

    /// The latest words match some option of `branch` at least as well as the
    /// nearby main line.
    fn reading_an_option(&self, branch: usize, text: &str) -> bool {
        let tail = recent_words(text, BRANCH_TAIL_WORDS);
        let strength = |h: Option<align::GlobalHit>| {
            h.filter(|h| h.confidence >= RELOCATE_MIN_CONFIDENCE)
                .map_or(0, |h| h.matched_words)
        };
        let in_option = self.branches[branch]
            .options
            .iter()
            .filter_map(|o| o.engine.as_ref())
            .map(|e| strength(e.locate_global(&tail)))
            .max()
            .unwrap_or(0);
        in_option >= BRANCH_MIN_WORDS && in_option >= strength(self.engine.locate_local(&tail))
    }

    /// Pick the option the speaker has started reading: the last few words
    /// must match one option better than every other option and better than
    /// the nearby main line. Text shared by several options (a common closing
    /// line) selects nothing until words unique to one are heard.
    fn try_select_by_reading(&mut self, branch: usize, text: &str) -> Option<TrackUpdate> {
        let tail = recent_words(text, BRANCH_TAIL_WORDS);
        let strength = |h: Option<align::GlobalHit>| {
            h.filter(|h| h.confidence >= RELOCATE_MIN_CONFIDENCE)
                .map_or(0, |h| h.matched_words)
        };
        let main = strength(self.engine.locate_local(&tail));
        let mut best: Option<(usize, usize, align::GlobalHit)> = None; // (option, words, hit)
        let mut runner_up = 0;
        for (i, opt) in self.branches[branch].options.iter().enumerate() {
            let Some(engine) = &opt.engine else { continue };
            let hit = engine.locate_global(&tail);
            let words = strength(hit);
            match best {
                Some((_, bw, _)) if words <= bw => runner_up = runner_up.max(words),
                _ => {
                    runner_up = runner_up.max(best.map_or(0, |b| b.1));
                    best = hit.map(|h| (i, words, h));
                }
            }
        }
        let (option, words, hit) = best?;
        if words < BRANCH_MIN_WORDS || words <= runner_up || words <= main {
            return None;
        }
        let pos = hit.sentence;
        let opt = &mut self.branches[branch].options[option];
        if let Some(e) = opt.engine.as_mut() {
            e.set_position(pos);
        }
        let label = opt.label.clone();
        let ti = opt.timelines.get(pos).copied().unwrap_or(self.committed_timeline);
        let question = self.branches[branch].question.clone();
        self.mode = Mode::InBranch { branch, option };
        // Reaching the branch confirms the main line up to it (with partials
        // only, `committed` may still be far behind the live cursor), including
        // when the speaker skipped the end of the lead-in.
        let at = self.committed.max(self.preview).max(self.branches[branch].trigger.unwrap_or(0));
        self.engine.set_position(at);
        self.committed = at;
        self.preview = at;
        self.option_pos = pos;
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

    /// Inside a selected option: follow the reader through its sentences
    /// (forward only), and return to the main line once they read on past the
    /// branch.
    fn observe_in_branch(&mut self, branch: usize, option: usize, text: &str, is_final: bool) -> TrackUpdate {
        if let Some(pm) = self.branches[branch].post_main {
            // Back on the main line: the post-branch sentence as a whole, or the
            // latest words matching the main line at or past it (more strongly
            // than they match the option).
            let tail = recent_words(text, BRANCH_TAIL_WORDS);
            let main = self
                .engine
                .locate_local(&tail)
                .filter(|h| h.sentence >= pm && h.confidence >= RELOCATE_MIN_CONFIDENCE);
            let in_option = self.branches[branch].options[option]
                .engine
                .as_ref()
                .and_then(|e| e.locate_global(&tail))
                .map_or(0, |h| h.matched_words);
            let resume = match main {
                Some(h) if h.matched_words >= BRANCH_MIN_WORDS && h.matched_words > in_option => {
                    Some(h.sentence)
                }
                // Whole-sentence similarity is loose (shared words give partial
                // credit), so only trust it once the latest words have left the
                // option.
                _ if in_option < BRANCH_MIN_WORDS
                    && align::similarity(text, &self.main_sentences[pm]) >= align::MATCH_THRESHOLD =>
                {
                    Some(pm)
                }
                _ => None,
            };
            if let Some(at) = resume {
                self.engine.set_position(at);
                self.commit_linear();
                return self.committed_update(true, None);
            }
        }
        // Still on the option: advance through its sentences.
        let opt = &mut self.branches[branch].options[option];
        let peek = opt.engine.as_ref().map(|e| e.peek(text));
        let matched = peek.as_ref().is_some_and(|r| r.matched);
        if let Some(r) = peek.filter(|r| r.matched) {
            let capped = r.position.min(self.option_pos + MAX_ADVANCE);
            if capped > self.option_pos {
                self.option_pos = capped;
                if let Some(e) = opt.engine.as_mut() {
                    e.set_position(capped);
                }
            }
        }
        self.committed_timeline = opt
            .timelines
            .get(self.option_pos)
            .copied()
            .unwrap_or(self.committed_timeline);
        self.committed_state = TrackState::InBranch {
            option_label: opt.label.clone(),
        };
        let mut u = self.committed_update(matched, None);
        u.committed = is_final;
        u
    }

    fn try_select(&mut self, branch: usize, text: &str) -> Option<TrackUpdate> {
        let option = self.choose_option(branch, text)?;
        let opt = &self.branches[branch].options[option];
        let label = opt.label.clone();
        let ti = opt.first_timeline.unwrap_or(self.committed_timeline);
        let question = self.branches[branch].question.clone();
        self.mode = Mode::InBranch { branch, option };
        self.option_pos = 0;
        if let Some(e) = self.branches[branch].options[option].engine.as_mut() {
            e.set_position(0);
        }
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
            relocated: false,
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
    fn partials_only_keep_advancing_past_max_advance() {
        // Apple's SFSpeechRecognizer emits only volatile partials during
        // continuous reading (no `isFinal` until a long pause). The cursor must
        // still track forward across the whole script. A committed-anchored cap
        // previously froze the preview at committed + MAX_ADVANCE (idx 4) because
        // `committed` only advances on finals -- the "prompter never budges" bug
        // captured in the real recording under examples/replay.
        let sentences = [
            "hi thanks for meeting with me today i appreciate it",
            "my goal is simple to keep every medication safe and effective",
            "i understand you have been feeling dizzy lately and bruising",
            "it sounds like these symptoms have been worrying you a lot",
            "i reviewed your profile and found a few critical interactions",
            "your warfarin and garlic together raise your risk of bleeding",
            "the saint johns wort can make your other medicines weaker",
            "lets walk through a simple plan to fix this together today",
            "first we will adjust the timing of your evening doses",
            "then we will follow up next week to confirm improvement",
        ];
        let script = script_from(vec![Section {
            name: "Body".into(),
            word_count: 0,
            elements: vec![Element::Text(sentences.iter().map(|s| sent(s)).collect())],
        }]);
        let mut t = ScriptTracker::new(&script);
        t.set_window_radius(10);

        // Read the whole script as a growing cumulative partial, feeding only
        // the leading edge (like the app's `recent_words(text, 10)`), NO finals.
        let mut spoken: Vec<&str> = Vec::new();
        let mut last = 0usize;
        for s in &sentences {
            for word in s.split_whitespace() {
                spoken.push(word);
                let start = spoken.len().saturating_sub(10);
                let lead = spoken[start..].join(" ");
                let u = t.observe(&SpeechUpdate::partial(lead));
                assert!(!u.committed, "partials must never commit");
                last = u.sentence_index;
            }
        }
        // With no finals at all, the cursor must have tracked to (near) the last
        // sentence -- far past the old committed + MAX_ADVANCE ceiling of 4.
        assert!(
            last >= sentences.len() - 2,
            "partials-only cursor froze at idx {last}, expected >= {}",
            sentences.len() - 2
        );
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
    fn returns_to_main_from_branch_on_a_partial() {
        // Branch selection happens on finals (the speaker stops to ask, so a
        // final fires there), but the speaker then resumes the main script
        // CONTINUOUSLY -- partials only, no final. The cursor must leave the
        // branch on a confident partial instead of pinning forever (the same
        // freeze class as the linear path). Regression for codex P1#2.
        let mut t = ScriptTracker::new(&sample_script());
        t.observe(&SpeechUpdate::finalized(
            "you are currently taking warfarin and metformin",
        ));
        let pick = t.observe(&SpeechUpdate::finalized(
            "okay then lets keep moving along nicely",
        ));
        assert!(matches!(pick.state, TrackState::InBranch { .. }));

        // Resume the main line on a PARTIAL (post-branch sentence).
        let back = t.observe(&SpeechUpdate::partial("does this make sense so far for you"));
        assert_eq!(back.state, TrackState::Speaking, "left the branch on a partial");
        assert_eq!(t.position(), 3);
    }

    #[test]
    fn reading_an_upcoming_answer_skips_to_it() {
        // The branch is two sentences ahead: skipping the rest of the lead-in
        // and reading an answer goes straight into that answer.
        let mut t = ScriptTracker::new(&sample_script());
        let u = t.observe(&SpeechUpdate::finalized(
            "great lets discuss your concerns in detail",
        ));
        assert_eq!(u.branch_choice.map(|c| c.option_label).as_deref(), Some("YES"));
        assert_eq!(t.position(), 2, "main cursor moves up to the branch point");
    }

    #[test]
    fn a_far_away_branch_answer_is_not_jumped_to() {
        // Same answer text, but the branch is many sentences ahead.
        let mut sentences: Vec<Sentence> = (0..12).map(|i| sent(&sentence_text(i))).collect();
        let lead = sentences.split_off(1);
        let script = script_from(vec![Section {
            name: "Body".into(),
            word_count: 0,
            elements: vec![
                Element::Text(sentences),
                Element::Text(lead),
                Element::Directive(Directive::Branch {
                    question: "any questions".into(),
                    options: vec![BranchOption {
                        label: "YES".into(),
                        sentences: vec![sent("great lets discuss your concerns in detail")],
                    }],
                }),
            ],
        }]);
        let mut t = ScriptTracker::new(&script);
        let u = t.observe(&SpeechUpdate::partial("great lets discuss your concerns in detail"));
        assert!(u.branch_choice.is_none());
        assert_eq!(t.preview_position(), 0);
    }

    #[test]
    fn choosing_a_branch_by_hand_follows_that_answer() {
        let mut t = ScriptTracker::new(&sample_script());
        t.choose_branch(0, 1);
        let u = t.observe(&SpeechUpdate::partial("okay then lets keep moving"));
        assert_eq!(u.state, TrackState::InBranch { option_label: "NO".into() });
        let u = t.observe(&SpeechUpdate::partial("does this make sense so far for you"));
        assert_eq!(u.state, TrackState::Speaking, "returns to the main line after the answer");
        assert_eq!(u.sentence_index, 3);
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

    fn long_script(n: usize) -> Script {
        let sentences: Vec<Sentence> = (0..n)
            .map(|i| sent(&format!("{} {} {} {} {} {}", w(i, 0), w(i, 1), w(i, 2), w(i, 3), w(i, 4), w(i, 5))))
            .collect();
        script_from(vec![Section {
            name: "Body".into(),
            word_count: 0,
            elements: vec![Element::Text(sentences)],
        }])
    }

    /// A distinct, deterministic word per (sentence, slot).
    fn w(i: usize, k: usize) -> String {
        const SYL: [&str; 12] = ["ka", "lo", "mi", "ne", "pu", "ra", "si", "to", "vu", "ze", "bo", "di"];
        format!("{}{}{}", SYL[i % 12], SYL[(i / 12 + k) % 12], SYL[(k * 5 + i) % 12])
    }

    fn sentence_text(i: usize) -> String {
        (0..6).map(|k| w(i, k)).collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn relocator_follows_a_real_skip_after_agreement() {
        let mut t = ScriptTracker::new(&long_script(40));
        t.set_window_radius(5);
        t.observe(&SpeechUpdate::partial(sentence_text(0)));
        assert_eq!(t.preview_position(), 0);
        // The reader skips to sentence 30 and keeps reading there.
        let mut moved = None;
        for i in 30..34 {
            let u = t.observe(&SpeechUpdate::partial(sentence_text(i)));
            if u.relocated {
                moved = Some(u.sentence_index);
                break;
            }
        }
        let at = moved.expect("relocator should move after agreement");
        assert!((30..=33).contains(&at), "relocated to {at}");
    }

    #[test]
    fn relocator_ignores_a_single_far_coincidence() {
        let mut t = ScriptTracker::new(&long_script(40));
        t.set_window_radius(5);
        t.observe(&SpeechUpdate::partial(sentence_text(0)));
        // One far phrase, then back to reading locally.
        let u = t.observe(&SpeechUpdate::partial(sentence_text(30)));
        assert!(!u.relocated);
        t.observe(&SpeechUpdate::partial(sentence_text(1)));
        let u = t.observe(&SpeechUpdate::partial(sentence_text(30)));
        assert!(!u.relocated, "agreement must be consecutive and progressing");
        assert!(t.preview_position() <= 2);
    }

    #[test]
    fn going_back_a_few_sentences_is_followed() {
        // Within the window: the local peek matches behind the cursor, which
        // partials alone never followed (the live Sept 23 read).
        let mut t = ScriptTracker::new(&long_script(30));
        t.set_window_radius(10);
        let mut spoken = String::new();
        for i in 0..10 {
            for k in 0..6 {
                spoken.push(' ');
                spoken.push_str(&w(i, k));
                t.observe(&SpeechUpdate::partial(recent_words(&spoken, 10)));
            }
        }
        assert_eq!(t.preview_position(), 9);
        // Go back to sentence 6 without pausing and keep reading.
        let mut moved = None;
        'read: for i in 6..9 {
            for k in 0..6 {
                spoken.push(' ');
                spoken.push_str(&w(i, k));
                let u = t.observe(&SpeechUpdate::partial(recent_words(&spoken, 10)));
                if u.relocated {
                    moved = Some(u.sentence_index);
                    break 'read;
                }
            }
        }
        let at = moved.expect("going back should be followed");
        assert!((6..=7).contains(&at), "relocated to {at}");
    }

    /// A script where sentence `dup` repeats sentence `orig` word for word. The
    /// repeated sentence is 12 words, long enough that the relocator would get
    /// several agreeing updates while only the repeated words are heard.
    fn script_with_repeat(n: usize, orig: usize, dup: usize) -> Script {
        let long = format!("{} {}", sentence_text(orig), sentence_text(orig + 50));
        let sentences: Vec<Sentence> = (0..n)
            .map(|i| if i == orig || i == dup { sent(&long) } else { sent(&sentence_text(i)) })
            .collect();
        script_from(vec![Section {
            name: "Body".into(),
            word_count: 0,
            elements: vec![Element::Text(sentences)],
        }])
    }

    /// Read sentences `from..to` word by word as Apple-style partials (last 10
    /// words), returning the first relocation target.
    fn read_until_relocated(t: &mut ScriptTracker, script: &Script, from: usize, to: usize) -> Option<usize> {
        let texts: Vec<String> = match &script.sections[0].elements[0] {
            Element::Text(ss) => ss.iter().map(|s| s.text.clone()).collect(),
            _ => unreachable!(),
        };
        let mut spoken = String::new();
        for text in &texts[from..to] {
            for word in text.split_whitespace() {
                spoken.push(' ');
                spoken.push_str(word);
                let u = t.observe(&SpeechUpdate::partial(recent_words(&spoken, 10)));
                if u.relocated {
                    return Some(u.sentence_index);
                }
            }
        }
        None
    }

    #[test]
    fn going_back_to_a_repeated_sentence_picks_the_one_being_read() {
        // Sentence 13 repeats sentence 3. From 16, go back and re-read 13 then
        // 14: the cursor must land on 13/14, never on the far copy at 3.
        let script = script_with_repeat(30, 3, 13);
        let mut t = ScriptTracker::new(&script);
        t.set_window_radius(10);
        t.set_position(16);
        let at = read_until_relocated(&mut t, &script, 13, 16).expect("should follow the re-read");
        assert!((13..=14).contains(&at), "relocated to {at}");
    }

    #[test]
    fn going_back_to_the_first_copy_of_a_repeat_is_followed() {
        // Same script; this time go back to the FIRST copy (3) and read on into
        // 4: once the words after the repeat are heard, it lands on 3/4.
        let script = script_with_repeat(30, 3, 13);
        let mut t = ScriptTracker::new(&script);
        t.set_window_radius(10);
        t.set_position(16);
        let at = read_until_relocated(&mut t, &script, 3, 6).expect("should follow the re-read");
        assert!((3..=4).contains(&at), "relocated to {at}");
    }

    #[test]
    fn one_backward_coincidence_does_not_move_the_cursor() {
        let mut t = ScriptTracker::new(&long_script(30));
        t.set_window_radius(10);
        t.set_position(9);
        let u = t.observe(&SpeechUpdate::partial(sentence_text(6)));
        assert!(!u.relocated);
        // Back to reading forward: the lone backward hit is forgotten.
        let u = t.observe(&SpeechUpdate::partial(sentence_text(9)));
        assert!(!u.relocated);
        let u = t.observe(&SpeechUpdate::partial(sentence_text(6)));
        assert!(!u.relocated);
        assert_eq!(t.preview_position(), 9);
    }

    #[test]
    fn relocator_can_go_back_to_reread() {
        let mut t = ScriptTracker::new(&long_script(40));
        t.set_window_radius(5);
        t.set_position(30);
        let mut moved = None;
        for i in 5..9 {
            let u = t.observe(&SpeechUpdate::partial(sentence_text(i)));
            if u.relocated {
                moved = Some(u.sentence_index);
                break;
            }
        }
        let at = moved.expect("relocator should follow a re-read");
        assert!((5..=8).contains(&at));
    }
}
