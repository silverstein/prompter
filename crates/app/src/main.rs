#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use prompter_core::script::{self, Directive, Element};
use prompter_core::{
    realign, recent_words, write_private, ScriptTracker, SessionRecorder, SpeechUpdate,
    TrackState, TrackUpdate,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
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
/// PID of the running speech helper (0 = none), so stop can signal it directly
/// instead of waiting for its next output line.
static SPEECH_PID: AtomicU32 = AtomicU32::new(0);
/// True while the other party (call audio) is speaking.
static OTHER_SPEAKING: AtomicBool = AtomicBool::new(false);

/// Ask the speech helper to stop cleanly (SIGTERM lets it close the recording).
fn signal_speech_helper() {
    let pid = SPEECH_PID.load(Ordering::SeqCst);
    if pid != 0 {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
    }
}

/// Path of the bundled speech helper (next to the app binary).
fn speech_helper_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("speech-recognizer")
}

/// Create a private per-session folder under ~/.prompter/sessions.
fn new_session_dir() -> Option<std::path::PathBuf> {
    let home = dirs_next::home_dir()?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    let dir = home.join(".prompter").join("sessions").join(stamp.to_string());
    fs::create_dir_all(&dir).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    Some(dir)
}

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
    /// Session start, for timestamping the ASR recording.
    started: Instant,
    /// Where the raw ASR stream is logged for offline replay (None if disabled).
    recording: Option<std::path::PathBuf>,
    /// Private per-session folder: the script copy (for the custom language
    /// model) and the audio recording (for the post-session verification pass).
    session_dir: Option<std::path::PathBuf>,
    /// Seconds the other party was heard speaking (call audio), if watched.
    patient_talk_secs: f64,
    watched_other_party: bool,
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
    relocated: bool,
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
        relocated: u.relocated,
    }
}

/// Path of the rolling ASR recording (for offline replay/eval).
fn recording_path() -> std::path::PathBuf {
    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".prompter").join("recording.jsonl")
}

/// Start a tracking session for `text` (the .script.md source).
#[tauri::command]
fn init_tracking(state: tauri::State<TrackingState>, text: String) -> Result<(), String> {
    let mut slot = state.0.lock().map_err(|_| "tracking state poisoned")?;
    // Clear any prior session FIRST: a failed init (or a new session) must not
    // leave a stale tracker that later speech could feed into. Its folder goes
    // too, or a quick double start leaves an orphan behind.
    if let Some(dir) = slot.as_ref().and_then(|s| s.session_dir.clone()) {
        let _ = fs::remove_dir_all(dir);
    }
    *slot = None;
    let parsed = script::parse(&text).map_err(|e| format!("{}", e))?;
    let mut tracker = ScriptTracker::new(&parsed);
    // Window wide enough to recover when the cursor falls behind a few
    // sentences; MAX_ADVANCE in the tracker caps any single forward jump. (Too
    // tight a window froze the cursor once it fell outside it.)
    tracker.set_window_radius(10);
    // Start a fresh ASR recording for offline replay/eval.
    let recording = {
        let path = recording_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let header = serde_json::json!({ "type": "script", "source": text }).to_string();
        fs::write(&path, format!("{header}\n")).ok().map(|_| path)
    };
    let session_dir = new_session_dir();
    if let Some(dir) = &session_dir {
        // The language model learns what will actually be said: sentences with
        // variables filled in (drug names, not "{{supplements}}"), pause
        // questions, and every branch option. One phrase per line.
        let _ = write_private(&dir.join("script.md"), &spoken_phrases(&parsed));
    }
    OTHER_SPEAKING.store(false, Ordering::SeqCst);
    *slot = Some(TrackingSession {
        tracker,
        recorder: SessionRecorder::new(&parsed),
        started: Instant::now(),
        recording,
        session_dir,
        patient_talk_secs: 0.0,
        watched_other_party: false,
    });
    Ok(())
}

/// Every line the speaker may say, variables substituted, one per line.
fn spoken_phrases(parsed: &script::Script) -> String {
    let mut out = String::new();
    for section in &parsed.sections {
        for element in &section.elements {
            match element {
                Element::Text(sentences) => {
                    for s in sentences {
                        out.push_str(&s.text);
                        out.push('\n');
                    }
                }
                Element::Directive(Directive::Pause { prompt }) => {
                    out.push_str(prompt);
                    out.push('\n');
                }
                Element::Directive(Directive::Branch { question, options }) => {
                    out.push_str(question);
                    out.push('\n');
                    for o in options {
                        for s in &o.sentences {
                            out.push_str(&s.text);
                            out.push('\n');
                        }
                    }
                }
            }
        }
    }
    out
}

