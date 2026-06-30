//! Offline replay of a recorded ASR stream through the `ScriptTracker`.
//!
//! The app records the raw recognizer stream (script + every `{text, final}`
//! event) to `~/.prompter/recording.jsonl` during a session. Replaying it here
//! drives the exact same tracking logic without a live read, so the matcher can
//! be tuned by watching where the cursor freezes / jumps / lags -- with the
//! human out of the loop.
//!
//! Usage:
//!   cargo run -p prompter-core --example replay -- [recording.jsonl] [window] [tail]
//!
//! Defaults: ~/.prompter/recording.jsonl, window 10, tail 10. Sweep `window`
//! against the SAME recording to compare settings.

use prompter_core::{recent_words, script, ScriptTracker, SessionRecorder, SpeechUpdate};
use std::io::BufRead;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/.prompter/recording.jsonl")
    });
    let window: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let tail: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);

    let file = std::fs::File::open(&path).unwrap_or_else(|e| {
        eprintln!("cannot open {path}: {e}");
        std::process::exit(1);
    });
    let reader = std::io::BufReader::new(file);

    let mut tracker: Option<ScriptTracker> = None;
    let mut recorder: Option<SessionRecorder> = None;
    let mut total = 0usize;
    let mut last = 0usize;
    let mut events = 0usize;
    let mut max_idx = 0usize;
    let mut freeze_run = 0usize;
    let mut max_freeze = 0usize;
    let mut max_jump = 0i64;

    for line in reader.lines() {
        let line = line.unwrap_or_default();
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("script") => {
                let src = v.get("source").and_then(|s| s.as_str()).unwrap_or("");
                let parsed = match script::parse(src) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("script parse error: {e}");
                        std::process::exit(1);
                    }
                };
                let mut t = ScriptTracker::new(&parsed);
                t.set_window_radius(window);
                total = t.sentence_count();
                recorder = Some(SessionRecorder::new(&parsed));
                println!(
                    "# {total} main sentences | window={window} tail={tail}\n# {:>8} {} {:>4} {:>5}  {:<9} tail",
                    "t(ms)", "F", "idx", "d", "state"
                );
                tracker = Some(t);
            }
            Some("asr") => {
                let text = v.get("text").and_then(|s| s.as_str()).unwrap_or("");
                let is_final = v.get("final").and_then(|b| b.as_bool()).unwrap_or(false);
                let t = v.get("t").and_then(|x| x.as_u64()).unwrap_or(0);
                let Some(tr) = tracker.as_mut() else {
                    continue;
                };
                let lead = recent_words(text, tail);
                let u = tr.observe(&SpeechUpdate {
                    text: lead.clone(),
                    words: Vec::new(),
                    is_final,
                });
                if let Some(rec) = recorder.as_mut() {
                    rec.record(&u, &lead);
                }
                let delta = u.sentence_index as i64 - last as i64;
                if u.sentence_index == last {
                    freeze_run += 1;
                    max_freeze = max_freeze.max(freeze_run);
                } else {
                    freeze_run = 0;
                }
                max_jump = max_jump.max(delta);
                last = u.sentence_index;
                max_idx = max_idx.max(u.sentence_index);
                events += 1;
                let state = format!("{:?}", u.state);
                let state = state
                    .split(|c| c == ' ' || c == '{')
                    .next()
                    .unwrap_or(&state);
                println!(
                    "{t:>10} {} {:>4} {:>+5}  {state:<9} {lead}",
                    if is_final { "F" } else { "." },
                    u.sentence_index,
                    delta
                );
            }
            _ => {}
        }
    }

    let pct = if total > 1 {
        100.0 * max_idx as f64 / (total - 1) as f64
    } else {
        0.0
    };
    println!(
        "\n# {events} events | reached idx {max_idx}/{} ({pct:.0}%) | longest freeze {max_freeze} events | max single jump +{max_jump}",
        total.saturating_sub(1)
    );

    // Speech-verified compliance, built from the SAME stream the app records.
    // Proves the recorder produces a real report from partials-only input.
    if let Some(rec) = recorder.as_ref() {
        let report = rec.build_report(0);
        let words_pct = if report.total_words > 0 {
            100.0 * report.words_delivered as f64 / report.total_words as f64
        } else {
            0.0
        };
        println!(
            "# recorder: {}/{} words delivered ({words_pct:.0}%) | pauses {}/{} | transcript {} lines | branches {}",
            report.words_delivered,
            report.total_words,
            report.pause_points_reached,
            report.pause_points_total,
            rec.transcript().len(),
            report.branches_taken.len(),
        );
    }
}
