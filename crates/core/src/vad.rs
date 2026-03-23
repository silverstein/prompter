// ──────────────────────────────────────────────────────────────
// Energy-based Voice Activity Detection (VAD).
//
//   AudioChunk.rms ──▶ VAD ──▶ VadState { speaking, silence_ms }
//
// Simple but effective for teleprompter use:
//   - Speech threshold adapts to ambient noise floor
//   - Hysteresis prevents rapid toggling (hangover period)
//   - Reports silence duration for pause detection
//
// No ML model, no Python, ~50 lines. Good enough for Tier 1
// (VAD-controlled scroll). Tier 2 adds whisper confirmation.
// ──────────────────────────────────────────────────────────────

/// VAD output for each chunk.
#[derive(Debug, Clone, Copy)]
pub struct VadResult {
    /// Whether speech is detected.
    pub speaking: bool,
    /// Milliseconds of continuous silence (0 when speaking).
    pub silence_ms: u64,
    /// Current RMS energy level (0.0–1.0).
    pub energy: f32,
    /// Adaptive noise floor estimate.
    pub noise_floor: f32,
}

/// Voice Activity Detector with adaptive threshold.
pub struct Vad {
    /// Noise floor estimate (slow-adapting).
    noise_floor: f32,
    /// Speech threshold = noise_floor * multiplier.
    multiplier: f32,
    /// Current state: true = speech detected.
    is_speaking: bool,
    /// Hangover: keep "speaking" for this many chunks after energy drops.
    /// Prevents rapid toggling during natural speech pauses.
    hangover_chunks: u32,
    hangover_remaining: u32,
    /// Silence tracking (milliseconds).
    silence_ms: u64,
    /// Milliseconds per chunk (100ms at 1600 samples / 16kHz).
    chunk_ms: u64,
    /// Noise floor adaptation rate (lower = slower adaptation).
    adapt_rate: f32,
}

impl Vad {
    /// Create a new VAD with sensible defaults for teleprompter use.
    pub fn new() -> Self {
        Self {
            noise_floor: 0.001,     // Initial low estimate
            multiplier: 4.0,        // Speech must be 4x above noise
            is_speaking: false,
            hangover_chunks: 5,     // 500ms hangover (5 × 100ms)
            hangover_remaining: 0,
            silence_ms: 0,
            chunk_ms: 100,          // 1600 samples at 16kHz = 100ms
            adapt_rate: 0.02,       // Slow adaptation
        }
    }

    /// Process one audio chunk and return the VAD result.
    pub fn process(&mut self, rms: f32) -> VadResult {
        let threshold = self.noise_floor * self.multiplier;
        let above_threshold = rms > threshold;

        if above_threshold {
            // Speech detected
            self.is_speaking = true;
            self.hangover_remaining = self.hangover_chunks;
            self.silence_ms = 0;
        } else if self.hangover_remaining > 0 {
            // In hangover period — keep "speaking" state active
            self.is_speaking = true;
            self.hangover_remaining -= 1;
            self.silence_ms = 0;
        } else {
            // Silence confirmed
            self.is_speaking = false;
            self.silence_ms += self.chunk_ms;

            // Adapt noise floor during confirmed silence
            // Only adapt upward slowly, downward faster (noise can decrease quickly)
            if rms > self.noise_floor {
                self.noise_floor += (rms - self.noise_floor) * self.adapt_rate;
            } else {
                self.noise_floor += (rms - self.noise_floor) * (self.adapt_rate * 3.0);
            }

            // Clamp noise floor to reasonable range
            self.noise_floor = self.noise_floor.clamp(0.0001, 0.02);
        }

        VadResult {
            speaking: self.is_speaking,
            silence_ms: self.silence_ms,
            energy: rms,
            noise_floor: self.noise_floor,
        }
    }

    /// Reset VAD state (e.g., at session start).
    pub fn reset(&mut self) {
        self.noise_floor = 0.001;
        self.is_speaking = false;
        self.hangover_remaining = 0;
        self.silence_ms = 0;
    }
}

impl Default for Vad {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_stays_silent() {
        let mut vad = Vad::new();
        // Feed very low energy (silence)
        for _ in 0..20 {
            let r = vad.process(0.0005);
            assert!(!r.speaking);
        }
        // Silence should have accumulated
        let r = vad.process(0.0005);
        assert!(r.silence_ms > 0);
    }

    #[test]
    fn speech_detected() {
        let mut vad = Vad::new();
        // Feed silence first to establish noise floor
        for _ in 0..10 {
            vad.process(0.0005);
        }
        // Now speech (high energy)
        let r = vad.process(0.05);
        assert!(r.speaking);
        assert_eq!(r.silence_ms, 0);
    }

    #[test]
    fn hangover_prevents_flapping() {
        let mut vad = Vad::new();
        // Establish noise floor
        for _ in 0..10 {
            vad.process(0.0005);
        }
        // Speech
        vad.process(0.05);
        assert!(vad.is_speaking);

        // Brief silence (1 chunk) — should stay speaking due to hangover
        let r = vad.process(0.0005);
        assert!(r.speaking, "hangover should keep speaking state");

        // After hangover expires (5 more silent chunks)
        for _ in 0..6 {
            vad.process(0.0005);
        }
        let r = vad.process(0.0005);
        assert!(!r.speaking, "should be silent after hangover");
    }

    #[test]
    fn noise_floor_adapts() {
        let mut vad = Vad::new();
        let initial = vad.noise_floor;

        // Feed moderate noise
        for _ in 0..100 {
            vad.process(0.003);
        }

        assert!(
            vad.noise_floor > initial,
            "noise floor should have adapted upward"
        );
    }
}
