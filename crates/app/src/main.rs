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

/// Save compliance report after session ends.
#[derive(serde::Deserialize)]
struct SessionReport {
    script_title: String,
    script_version: Option<String>,
    sections_covered: Vec<String>,
    sections_skipped: Vec<String>,
    duration_secs: u64,
    section_times: std::collections::HashMap<String, u64>,
    pause_points_reached: usize,
    pause_points_total: usize,
    branches_taken: std::collections::HashMap<String, String>,
    total_words: usize,
    words_delivered: usize,
}

#[tauri::command]
fn save_compliance(report: SessionReport) -> Result<String, String> {
    let compliance = prompter_core::ComplianceReport {
        script_title: report.script_title,
        script_version: report.script_version,
        sections_covered: report.sections_covered,
        sections_skipped: report.sections_skipped,
        duration_secs: report.duration_secs,
        section_times: report.section_times,
        pause_points_reached: report.pause_points_reached,
        pause_points_total: report.pause_points_total,
        branches_taken: report.branches_taken,
        total_words: report.total_words,
        words_delivered: report.words_delivered,
    };

    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = home.join("meetings").join("consults");

    let path = compliance
        .write_to_dir(&dir)
        .map_err(|e| format!("Failed to save compliance report: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}

// ── Settings persistence (~/.prompter/settings.json) ──

fn settings_path() -> std::path::PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".prompter").join("settings.json")
}

#[derive(Debug, Serialize, serde::Deserialize, Default)]
struct Settings {
    #[serde(default = "default_font_size")]
    font_size: u32,
    #[serde(default = "default_speed")]
    speed: u32,
    #[serde(default)]
    always_on_top: bool,
    #[serde(default)]
    recent_scripts: Vec<RecentScript>,
}

fn default_font_size() -> u32 { 26 }
fn default_speed() -> u32 { 150 }

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct RecentScript {
    path: String,
    title: String,
    timestamp: u64,
}

#[tauri::command]
fn load_settings() -> Settings {
    let path = settings_path();
    if let Ok(data) = fs::read_to_string(&path) {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        Settings::default()
    }
}

#[tauri::command]
fn save_settings(settings: Settings) -> Result<(), String> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn add_recent_script(path: String, title: String) -> Result<(), String> {
    let mut settings = load_settings();

    // Remove duplicate if exists
    settings.recent_scripts.retain(|r| r.path != path);

    // Add to front
    settings.recent_scripts.insert(0, RecentScript {
        path,
        title,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    });

    // Keep max 10
    settings.recent_scripts.truncate(10);

    save_settings(settings)
}

// ── Always-on-top ──

#[tauri::command]
fn set_always_on_top(app: tauri::AppHandle, on_top: bool) -> Result<(), String> {
    use tauri::Manager;
    if let Some(win) = app.get_webview_window("main") {
        win.set_always_on_top(on_top).map_err(|e| e.to_string())?;
    }
    // Persist
    let mut settings = load_settings();
    settings.always_on_top = on_top;
    save_settings(settings)?;
    Ok(())
}

// ── List scripts in watched folder ──

#[tauri::command]
fn list_available_scripts() -> Vec<RecentScript> {
    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let scripts_dir = home.join("meetings").join("scripts");
    let mut results = Vec::new();

    if let Ok(entries) = fs::read_dir(&scripts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(content) = fs::read_to_string(&path) {
                    let title = if let Ok(parsed) = script::parse(&content) {
                        parsed.frontmatter.title
                    } else {
                        path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Untitled")
                            .to_string()
                    };

                    let modified = entry.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    results.push(RecentScript {
                        path: path.to_string_lossy().to_string(),
                        title,
                        timestamp: modified,
                    });
                }
            }
        }
    }

    // Sort newest first
    results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    results
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            load_script,
            parse_script_text,
            start_audio,
            stop_audio,
            save_compliance,
            load_settings,
            save_settings,
            add_recent_script,
            set_always_on_top,
            list_available_scripts
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prompter");
}