/// Manual navigation (arrow keys, section jump): re-anchor the tracker so the
/// next recognized speech is matched from where the speaker says they are. The
/// one-key "I'm here" override every tracking system keeps.
#[tauri::command]
fn set_tracking_position(state: tauri::State<TrackingState>, sentence_index: usize) {
    if let Ok(mut slot) = state.0.lock() {
        if let Some(s) = slot.as_mut() {
            s.tracker.set_position(sentence_index);
        }
    }
}

/// The operator picked a branch answer by hand: the tracker follows from there.
#[tauri::command]
fn choose_branch(state: tauri::State<TrackingState>, branch: usize, option: usize) {
    if let Ok(mut slot) = state.0.lock() {
        if let Some(s) = slot.as_mut() {
            s.tracker.choose_branch(branch, option);
        }
    }
}

/// Clear the tracking session (e.g. on reset), so a stale tracker cannot mis-track.
#[tauri::command]
fn clear_tracking(state: tauri::State<TrackingState>) {
    if let Ok(mut slot) = state.0.lock() {
        if let Some(dir) = slot.as_ref().and_then(|s| s.session_dir.clone()) {
            let _ = fs::remove_dir_all(dir);
        }
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
    /// Coaching computed from the full report (including delivery stats).
    coaching: Vec<CoachingInsight>,
    /// True when coverage was confirmed against the full recording.
    verified_by_recording: bool,
    /// True when a verification pass is running; a `verification-complete`
    /// (or `verification-failed`) event follows.
    verifying: bool,
}

fn coaching_out(report: &prompter_core::ComplianceReport) -> Vec<CoachingInsight> {
    prompter_core::coaching::analyze(report)
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

fn compliance_out(
    report: &prompter_core::ComplianceReport,
    saved_path: &std::path::Path,
    transcript: String,
    verifying: bool,
) -> ComplianceOut {
    ComplianceOut {
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
        saved_path: saved_path.to_string_lossy().to_string(),
        transcript_markdown: transcript,
        coaching: coaching_out(report),
        verified_by_recording: report.delivery.verified_by_recording,
        verifying,
    }
}

/// Finish the tracking session: build the speech-verified compliance report,
/// write it (and the transcript) to disk, and return it. If the session was
/// recorded, a background pass then re-transcribes the whole recording, aligns
/// it against the script, rewrites the report, and emits
/// `verification-complete`.
#[tauri::command]
fn finish_tracking(
    app: tauri::AppHandle,
    state: tauri::State<TrackingState>,
    duration_secs: u64,
    section_times: HashMap<String, u64>,
) -> Result<ComplianceOut, String> {
    // Take (and clear) the session, then release the lock before disk I/O so the
    // speech reader is never blocked and a stale session can't leak forward.
    let mut session = {
        let mut slot = state.0.lock().map_err(|_| "tracking state poisoned")?;
        slot.take()
    }
    .ok_or("no active tracking session (call init_tracking first)")?;

    let patient_secs = session
        .watched_other_party
        .then_some(session.patient_talk_secs.round() as u64);
    session.recorder.set_timing(None, patient_secs);
    let mut report = session.recorder.build_report(duration_secs);
    // The recorder has no clock; the UI supplies per-section timing.
    report.section_times = section_times.clone();
    let transcript = session.recorder.transcript_markdown();

    let home = dirs_next::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let dir = home.join("meetings").join("consults");
    let path = report
        .write_to_dir(&dir)
        .map_err(|e| format!("Failed to save compliance report: {}", e))?;
    // Best-effort transcript artifact next to the report (0600: patient speech).
    let _ = write_private(&path.with_extension("transcript.md"), &transcript);

    let audio = session
        .session_dir
        .as_ref()
        .map(|d| d.join("audio.caf"))
        .filter(|a| a.exists());
    let verifying = audio.is_some();
    let out = compliance_out(&report, &path, transcript.clone(), verifying);

    match audio {
        Some(audio) => {
            let session_dir = session.session_dir.clone();
            std::thread::spawn(move || {
                let started = Instant::now();
                let result = verify_with_recording(&mut session, &audio, duration_secs, &section_times, &path);
                log_verification(duration_secs, started.elapsed().as_secs_f32(), &result);
                if !load_settings().keep_session_audio {
                    if let Some(d) = session_dir {
                        let _ = fs::remove_dir_all(d);
                    }
                }
                match result {
                    Ok(report) => {
                        let _ = app.emit(
                            "verification-complete",
                            compliance_out(&report, &path, transcript, false),
                        );
                    }
                    Err(e) => {
                        eprintln!("[prompter] verification failed: {e}");
                        let _ = app.emit("verification-failed", e);
                    }
                }
            });
        }
        None => {
            if let Some(d) = &session.session_dir {
                let _ = fs::remove_dir_all(d);
            }
        }
    }
    Ok(out)
}

/// The post-session pass: re-transcribe the whole recording (on-device, biased
/// toward the script), align it against the script, and rewrite the report
/// with recording-verified coverage, speaking pace, and off-script words.
/// Append one line per verification pass to `~/.prompter/verification.log`
/// (outcome and timing only, never transcript text), so a failure the user
/// dismissed can still be diagnosed.
fn log_verification(duration_secs: u64, took_secs: f32, result: &Result<prompter_core::ComplianceReport, String>) {
    use std::io::Write;
    let Some(home) = dirs_next::home_dir() else { return };
    let path = home.join(".prompter").join("verification.log");
    let outcome = match result {
        Ok(_) => "ok".to_string(),
        Err(e) => format!("failed: {e}"),
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(path) {
        let _ = writeln!(f, "{stamp} session={duration_secs}s check={took_secs:.1}s {outcome}");
    }
}

fn verify_with_recording(
    session: &mut TrackingSession,
    audio: &std::path::Path,
    duration_secs: u64,
    section_times: &HashMap<String, u64>,
    report_path: &std::path::Path,
) -> Result<prompter_core::ComplianceReport, String> {
    // Let the live helper finish closing the recording.
    let deadline = Instant::now() + std::time::Duration::from_secs(8);
    while AUDIO_RUNNING.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let helper = speech_helper_path();
    let mut cmd = std::process::Command::new(&helper);
    cmd.arg("--file").arg(audio);
    if let Some(dir) = &session.session_dir {
        cmd.arg("--script").arg(dir.join("script.md"));
    }
    let output = cmd.output().map_err(|e| format!("could not run speech helper: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find(|v| v.get("file_text").is_some())
        .ok_or_else(|| {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if err.is_empty() { "no transcript from recording".to_string() } else { err }
        })?;
    let text = parsed["file_text"].as_str().unwrap_or_default().to_string();
    let words = parsed["words"].as_u64().unwrap_or(0) as f32;
    let errors: Vec<&str> = parsed["errors"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| e.as_str()).collect())
        .unwrap_or_default();
    if words == 0.0 && !errors.is_empty() {
        return Err(format!("speech recognizer: {}", errors.join("; ")));
    }
    let speaking_secs = parsed["speaking_secs"].as_f64().unwrap_or(0.0) as f32;

    let realigned = realign(session.recorder.sentences(), &text);
    if !session.recorder.apply_realignment(&realigned) {
        return Err("recording too short or unclear to verify".into());
    }
    let wpm = (speaking_secs >= 30.0).then(|| words / (speaking_secs / 60.0));
    session.recorder.set_timing(wpm, None);
    let mut report = session.recorder.build_report(duration_secs);
    report.section_times = section_times.clone();
    report
        .rewrite(report_path)
        .map_err(|e| format!("could not update report: {e}"))?;
    let _ = write_private(
        &report_path.with_extension("recording-transcript.md"),
        &format!("# Full-recording transcript — {}\n\n{}\n", report.script_title, text),
    );
    Ok(report)
}

/// Start speech recognition using Apple's SFSpeechRecognizer via Swift subprocess.
/// Streams recognized text to the frontend as "speech" events. When a tracking
/// session is active, the helper also builds a custom language model from the
/// script and records the microphone for the post-session verification pass;
/// with "detect the other party" on, it watches call audio too.
#[tauri::command]
fn start_speech(app: tauri::AppHandle) -> Result<String, String> {
    if AUDIO_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Stop the previous helper first.
        AUDIO_STOP.store(true, Ordering::SeqCst);
        signal_speech_helper();
        std::thread::sleep(std::time::Duration::from_millis(300));
        AUDIO_RUNNING.store(true, Ordering::SeqCst);
    }
    AUDIO_STOP.store(false, Ordering::SeqCst);
    OTHER_SPEAKING.store(false, Ordering::SeqCst);
    let stop = Arc::clone(&AUDIO_STOP);

    let recognizer_path = speech_helper_path();
    if !recognizer_path.exists() {
        AUDIO_RUNNING.store(false, Ordering::SeqCst);
        return Err(format!(
            "Speech recognizer not found at {}",
            recognizer_path.display()
        ));
    }

    let settings = load_settings();
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    {
        let tstate = app.state::<TrackingState>();
        let mut guard = tstate.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = guard.as_mut() {
            if let Some(dir) = &s.session_dir {
                args.push("--script".into());
                args.push(dir.join("script.md").into());
                if settings.verify_with_recording {
                    args.push("--record".into());
                    args.push(dir.join("audio.caf").into());
                }
            }
            s.watched_other_party = settings.detect_other_party;
        }
    }
    if settings.detect_other_party {
        args.push("--system-audio".into());
    }

    std::thread::spawn(move || {
        use std::io::BufRead;

        eprintln!(
            "[prompter] Starting speech recognizer: {} {:?}",
            recognizer_path.display(),
            args
        );

        let mut child = match std::process::Command::new(&recognizer_path)
            .args(&args)
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
        SPEECH_PID.store(child.id(), Ordering::SeqCst);

        // Surface helper errors instead of failing silently (a denied
        // microphone / speech permission used to just leave the prompter idle).
        if let Some(stderr) = child.stderr.take() {
            let app_err = app.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[prompter] helper: {line}");
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue;
                    };
                    let Some(err) = v.get("error").and_then(|e| e.as_str()) else {
                        continue;
                    };
                    let user_msg = match err {
                        e if e.starts_with("speech_auth") => Some(
                            "Speech recognition permission is off. Turn it on for Prompter in System Settings > Privacy & Security > Speech Recognition.".to_string(),
                        ),
                        "recognizer_unavailable" => {
                            Some("On-device speech recognition isn't available on this Mac.".to_string())
                        }
                        e if e.starts_with("audio_engine") => Some(format!(
                            "Couldn't open the microphone ({e}). Check System Settings > Privacy & Security > Microphone."
                        )),
                        e if e.starts_with("system_audio") => Some(format!(
                            "Call-audio detection stopped ({e}). Tracking continues without it."
                        )),
                        "screen_capture_denied" => Some(
                            "Call-audio detection needs Screen & System Audio Recording permission for Prompter. Tracking continues without it.".to_string(),
                        ),
                        _ => None,
                    };
                    if let Some(msg) = user_msg {
                        // Call-audio problems are non-fatal: voice tracking
                        // carries on, so don't trigger the timer fallback.
                        let event = if err.starts_with("screen_capture") || err.starts_with("system_audio") {
                            "speech-warning"
                        } else {
                            "speech-error"
                        };
                        let _ = app_err.emit(event, msg);
                    }
                }
            });
        }

        let stdout = child.stdout.take().unwrap();
        let reader = std::io::BufReader::new(stdout);

        for line in reader.lines() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(line) = line else { continue };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            // Status events from the helper (language model, call audio).
            if let Some(ev) = val.get("event").and_then(|e| e.as_str()) {
                eprintln!("[prompter] helper event: {line}");
                let _ = app.emit("speech-status", ev.to_string());
                continue;
            }

            // Other party (call audio) started / finished speaking.
            if let Some(other) = val.get("other").and_then(|o| o.as_bool()) {
                OTHER_SPEAKING.store(other, Ordering::SeqCst);
                if !other {
                    let secs = val.get("secs").and_then(|s| s.as_f64()).unwrap_or(0.0);
                    let tstate = app.state::<TrackingState>();
                    let mut guard = tstate.0.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(s) = guard.as_mut() {
                        s.patient_talk_secs += secs;
                    }
                }
                let _ = app.emit("other-party", other);
                continue;
            }

            let Some(text) = val.get("text").and_then(|t| t.as_str()) else {
                continue;
            };
            let is_final = val.get("final").and_then(|f| f.as_bool()).unwrap_or(false);
            let preview: String = text.chars().take(100).collect();
            eprintln!("[prompter] Speech: {preview}");

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

            // While the other party is talking, their voice can leak into the
            // mic through the speakers. Don't let it steer the cursor.
            if OTHER_SPEAKING.load(Ordering::SeqCst) {
                continue;
            }

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
                    // Log the raw ASR event for offline replay/eval.
                    if let Some(path) = &s.recording {
                        let line = serde_json::json!({
                            "type": "asr",
                            "t": s.started.elapsed().as_millis() as u64,
                            "text": text,
                            "final": is_final,
                        })
                        .to_string();
                        use std::io::Write;
                        if let Ok(mut f) = fs::OpenOptions::new().append(true).open(path) {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    // Align on and record the leading edge (recent
                    // words). The recognizer streams the whole growing
                    // cumulative utterance, so recording the raw `text`
                    // would write the entire script-so-far into every
                    // transcript line; the leading edge is the words
                    // actually just spoken for this sentence.
                    let lead = recent_words(text, 10);
                    let update = s.tracker.observe(&SpeechUpdate {
                        text: lead.clone(),
                        words: Vec::new(),
                        is_final,
                    });
                    s.recorder.record(&update, &lead);
                    track_event(&update)
                })
            };
            if let Some(ev) = track {
                let _ = app.emit("track-update", ev);
            }
        }

        // Clean up: ask nicely (SIGTERM closes the recording), then insist.
        signal_speech_helper();
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                _ => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
        SPEECH_PID.store(0, Ordering::SeqCst);
        OTHER_SPEAKING.store(false, Ordering::SeqCst);
        AUDIO_RUNNING.store(false, Ordering::Relaxed);
        eprintln!("[prompter] Speech recognizer stopped");
    });

    Ok("started".into())
}

