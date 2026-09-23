pub mod align;
pub mod checklist;
pub mod coaching;
pub mod compliance;
pub mod error;
pub mod script;
pub mod realign;
pub mod session;
#[cfg(feature = "sherpa")]
pub mod sherpa;
pub mod speech;
pub mod tracker;

pub use align::{similarity, AlignResult, AlignmentEngine, MATCH_THRESHOLD};
pub use checklist::{
    cmr_checklist, obra_counseling_checklist, ChecklistEvaluator, ChecklistItem, ChecklistResult,
    ChecklistStatus, KeywordChecklistEvaluator, LlmChecklistEvaluator, LlmClient,
};
pub use compliance::{write_private, ComplianceReport, DeliveryStats};
pub use error::{ParseError, PrompterError};
pub use script::{BranchOption, Directive, Frontmatter, Script, Section, Sentence};
pub use realign::{realign, Realignment};
pub use session::{SessionRecorder, TranscriptLine};
pub use speech::{recent_words, MockSpeechProvider, RecognizedWord, SpeechProvider, SpeechUpdate};
pub use tracker::{BranchChoice, ScriptTracker, TimelineStep, TrackState, TrackUpdate};
