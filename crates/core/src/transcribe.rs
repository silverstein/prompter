use std::path::Path;

// ──────────────────────────────────────────────────────────────
// Streaming transcriber — persistent WhisperContext for chunk processing.
//
//   Audio chunks ──▶ StreamingTranscriber ──▶ recognized text
//
// Unlike batch transcription (load file → transcribe → drop context),
// the streaming transcriber keeps the WhisperContext alive across chunks.
// This avoids the ~100ms model-load penalty per invocation.
//
// Usage:
//   let mut t = StreamingTranscriber::new("path/to/model.bin")?;
//   let text = t.transcribe_chunk(&samples)?;  // ~200-500ms for 3s chunk
//   let text = t.transcribe_chunk(&more_samples)?;
// ──────────────────────────────────────────────────────────────

/// Streaming transcriber with persistent whisper context.
pub struct StreamingTranscriber {
    ctx: whisper_rs::WhisperContext,
}

impl StreamingTranscriber {
    /// Create a new transcriber with a whisper model.
    /// The model file is typically at ~/.config/minutes/models/ggml-tiny.bin
    pub fn new(model_path: &Path) -> Result<Self, TranscribeError> {
        if !model_path.exists() {
            return Err(TranscribeError::ModelNotFound(
                model_path.to_string_lossy().to_string(),
            ));
        }

        let ctx = whisper_rs::WhisperContext::new_with_params(
            model_path.to_str().unwrap_or_default(),
            whisper_rs::WhisperContextParameters::default(),
        )
        .map_err(|e| TranscribeError::ModelLoad(format!("{}", e)))?;

        Ok(Self { ctx })
    }

    /// Transcribe a chunk of 16kHz mono f32 audio samples.
    /// Returns the recognized text (may be empty for silence).
    /// Typical latency: ~200-500ms for a 3-second chunk on Apple Silicon.
    pub fn transcribe_chunk(&mut self, samples: &[f32]) -> Result<String, TranscribeError> {
        if samples.is_empty() {
            return Ok(String::new());
        }

        // Minimum ~0.5 seconds of audio for whisper to work
        if samples.len() < 8000 {
            return Ok(String::new());
        }

        let mut state = self
            .ctx
            .create_state()
            .map_err(|e| TranscribeError::Inference(format!("{}", e)))?;

        let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });

        // Optimize for speed over accuracy (we're doing real-time tracking)
        params.set_n_threads(4);
        params.set_language(Some("en"));
        params.set_no_context(true);      // Don't use cross-chunk context
        params.set_single_segment(true);  // Output as one segment
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);

        state
            .full(params, samples)
            .map_err(|e| TranscribeError::Inference(format!("{}", e)))?;

        let n = state.full_n_segments();
        let mut text = String::new();

        for i in 0..n {
            if let Some(seg) = state.get_segment(i) {
                if let Ok(seg_text) = seg.to_str_lossy() {
                    let trimmed = seg_text.trim();
                    if !trimmed.is_empty() {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(trimmed);
                    }
                }
            }
        }

        Ok(text)
    }
}

/// Errors from the transcription subsystem.
#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("whisper model not found: {0}")]
    ModelNotFound(String),

    #[error("failed to load whisper model: {0}")]
    ModelLoad(String),

    #[error("transcription failed: {0}")]
    Inference(String),
}