/// Stop speech recognition.
#[tauri::command]
fn stop_speech() -> Result<(), String> {
    AUDIO_STOP.store(true, Ordering::SeqCst);
    // Signal the helper directly: the reader loop only checks the stop flag
    // when a line arrives, which may be never if the room has gone quiet.
    signal_speech_helper();
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
        delivery: Default::default(),
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

#[derive(Debug, Serialize, serde::Deserialize)]
struct Settings {
    #[serde(default = "default_font_size")]
    font_size: u32,
    #[serde(default = "default_speed")]
    speed: u32,
    #[serde(default)]
    always_on_top: bool,
    #[serde(default = "default_highlight_mode")]
    highlight_mode: String,
    /// Hide the window from screenshots / screen-share / screen-recording
    /// (macOS content protection). `None` = use the conf default (protected).
    #[serde(default)]
    hide_from_screen_share: Option<bool>,
    #[serde(default)]
    recent_scripts: Vec<RecentScript>,
    /// Record the session and re-check coverage against the full recording
    /// afterward (the recording is deleted once checked).
    #[serde(default = "default_true")]
    verify_with_recording: bool,
    /// Keep the session audio after the verification pass (off by default:
    /// consult audio is patient data).
    #[serde(default)]
    keep_session_audio: bool,
    /// Watch call audio to know when the other party is speaking (needs Screen
    /// & System Audio Recording permission).
    #[serde(default)]
    detect_other_party: bool,
    /// Where the line being read sits, as a fraction of the prompter height.
    /// Near the top keeps the reader's eyes close to the camera.
    #[serde(default = "default_eye_line")]
    eye_line: f32,
    /// Width of the text column in px. Narrow keeps eye movement small, so the
    /// reader doesn't visibly scan side to side on camera.
    #[serde(default = "default_col_width")]
    col_width: u32,
}

fn default_eye_line() -> f32 {
    0.10
}

fn default_col_width() -> u32 {
    460
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    /// The same defaults a settings file with missing keys gets, so a fresh
    /// install (no file yet) behaves identically.
    fn default() -> Self {
        serde_json::from_str("{}").expect("all Settings fields have serde defaults")
    }
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
        delivery: Default::default(),
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

#[derive(Debug, Clone, Serialize)]
struct CoachingInsight {
    severity: String,
    message: String,
    advice: String,
}

/// Find a script file by consultation_id.
///
/// Searches the watched scripts folder AND `~/Downloads`. The id-based deep
/// link (SynapseRx "Open in Prompter") carries only the consultation id, so we
/// resolve it to a local file by matching the filename or the frontmatter
/// `consultation_id`. The SynapseRx "Download" export lands the `.script.md` in
/// `~/Downloads` (browser default), so without the Downloads fallback a
/// freshly-downloaded consultation is invisible to the deep link and the app
/// reports "No script found". Picks the most recently modified match so a
/// re-export / newer download wins.
fn find_script_by_consultation_id(consultation_id: &str) -> Option<String> {
    let home = dirs_next::home_dir()?;
    let dirs = [
        home.join("meetings").join("scripts"),
        home.join("Downloads"),
    ];

    let mut best: Option<(std::time::SystemTime, String)> = None;
    for dir in &dirs {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let mut matched = false;
            // Check filename contains the consultation_id.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.contains(consultation_id) {
                    matched = true;
                }
            }
            // Otherwise check the frontmatter `consultation_id` field (the
            // download filename is a case reference, not the raw id, so this is
            // the reliable match for a downloaded export).
            if !matched {
                if let Ok(content) = fs::read_to_string(&path) {
                    if content.contains(&format!("consultation_id: \"{consultation_id}\""))
                        || content.contains(&format!("consultation_id: {consultation_id}"))
                    {
                        matched = true;
                    }
                }
            }
            if matched {
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
                    best = Some((mtime, path.to_string_lossy().to_string()));
                }
            }
        }
    }
    best.map(|(_, path)| path)
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

