use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrompterError {
    #[error(transparent)]
    Parse(#[from] ParseError),

    #[cfg(feature = "audio")]
    #[error(transparent)]
    Audio(#[from] AudioError),
}

/// Errors from the audio capture subsystem.
#[cfg(feature = "audio")]
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("no audio input device found")]
    NoDevice,

    #[error("microphone permission denied")]
    PermissionDenied,

    #[error("audio device disconnected")]
    DeviceLost,

    #[error("audio config error: {0}")]
    Config(String),

    #[error("audio stream error: {0}")]
    Stream(String),
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("invalid YAML frontmatter: {0}")]
    Yaml(String),

    #[error("missing required field: {0}")]
    MissingField(&'static str),

    #[error("empty script body — no content after frontmatter")]
    EmptyScript,

    #[error("invalid directive at line {line}: {message}")]
    InvalidDirective { line: usize, message: String },

    #[error("unresolved variable: {{{{{name}}}}}")]
    UnresolvedVariable { name: String },

    #[error("missing frontmatter — script must start with ---")]
    MissingFrontmatter,

    #[error("branch has no options at line {line}")]
    EmptyBranch { line: usize },
}
