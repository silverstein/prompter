pub mod compliance;
pub mod error;
pub mod script;

#[cfg(feature = "audio")]
pub mod audio;
#[cfg(feature = "audio")]
pub mod vad;

pub use compliance::ComplianceReport;
pub use error::{ParseError, PrompterError};
pub use script::{BranchOption, Directive, Frontmatter, Script, Section, Sentence};

#[cfg(feature = "audio")]
pub use audio::{AudioChunk, AudioStream};
#[cfg(feature = "audio")]
pub use vad::{Vad, VadResult};
