use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrompterError {
    #[error(transparent)]
    Parse(#[from] ParseError),

    #[cfg(feature = "audio")]
    #[error("audio error: {0}")]
    Audio(#[from] minutes_core::error::CaptureError),
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
