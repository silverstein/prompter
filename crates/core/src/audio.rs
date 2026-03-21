use crate::error::AudioError;
use crossbeam_channel::{Receiver, Sender, bounded};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

// ──────────────────────────────────────────────────────────────
// Streaming audio capture for Prompter.
//
//   Microphone ──▶ cpal callback ──▶ mono 16kHz f32
//        │
//        ├──▶ AudioChunk channel (for VAD + whisper consumers)
//        ├──▶ WAV file writer (optional, for recording)
//        └──▶ audio level (atomic, for UI meter)
//
// Design: channel-based, non-blocking. Consumers pull chunks
// at their own pace. If the channel fills, oldest chunks are
// dropped (bounded channel) — VAD needs fresh data, not old.
// ──────────────────────────────────────────────────────────────

/// A chunk of 16kHz mono f32 audio samples.
#[derive(Clone)]
pub struct AudioChunk {
    /// 16kHz mono f32 samples, typically ~1600 samples (100ms)
    pub samples: Vec<f32>,
    /// RMS energy of this chunk (0.0–1.0 scale)
    pub rms: f32,
}

/// Shared audio level (0–100) for UI visualization.
static AUDIO_LEVEL: AtomicU32 = AtomicU32::new(0);

/// Get the current audio input level (0–100).
pub fn audio_level() -> u32 {
    AUDIO_LEVEL.load(Ordering::Relaxed)
}

/// Handle to a running audio stream. Drop to stop capture.
pub struct AudioStream {
    _stream: cpal::Stream,
    stop: Arc<AtomicBool>,
    pub receiver: Receiver<AudioChunk>,
    pub sample_rate: u32,
    pub device_name: String,
}

impl AudioStream {
    /// Start capturing from the default input device.
    /// Returns a stream handle with a channel receiver for audio chunks.
    /// Chunks arrive at ~10Hz (100ms each at 16kHz = 1600 samples).
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or(AudioError::NoDevice)?;

        let device_name = device.name().unwrap_or_else(|_| "unknown".into());
        let config = device
            .default_input_config()
            .map_err(|e| AudioError::Config(e.to_string()))?;

        let native_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let ratio = native_rate as f64 / 16000.0;

        // Bounded channel: 64 chunks = ~6.4 seconds of buffered audio.
        // If consumer falls behind, sender drops oldest (try_send).
        let (tx, rx): (Sender<AudioChunk>, Receiver<AudioChunk>) = bounded(64);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);

        // Accumulate resampled 16kHz samples, emit chunks every ~100ms (1600 samples)
        let chunk_size: usize = 1600;

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let mut resample_buf: Vec<f32> = Vec::new();
                let mut resample_pos: f64 = 0.0;
                let mut chunk_buf: Vec<f32> = Vec::with_capacity(chunk_size);
                let tx = tx.clone();

                device
                    .build_input_stream(
                        &config.into(),
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            if stop_clone.load(Ordering::Relaxed) {
                                return;
                            }

                            // Mix to mono
                            for frame in data.chunks(channels) {
                                let mono: f32 = frame.iter().sum::<f32>() / channels as f32;
                                resample_buf.push(mono);
                            }

                            // Resample to 16kHz
                            while resample_pos < resample_buf.len() as f64 {
                                let idx = resample_pos as usize;
                                if idx < resample_buf.len() {
                                    chunk_buf.push(resample_buf[idx]);
                                }
                                resample_pos += ratio;

                                // Emit chunk when we have enough samples
                                if chunk_buf.len() >= chunk_size {
                                    let samples: Vec<f32> = chunk_buf.drain(..chunk_size).collect();
                                    let rms = compute_rms(&samples);

                                    // Update global level
                                    let level = (rms * 2000.0).min(100.0) as u32;
                                    AUDIO_LEVEL.store(level, Ordering::Relaxed);

                                    let _ = tx.try_send(AudioChunk { samples, rms });
                                }
                            }

                            // Compact resample buffer
                            let consumed = resample_pos as usize;
                            if consumed > 0 && consumed <= resample_buf.len() {
                                resample_buf.drain(..consumed);
                                resample_pos -= consumed as f64;
                            }
                        },
                        move |err| {
                            eprintln!("[prompter] audio error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| AudioError::Stream(e.to_string()))?
            }
            cpal::SampleFormat::I16 => {
                let mut resample_buf: Vec<f32> = Vec::new();
                let mut resample_pos: f64 = 0.0;
                let mut chunk_buf: Vec<f32> = Vec::with_capacity(chunk_size);
                let tx = tx.clone();

                device
                    .build_input_stream(
                        &config.into(),
                        move |data: &[i16], _: &cpal::InputCallbackInfo| {
                            if stop_clone.load(Ordering::Relaxed) {
                                return;
                            }

                            for frame in data.chunks(channels) {
                                let mono: f32 =
                                    frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>()
                                        / channels as f32;
                                resample_buf.push(mono);
                            }

                            while resample_pos < resample_buf.len() as f64 {
                                let idx = resample_pos as usize;
                                if idx < resample_buf.len() {
                                    chunk_buf.push(resample_buf[idx]);
                                }
                                resample_pos += ratio;

                                if chunk_buf.len() >= chunk_size {
                                    let samples: Vec<f32> = chunk_buf.drain(..chunk_size).collect();
                                    let rms = compute_rms(&samples);
                                    let level = (rms * 300.0).min(100.0) as u32;
                                    AUDIO_LEVEL.store(level, Ordering::Relaxed);
                                    let _ = tx.try_send(AudioChunk { samples, rms });
                                }
                            }

                            let consumed = resample_pos as usize;
                            if consumed > 0 && consumed <= resample_buf.len() {
                                resample_buf.drain(..consumed);
                                resample_pos -= consumed as f64;
                            }
                        },
                        move |err| {
                            eprintln!("[prompter] audio error: {}", err);
                        },
                        None,
                    )
                    .map_err(|e| AudioError::Stream(e.to_string()))?
            }
            fmt => return Err(AudioError::Config(format!("unsupported format: {:?}", fmt))),
        };

        stream
            .play()
            .map_err(|e| AudioError::Stream(format!("play: {}", e)))?;

        Ok(AudioStream {
            _stream: stream,
            stop,
            receiver: rx,
            sample_rate: 16000,
            device_name,
        })
    }

    /// Stop the audio stream.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for AudioStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Compute RMS energy of a sample buffer.
fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}
