#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prompter_core::script::{self, Directive, Element};
use prompter_core::{ScriptTracker, SessionRecorder, SpeechUpdate, TrackState, TrackUpdate};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

// ── Serializable types for the frontend ──

#[derive(Debug, Serialize)]
struct ScriptData {
    title: String,
    version: Option<String>,
    estimated_duration: Option<String>,
    sections: Vec<SectionData>,
    word_count: usize,
    /// The raw .script.md source, so the frontend can hand it back to
    /// `init_tracking` (which parses it into a tracker) without re-reading.
    source: String,
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

fn convert_script(s: script::Script, source: String) -> ScriptData {
    ScriptData {
        title: s.frontmatter.title,
        version: s.frontmatter.version,
        estimated_duration: s.frontmatter.estimated_duration,
        word_count: s.word_count,
        source,
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
    Ok(convert_script(parsed, content))
}

#[tauri::command]
fn parse_script_text(text: String) -> Result<ScriptData, String> {
    let parsed = script::parse(&text).map_err(|e| format!("{}", e))?;
    Ok(convert_script(parsed, text))
}

// ── Rust-side script tracking (the canonical aligner) ──
//
// The frontend renders the script and (for now) still drives the visible scroll
// with its own matcher, but the recognized speech is also fed here so coverage
// and the compliance report come from real alignment evidence, not the cursor
// position. `track-update` events are emitted for the UI to consume.

struct TrackingSession {
    tracker: ScriptTracker,
    recorder: SessionRecorder,
}

#[derive(Default)]
struct TrackingState(Mutex<Option<TrackingSession>>);

/// Tauri event payload for one tracker update.
#[derive(Clone, Serialize)]
struct TrackEvent {
    sentence_index: usize,
    timeline_index: usize,
    committed: bool,
    matched: bool,
    state: String,
    prompt: Option<String>,
    question: Option<String>,
    options: Option<Vec<String>>,
    option_label: Option<String>,
    branch_question: Option<String>,
    selected_option: Option<String>,
}

fn track_event(u: &TrackUpdate) -> TrackEvent {
    let (state, prompt, question, options, option_label) = match &u.state {
        TrackState::Speaking => ("speaking", None, None, None, None),
        TrackState::AtPause { prompt } => ("pause", Some(prompt.clone()), None, None, None),
        TrackState::AtBranch { question, options } => (
            "branch",
            None,
            Some(question.clone()),
            Some(options.clone()),
            None,
        ),
        TrackState::InBranch { option_label } => {
            ("in_branch", None, None, None, Some(option_label.clone()))
        }
        TrackState::AdLibbing => ("adlib", None, None, None, None),
    };
    TrackEvent {
        sentence_index: u.sentence_index,
        timeline_index: u.timeline_index,
        committed: u.committed,
        matched: u.matched,
        state: state.to_string(),
        prompt,
        question,
        options,
        option_label,
        branch_question: u.branch_choice.as_ref().map(|c| c.question.clone()),
        selected_option: u.branch_choice.as_ref().map(|c| c.option_label.clone()),
    }
}

/// The last `n` whitespace-separated words of `text`.
///
/// Apple's `SFSpeechRecognizer` streams the *cumulative* utterance (the whole
/// thing, growing). The bigram-Dice aligner only matches ~1-3 sentences, so the
/// full cumulative string stops matching a few sentences in and the cursor
/// freezes. Feeding only the leading edge (recent words) keeps the match local,
/// the way the prior JS matcher's "last 6 words" did.
fn recent_words(text: &str, n: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let start = words.len().saturating_sub(n);
    words[start..].join(" ")
}

/// Start a tracking session for `text` (the .script.md source).
#[tauri::command]
fn init_tracking(state: tauri::State<TrackingState>, text: String) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "tracking state poisoned")?;
    // Clear any prior session FIRST: a failed init (or a new session) must not
    // leave a stale tracker that later speech could feed into.
    *slot = None;
    let parsed = script::parse(&text).map_err(|e| format!("{}", e))?;
    let mut tracker = ScriptTracker::new(&parsed);
    // Tight search window for live ASR following: keeps the match local so a
    // spurious far match can't throw the cursor across the script.
    tracker.set_window_radius(6);
    *slot = Some(TrackingSession {
        tracker,
        recorder: SessionRecorder::new(&parsed),
    });
    Ok(())
}

/// Clear the tracking session (e.g. on reset), so a stale tracker cannot mis-track.
#[tauri::command]
fn clear_tracking(state: tauri::State<TrackingState>) {
    if let Ok(mut slot) = state.0.lock() {
        *slot = None;
    }
}