/// Label for the screen-share toggle, with a check when protection is on.
/// The tray's screen-share menu item, kept so the in-app toggle can update its
/// label too.
#[derive(Default)]
struct ScreenShareItem(Mutex<Option<tauri::menu::MenuItem<tauri::Wry>>>);

/// Hide (or show) the prompter in screenshots, screen shares and recordings,
/// persist the choice, and keep the tray item and the UI in sync.
fn apply_screen_share(app: &tauri::AppHandle, hidden: bool) {
    for (_, win) in app.webview_windows() {
        let _ = win.set_content_protected(hidden);
    }
    if let Some(item) = app.state::<ScreenShareItem>().0.lock().ok().and_then(|g| g.clone()) {
        let _ = item.set_text(screen_share_label(hidden));
    }
    let mut s = load_settings();
    s.hide_from_screen_share = Some(hidden);
    let _ = save_settings(s);
    let _ = app.emit("screen-share-changed", hidden);
}

#[tauri::command]
fn set_hide_from_screen_share(app: tauri::AppHandle, hidden: bool) {
    apply_screen_share(&app, hidden);
}

fn screen_share_label(hidden: bool) -> &'static str {
    if hidden {
        "Hide from Screen Share ✓"
    } else {
        "Hide from Screen Share"
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(TrackingState::default())
        .manage(ScreenShareItem::default())
        .setup(|app| {
            use tauri::Listener;

            // Menu-bar tray with the screen-share visibility toggle. The window
            // is content-protected (hidden from screenshots / screen-share /
            // recording) by default; this lets the operator reveal it on demand
            // and persists the choice. Mirrors the minutes "Hide from Screen
            // Share" tray item.
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;

                // Saved choice; None means "use the conf default" (protected).
                let hidden = load_settings().hide_from_screen_share.unwrap_or(true);
                // Re-apply the saved choice to the live window (conf seeds it
                // true, so this only matters when the operator chose to reveal).
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.set_content_protected(hidden);
                }

                let screen_item = MenuItem::with_id(
                    app,
                    "screen-share-toggle",
                    screen_share_label(hidden),
                    true,
                    None::<&str>,
                )?;
                let quit =
                    MenuItem::with_id(app, "tray_quit", "Quit Prompter", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&screen_item, &quit])?;

                if let Ok(mut slot) = app.state::<ScreenShareItem>().0.lock() {
                    *slot = Some(screen_item.clone());
                }

                let mut tray = TrayIconBuilder::with_id("prompter-tray")
                    .menu(&menu)
                    .on_menu_event(move |app, event| match event.id().as_ref() {
                        "screen-share-toggle" => {
                            let hidden = load_settings().hide_from_screen_share.unwrap_or(true);
                            apply_screen_share(app, !hidden);
                        }
                        "tray_quit" => app.exit(0),
                        _ => {}
                    });
                if let Some(icon) = app.default_window_icon() {
                    tray = tray.icon(icon.clone());
                }
                let _ = tray.build(app);
            }
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
            set_tracking_position,
            choose_branch,
            start_speech,
            stop_speech,
            save_compliance,
            get_coaching,
            load_settings,
            save_settings,
            add_recent_script,
            set_always_on_top,
            set_hide_from_screen_share,
            list_available_scripts
        ])
        .run(tauri::generate_context!())
        .expect("error while running Prompter");
}
