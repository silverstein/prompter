//! Portable on-device ASR provider (sherpa-onnx streaming Zipformer).
//!
//! The cross-platform [`crate::speech::SpeechProvider`] baseline (decision D4 in
//! `docs/UPGRADE-2026.md`): true streaming, word/token-onset timestamps, CPU on
//! Windows/Linux/macOS from one code path, on-device (no HIPAA BAA surface), and
//! hotword biasing toward the script's drug names. Feature-gated behind
//! `sherpa`; the model files are loaded at runtime from paths, so this compiles
//! without them but needs a downloaded streaming Zipformer model + a real audio
//! feed to run.
//!
//! Capture is the caller's job (cpal in the Tauri backend): push mono f32 audio
//! via [`SherpaSpeechProvider::feed`], then drain [`SpeechProvider::poll`]. The
//! provider emits a partial [`SpeechUpdate`] as the hypothesis grows and a final
//! one at each detected endpoint (then resets the stream for the next segment).

use crate::speech::{RecognizedWord, SpeechProvider, SpeechUpdate};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream, RecognizerResult};
use std::collections::VecDeque;

/// Paths + tuning for a streaming Zipformer transducer model.
#[derive(Debug, Clone)]
pub struct SherpaConfig {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    pub num_threads: i32,
    /// Hotword phrases (one per line) to bias decoding toward, e.g. the script's
    /// drug names. Enables `modified_beam_search` when present.
    pub hotwords: Option<String>,
    pub hotwords_score: f32,
}

impl SherpaConfig {
    /// A model directory with the conventional file names.
    pub fn from_dir(dir: &str) -> Self {
        Self {
            encoder: format!("{dir}/encoder.onnx"),
            decoder: format!("{dir}/decoder.onnx"),
            joiner: format!("{dir}/joiner.onnx"),
            tokens: format!("{dir}/tokens.txt"),
            num_threads: 1,
            hotwords: None,
            hotwords_score: 1.5,
        }
    }
}

/// Streaming sherpa-onnx recognizer wrapped as a [`SpeechProvider`].
pub struct SherpaSpeechProvider {
    recognizer: OnlineRecognizer,
    stream: OnlineStream,
    sample_rate: i32,
    /// Last partial text emitted for the current segment (de-duplicates polls).
    last_text: String,
    queue: VecDeque<SpeechUpdate>,
}

impl SherpaSpeechProvider {
    /// Create a provider from a model config. Returns an error if the model
    /// files cannot be loaded.
    pub fn new(config: &SherpaConfig) -> Result<Self, String> {
        let mut cfg = OnlineRecognizerConfig::default();
        cfg.model_config.transducer.encoder = Some(config.encoder.clone());
        cfg.model_config.transducer.decoder = Some(config.decoder.clone());
        cfg.model_config.transducer.joiner = Some(config.joiner.clone());
        cfg.model_config.tokens = Some(config.tokens.clone());
        cfg.model_config.num_threads = config.num_threads.max(1);
        cfg.model_config.provider = Some("cpu".to_string());
        // Endpoint detection turns the continuous stream into utterances; the
        // defaults below match sherpa's streaming examples.
        cfg.enable_endpoint = true;
        cfg.rule1_min_trailing_silence = 2.4;
        cfg.rule2_min_trailing_silence = 1.2;
        cfg.rule3_min_utterance_length = 20.0;
        if config.hotwords.is_some() {
            cfg.decoding_method = Some("modified_beam_search".to_string());
            cfg.hotwords_score = config.hotwords_score;
        } else {
            cfg.decoding_method = Some("greedy_search".to_string());
        }

        let recognizer = OnlineRecognizer::create(&cfg).ok_or_else(|| {
            "sherpa-onnx: failed to create recognizer (check model paths)".to_string()
        })?;
        let stream = match &config.hotwords {
            Some(hotwords) => recognizer.create_stream_with_hotwords(hotwords),
            None => recognizer.create_stream(),
        };

        Ok(Self {
            recognizer,
            stream,
            sample_rate: cfg.feat_config.sample_rate,
            last_text: String::new(),
            queue: VecDeque::new(),
        })
    }

    /// Feed captured mono audio (f32) at the model's sample rate. Decodes what is
    /// ready and queues any partial/final update produced.
    pub fn feed(&mut self, samples: &[f32]) {
        self.stream.accept_waveform(self.sample_rate, samples);
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let Some(result) = self.recognizer.get_result(&self.stream) else {
            return;
        };
        let text = result.text.trim().to_string();
        if self.recognizer.is_endpoint(&self.stream) {
            if !text.is_empty() {
                self.queue.push_back(SpeechUpdate {
                    text,
                    words: build_words(&result),
                    is_final: true,
                });
            }
            // Boundary reached: reset for the next utterance.
            self.recognizer.reset(&self.stream);
            self.last_text.clear();
        } else if !text.is_empty() && text != self.last_text {
            self.last_text = text.clone();
            self.queue.push_back(SpeechUpdate {
                text,
                words: build_words(&result),
                is_final: false,
            });
        }
    }
}

/// Map sherpa token timestamps (seconds) to [`RecognizedWord`]s. Tokens are
/// sub-word (BPE) units, so these are token-onset anchors, not whole words --
/// still useful for sub-sentence alignment. Empty when the model emits no
/// timestamps.
fn build_words(result: &RecognizerResult) -> Vec<RecognizedWord> {
    let Some(timestamps) = &result.timestamps else {
        return Vec::new();
    };
    result
        .tokens
        .iter()
        .zip(timestamps.iter())
        .map(|(token, &start)| RecognizedWord {
            text: token.trim().to_string(),
            start_ms: Some((start * 1000.0).max(0.0) as u64),
            end_ms: None,
            confidence: None,
        })
        .collect()
}

impl SpeechProvider for SherpaSpeechProvider {
    fn id(&self) -> &str {
        "sherpa-onnx"
    }

    fn poll(&mut self) -> Option<SpeechUpdate> {
        self.queue.pop_front()
    }

    fn reset(&mut self) {
        self.recognizer.reset(&self.stream);
        self.last_text.clear();
        self.queue.clear();
    }
}