/// Speech-verified compliance report returned to the frontend at session end.
#[derive(Clone, Serialize)]
struct ComplianceOut {
    script_title: String,
    script_version: Option<String>,
    sections_covered: Vec<String>,
    sections_skipped: Vec<String>,
    duration_secs: u64,
    pause_points_reached: usize,
    pause_points_total: usize,
    branches_taken: HashMap<String, String>,
    total_words: usize,
    words_delivered: usize,
    adherence_pct: f64,
    saved_path: String,
    transcript_markdown: String,
}

/// Finish the tracking session: build the speech-verified compliance report,
/// write it (and the transcript) to disk, and return it.
#[tauri::command]
fn finish_tracking(
    state: tauri::State<TrackingState>,
    duration_secs: u64,
    section_times: HashMap<String, u64>,
) -> Result<ComplianceOut, String> {
    // Take (and clear) the session, then release the lock before disk I/O so the
    // speech reader is never blocked and a stale session can't leak forward.
    let session = {
        let mut slot = state.0.lock().map_err(|_| "tracking state poisoned")?;
        slot.take()
    }
    .ok_or("no active tracking session (call init_tracking first)")?;

    let mut report = session.recorder.build_report(duration_secs);
    // The recorder has no clock; the UI supplies per-section timing.
    report.section_times = section_times;
    let transcript = session.recorder.transcript_markdown();

    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = home.join("meetings").join("consults");
    let path = report
        .write_to_dir(&dir)
        .map_err(|e| format!("Failed to save compliance report: {}", e))?;
    // Best-effort transcript artifact next to the report.
    let transcript_path = path.with_extension("transcript.md");
    let _ = fs::write(&transcript_path, &transcript);

    Ok(ComplianceOut {
        script_title: report.script_title.clone(),
        script_version: report.script_version.clone(),
        sections_covered: report.sections_covered.clone(),
        sections_skipped: report.sections_skipped.clone(),
        duration_secs: report.duration_secs,
        pause_points_reached: report.pause_points_reached,
        pause_points_total: report.pause_points_total,
        branches_taken: report.branches_taken.clone(),
        total_words: report.total_words,
        words_delivered: report.words_delivered,
        adherence_pct: report.adherence_pct(),
        saved_path: path.to_string_lossy().to_string(),
        transcript_markdown: transcript,
    })
}

/// Start speech recognition using Apple's SFSpeechRecognizer via Swift subprocess.
/// Streams recognized text to the frontend as "speech" events.
#[tauri::command]
fn start_speech(app: tauri::AppHandle) -> Result<String, String> {
    if AUDIO_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Kill previous
        AUDIO_STOP.store(true, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(300));
        AUDIO_RUNNING.store(true, Ordering::SeqCst);
    }
    AUDIO_STOP.store(false, Ordering::SeqCst);
    let stop = Arc::clone(&AUDIO_STOP);

    // Find the speech-recognizer binary (bundled next to the app binary)
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let recognizer_path = exe_dir.join("speech-recognizer");

    if !recognizer_path.exists() {
        return Err(format!(
            "Speech recognizer not found at {}",
            recognizer_path.display()
        ));
    }

    std::thread::spawn(move || {
        use std::io::BufRead;

        eprintln!(
            "[prompter] Starting speech recognizer: {}",
            recognizer_path.display()
        );

        let mut child = match std::process::Command::new(&recognizer_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[prompter] Failed to spawn speech recognizer: {}", e);
                let _ = app.emit("speech-error", format!("{}", e));
                AUDIO_RUNNING.store(false, Ordering::Relaxed);
                return;
            }
        };

        let stdout = child.stdout.take().unwrap();
        let reader = std::io::BufReader::new(stdout);

        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                break;
            }

            if let Ok(line) = line {
                eprintln!("[prompter] Speech: {}", &line[..line.len().min(100)]);
                // Parse JSON and emit to frontend
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(text) = val.get("text").and_then(|t| t.as_str()) {
                        let is_final = val.get("final").and_then(|f| f.as_bool()).unwrap_or(false);

                        #[derive(Clone, Serialize)]
                        struct SpeechEvent {
                            text: String,
                            is_final: bool,
                        }

                        let _ = app.emit(
                            "speech",
                            SpeechEvent {
                                text: text.to_string(),
                                is_final,
                            },
                        );

                        // Feed the canonical Rust tracker: accumulate compliance
                        // evidence, then emit a track-update for the UI. The
                        // lock guard is dropped at the end of this block, before
                        // the emit, to avoid holding it across the borrow.
                        let track = {
                            let tstate = app.state::<TrackingState>();
                            // Recover a poisoned lock rather than silently
                            // dropping tracking (which would stall the scroll).
                            let mut guard = tstate.0.lock().unwrap_or_else(|p| p.into_inner());
                            guard.as_mut().map(|s| {
                                // Align on the leading edge (recent words), but
                                // record the full recognized text in the transcript.
                                let update = s.tracker.observe(&SpeechUpdate {
                                    text: recent_words(text, 10),
                                    words: Vec::new(),
                                    is_final,
                                });
                                s.recorder.record(&update, text);
                                track_event(&update)
                            })
                        };
                        if let Some(ev) = track {
                            let _ = app.emit("track-update", ev);
                        }
                    }
                }
            }
        }

        // Clean up
        let _ = child.kill();
        let _ = child.wait();
        AUDIO_RUNNING.store(false, Ordering::Relaxed);
        eprintln!("[prompter] Speech recognizer stopped");
    });

    Ok("started".into())
}

