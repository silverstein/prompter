#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prompter_core::script::{self, Directive, Element};
use prompter_core::Vad;
use serde::Serialize;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Emitter;

// ── Serializable types for the frontend ──

#[derive(Debug, Serialize)]
struct ScriptData {
    title: String,
    version: Option<String>,
    estimated_duration: Option<String>,
    sections: Vec<SectionData>,
    word_count: usize,
}

#[derive(Debug, Serialize)]
struct SectionData {
    name: String,
    elements: Vec<ElementData>,
    word_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum ElementData {
    #[serde(rename = "text")]
    Text { sentences: Vec<SentenceData> },
    #[serde(rename = "pause")]
    Pause { prompt: String },
    #[serde(rename = "branch")]
    Branch {
        question: String,
        options: Vec<BranchOptionData>,
    },
}

#[derive(Debug, Serialize)]
struct SentenceData {
    text: String,
    word_count: usize,
}

#[derive(Debug, Serialize)]
struct BranchOptionData {
    label: String,
    sentences: Vec<SentenceData>,
}

fn convert_script(s: script::Script) -> ScriptData {
    ScriptData {
        title: s.frontmatter.title,
        version: s.frontmatter.version,
        estimated_duration: s.frontmatter.estimated_duration,
        word_count: s.word_count,
        sections: s
            .sections
            .into_iter()
            .map(|sec| SectionData {
                name: sec.name,
                word_count: sec.word_count,
                elements: sec
                    .elements
                    .into_iter()
                    .map(|el| match el {
                        Element::Text(sentences) => ElementData::Text {
                            sentences: sentences
                                .into_iter()
                                .map(|s| SentenceData {
                                    text: s.text,
                                    word_count: s.word_count,
                                })
                                .collect(),
                        },
                        Element::Directive(Directive::Pause { prompt }) => {
                            ElementData::Pause { prompt }
                        }
                        Element::Directive(Directive::Branch { question, options }) => {
                            ElementData::Branch {
                                question,
                                options: options
                                    .into_iter()
                                    .map(|o| BranchOptionData {
                                        label: o.label,
                                        sentences: o
                                            .sentences
                                            .into_iter()
                                            .map(|s| SentenceData {
                                                text: s.text,
                                                word_count: s.word_count,
                                            })
                                            .collect(),
                                    })
                                    .collect(),
                            }
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ── Shared stop flag for audio thread ──
// AudioStream contains cpal::Stream which is !Send, so we can't store it
// in Tauri state. Instead we spawn a dedicated thread that owns the stream
// and communicate via an atomic stop flag.

static AUDIO_RUNNING: AtomicBool = AtomicBool::new(false);
// Use a lazy-initialized Arc<AtomicBool> for the stop signal
static AUDIO_STOP: std::sync::LazyLock<Arc<AtomicBool>> =
    std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

// ── Tauri commands ──

#[tauri::command]
fn load_script(path: String) -> Result<ScriptData, String> {
    let content = fs::read_to_string(&path).map_err(|e| format!("Could not read file: {}", e))?;
    let parsed = script::parse(&content).map_err(|e| format!("{}", e))?;
    Ok(convert_script(parsed))
}

#[tauri::command]
fn parse_script_text(text: String) -> Result<ScriptData, String> {
    let parsed = script::parse(&text).map_err(|e| format!("{}", e))?;
    Ok(convert_script(parsed))
}

/// VAD event emitted to the frontend ~10 times per second.
#[derive(Clone, Serialize)]
struct VadEvent {
    speaking: bool,
    silence_ms: u64,
    level: u32,
}

/// Start audio capture + VAD. Emits "vad" events to the frontend.
#[tauri::command]
fn start_audio(app: tauri::AppHandle) -> Result<String, String> {
    if AUDIO_RUNNING.load(Ordering::Relaxed) {
        return Err("Audio already running".into());
    }

    // Reset stop flag
    AUDIO_STOP.store(false, Ordering::Relaxed);
    AUDIO_RUNNING.store(true, Ordering::Relaxed);

    let stop = Arc::clone(&AUDIO_STOP);

    // Spawn a dedicated thread that owns the AudioStream (which is !Send)
    // This thread creates the stream, runs VAD, and emits events.
    std::thread::spawn(move || {
        let stream = match prompter_core::AudioStream::start() {
            Ok(s) => s,
            Err(e) => {
                let _ = app.emit("vad-error", format!("{}", e));
                AUDIO_RUNNING.store(false, Ordering::Relaxed);
                return;
            }
        };

        let _ = app.emit("audio-started", &stream.device_name);

        let rx = stream.receiver.clone();
        let mut vad = Vad::new();

        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            match rx.recv_timeout(std::time::Duration::from_millis(150)) {
                Ok(chunk) => {
                    let result = vad.process(chunk.rms);
                    let _ = app.emit(
                        "vad",
                        VadEvent {
                            speaking: result.speaking,
                            silence_ms: result.silence_ms,
                            level: (result.energy * 2000.0).min(100.0) as u32,
                        },
                    );
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }

        drop(stream);
        AUDIO_RUNNING.store(false, Ordering::Relaxed);
    });

    Ok("starting".into())
}

/// Stop audio capture.
#[tauri::command]
fn stop_audio() -> Result<(), String> {
    AUDIO_STOP.store(true, Ordering::Relaxed);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            load_script,
            parse_script_text,
            start_audio,
            stop_audio
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prompter");
}
