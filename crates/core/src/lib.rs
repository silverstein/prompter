pub mod align;
pub mod coaching;
pub mod compliance;
pub mod error;
pub mod script;
pub mod session;
pub mod speech;
pub mod tracker;

// Audio + VAD: re-exported from minutes-core (shared library)
#[cfg(feature = "audio")]
pub use minutes_core::streaming::{AudioChunk, AudioStream};
#[cfg(feature = "audio")]
pub use minutes_core::vad::{Vad, VadResult};

// Whisper transcriber (Prompter-specific — streaming chunk processing)
#[cfg(feature = "whisper")]
pub mod transcribe;
#[cfg(feature = "whisper")]
pub use transcribe::StreamingTranscriber;

pub use align::{similarity, AlignResult, AlignmentEngine, MATCH_THRESHOLD};
pub use compliance::ComplianceReport;
pub use error::{ParseError, PrompterError};
pub use script::{BranchOption, Directive, Frontmatter, Script, Section, Sentence};
pub use session::{SessionRecorder, TranscriptLine};
pub use speech::{MockSpeechProvider, RecognizedWord, SpeechProvider, SpeechUpdate};
pub use tracker::{BranchChoice, ScriptTracker, TimelineStep, TrackState, TrackUpdate};