/// Stop speech recognition.
#[tauri::command]
fn stop_speech() -> Result<(), String> {
    AUDIO_STOP.store(true, Ordering::SeqCst);
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
    #[serde(default = "default_highlight_mode")]
    highlight_mode: String,
    #[serde(default)]
    recent_scripts: Vec<RecentScript>,
}

fn default_highlight_mode() -> String {
    "soft".to_string()
}

fn default_font_size() -> u32 {
    34
}
fn default_speed() -> u32 {
    150
}

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
    settings.recent_scripts.insert(
        0,
        RecentScript {
            path,
            title,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        },
    );

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

                    let modified = entry
                        .metadata()
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

/// Generate coaching insights from a compliance report.
#[tauri::command]
fn get_coaching(report: SessionReport) -> Vec<CoachingInsight> {
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

    prompter_core::coaching::analyze(&compliance)
        .into_iter()
        .map(|i| CoachingInsight {
            severity: match i.severity {
                prompter_core::coaching::Severity::Praise => "praise".into(),
                prompter_core::coaching::Severity::Info => "info".into(),
                prompter_core::coaching::Severity::Warning => "warning".into(),
                prompter_core::coaching::Severity::Critical => "critical".into(),
            },
            message: i.message,
            advice: i.advice,
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct CoachingInsight {
    severity: String,
    message: String,
    advice: String,
}

/// Find a script file by consultation_id in the watched folder.
fn find_script_by_consultation_id(consultation_id: &str) -> Option<String> {
    let home = dirs_next::home_dir()?;
    let scripts_dir = home.join("meetings").join("scripts");

    if let Ok(entries) = fs::read_dir(&scripts_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            // Check filename contains the consultation_id
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.contains(consultation_id) {
                    return Some(path.to_string_lossy().to_string());
                }
            }
            // Also check frontmatter for consultation_id field
            if let Ok(content) = fs::read_to_string(&path) {
                if content.contains(&format!("consultation_id: \"{}\"", consultation_id))
                    || content.contains(&format!("consultation_id: {}", consultation_id))
                {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

/// Parse a deep link URL and extract parameters.
/// Supports: prompter://open?file=/path/to/script.md
///           prompter://open?consultation_id=abc-123
fn parse_deep_link(url: &str) -> Option<(String, String)> {
    // Strip the scheme
    let rest = url.strip_prefix("prompter://").unwrap_or(url);
    let rest = rest.strip_prefix("open").unwrap_or(rest);
    let rest = rest.strip_prefix('?').unwrap_or(rest);

    for param in rest.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            let value = urlencoding_decode(value);
            return Some((key.to_string(), value));
        }
    }
    None
}

/// URL decoding — collects percent-encoded bytes then decodes as UTF-8.
fn urlencoding_decode(s: &str) -> String {
    let mut bytes: Vec<u8> = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                bytes.push(byte);
            }
        } else if c == '+' {
            bytes.push(b' ');
        } else if c.is_ascii() {
            bytes.push(c as u8);
        } else {
            // Non-ASCII char not percent-encoded — encode as UTF-8
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            bytes.extend_from_slice(encoded.as_bytes());
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(TrackingState::default())
        .setup(|app| {
            use tauri::Listener;
            // Handle deep links (prompter://open?file=... or prompter://open?consultation_id=...)
            let handle = app.handle().clone();
            app.handle().listen("deep-link://new-url", move |event| {
                let payload = event.payload();
                if let Ok(urls) = serde_json::from_str::<Vec<String>>(payload) {
                    for url in urls {
                        if let Some((key, value)) = parse_deep_link(&url) {
                            match key.as_str() {
                                "file" => {
                                    let _ = handle.emit("deep-link-open", value);
                                }
                                "consultation_id" => {
                                    if let Some(path) = find_script_by_consultation_id(&value) {
                                        let _ = handle.emit("deep-link-open", path);
                                    } else {
                                        let _ = handle.emit(
                                            "deep-link-error",
                                            format!("No script found for consultation {}", value),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_script,
            parse_script_text,
            init_tracking,
            clear_tracking,
            finish_tracking,
            start_speech,
            stop_speech,
            save_compliance,
            get_coaching,
            load_settings,
            save_settings,
            add_recent_script,
            set_always_on_top,
            list_available_scripts
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prompter");
}
