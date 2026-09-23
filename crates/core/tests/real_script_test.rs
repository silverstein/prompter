use prompter_core::script::{self, Directive, Element};

const MTM_SCRIPT: &str = include_str!("fixtures/mtm-consultation.script.md");

#[test]
fn parse_real_mtm_consultation() {
    let script = script::parse(MTM_SCRIPT).expect("Failed to parse real MTM script");

    // Correct section count
    assert_eq!(script.sections.len(), 4, "Expected 4 sections");
    assert_eq!(script.sections[0].name, "Intro");
    assert_eq!(script.sections[1].name, "Explanation of Findings");
    assert_eq!(script.sections[2].name, "Recommendations");
    assert_eq!(script.sections[3].name, "Closing");

    // Frontmatter parsed correctly
    assert_eq!(script.frontmatter.title, "MTM Consultation — Jane Smith");
    assert_eq!(script.frontmatter.version.as_deref(), Some("2.1"));
    assert_eq!(
        script.frontmatter.estimated_duration.as_deref(),
        Some("18min")
    );
}

#[test]
fn variable_substitution_in_real_script() {
    let script = script::parse(MTM_SCRIPT).unwrap();

    // Check patient_name is NOT in the script (it wasn't used as {{patient_name}} in body)
    // Check medications substitution
    let findings = &script.sections[1];
    let all_text: String = findings
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Text(sentences) => {
                Some(sentences.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        all_text.contains("Warfarin, Metformin"),
        "medications variable not substituted"
    );
    assert!(
        all_text.contains("Garlic extract, Ginkgo biloba, St. John's Wort"),
        "supplements variable not substituted"
    );
    assert!(
        all_text.contains("dizziness, unusual bruising"),
        "symptoms variable not substituted"
    );
}

#[test]
fn pause_points_in_real_script() {
    let script = script::parse(MTM_SCRIPT).unwrap();

    // Count total pause directives across all sections
    let pause_count: usize = script
        .sections
        .iter()
        .flat_map(|s| &s.elements)
        .filter(|e| matches!(e, Element::Directive(Directive::Pause { .. })))
        .count();

    assert!(
        pause_count >= 6,
        "Expected at least 6 pause points, found {}",
        pause_count
    );
}

#[test]
fn branch_in_recommendations() {
    let script = script::parse(MTM_SCRIPT).unwrap();
    let recs = &script.sections[2];

    let branch = recs.elements.iter().find_map(|e| match e {
        Element::Directive(Directive::Branch { question, options }) => {
            Some((question.as_str(), options))
        }
        _ => None,
    });

    let (question, options) = branch.expect("No branch directive found in Recommendations");
    assert!(question.contains("get this plan organized"));
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].label, "YES");
    assert_eq!(options[1].label, "NO");

    // YES branch should be substantially longer than NO
    let yes_words: usize = options[0].sentences.iter().map(|s| s.word_count).sum();
    let no_words: usize = options[1].sentences.iter().map(|s| s.word_count).sum();
    assert!(
        yes_words > no_words * 2,
        "YES branch ({} words) should be much longer than NO ({} words)",
        yes_words,
        no_words
    );
}

#[test]
fn word_count_realistic() {
    let script = script::parse(MTM_SCRIPT).unwrap();

    // The real MTM script is ~2000-3000 words
    assert!(
        script.word_count > 500,
        "Word count {} seems too low for a real consultation script",
        script.word_count
    );
    assert!(
        script.word_count < 10000,
        "Word count {} seems unrealistically high",
        script.word_count
    );

    println!("Total word count: {}", script.word_count);
    for section in &script.sections {
        println!("  {} — {} words", section.name, section.word_count);
    }
}

/// Simulate Apple's recognizer on a full read of the real MTM script: one
/// cumulative partial per word, never a final. The cursor must reach the last
/// main sentence, never jump more than one sentence at a time, and never go back.
#[test]
fn simulated_full_read_tracks_to_the_end_without_jumps() {
    use prompter_core::{recent_words, ScriptTracker, SpeechUpdate, TimelineStep};
    let script = script::parse(MTM_SCRIPT).unwrap();
    let mut tracker = ScriptTracker::new(&script);
    tracker.set_window_radius(10);
    let main: Vec<String> = tracker
        .timeline()
        .iter()
        .filter_map(|s| match s {
            TimelineStep::Sentence { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let mut spoken = String::new();
    let mut last = 0usize;
    for sentence in &main {
        for word in sentence.split_whitespace() {
            spoken.push(' ');
            spoken.push_str(word);
            let u = tracker.observe(&SpeechUpdate::partial(recent_words(&spoken, 10)));
            assert!(u.sentence_index >= last, "went back {} -> {}", last, u.sentence_index);
            assert!(u.sentence_index <= last + 1, "jumped {} -> {} on {:?}", last, u.sentence_index, recent_words(&spoken, 10));
            last = u.sentence_index;
        }
    }
    assert!(last + 1 >= main.len(), "reached {last} of {}", main.len());
}

/// A real skip of many sentences mid-read is followed (via the relocator or
/// the capped local catch-up), and lands on the sentence being read.
#[test]
fn simulated_skip_is_followed() {
    use prompter_core::{recent_words, ScriptTracker, SpeechUpdate, TimelineStep};
    let script = script::parse(MTM_SCRIPT).unwrap();
    let mut tracker = ScriptTracker::new(&script);
    tracker.set_window_radius(10);
    let main: Vec<String> = tracker
        .timeline()
        .iter()
        .filter_map(|s| match s {
            TimelineStep::Sentence { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(main.len() > 30, "fixture should be long");
    let skip_to = main.len() - 8;
    let mut spoken = String::new();
    for i in (0..3).chain(skip_to..skip_to + 3) {
        for word in main[i].split_whitespace() {
            spoken.push(' ');
            spoken.push_str(word);
            tracker.observe(&SpeechUpdate::partial(recent_words(&spoken, 10)));
        }
    }
    let at = tracker.preview_position();
    assert!(
        (skip_to..=skip_to + 2).contains(&at),
        "expected to follow the skip to ~{skip_to}, cursor at {at}"
    );
}

/// Replay of a real live read (Sept 23, Apple partials only, no finals) of a
/// short test script. At ~71 s the reader goes back two sentences, from "He
/// relit the lamp..." (8) to "On the fourth night..." (6), and re-reads. The
/// cursor must follow back, then carry on to the end.
#[test]
fn real_read_going_back_is_followed() {
    use prompter_core::{recent_words, ScriptTracker, SpeechUpdate};
    let rec = include_str!("fixtures/lighthouse-reread.recording.jsonl");
    let mut tracker: Option<ScriptTracker> = None;
    let mut went_back_to = None;
    let mut last = 0usize;
    for line in rec.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        match v["type"].as_str() {
            Some("script") => {
                let parsed = script::parse(v["source"].as_str().unwrap()).unwrap();
                let mut t = ScriptTracker::new(&parsed);
                t.set_window_radius(10);
                tracker = Some(t);
            }
            Some("asr") => {
                let t = tracker.as_mut().unwrap();
                let ms = v["t"].as_u64().unwrap();
                let u = t.observe(&SpeechUpdate {
                    text: recent_words(v["text"].as_str().unwrap(), 10),
                    words: Vec::new(),
                    is_final: v["final"].as_bool().unwrap_or(false),
                });
                if (71_500..82_000).contains(&ms) && u.relocated {
                    went_back_to = Some(u.sentence_index);
                }
                if !u.relocated {
                    assert!(u.sentence_index >= last, "went back {} -> {} at {ms}ms without relocating", last, u.sentence_index);
                }
                last = u.sentence_index;
            }
            _ => {}
        }
    }
    let back = went_back_to.expect("the re-read at ~72s should move the cursor back");
    assert!((6..=7).contains(&back), "went back to {back}");
    assert_eq!(last, 15, "should finish on the last sentence");
}

/// Main sentences and the NO/YES option sentences of the real MTM script, plus
/// a tracker positioned for a word-by-word partials read.
fn mtm_branch_setup() -> (prompter_core::ScriptTracker, Vec<String>, Vec<String>, Vec<String>) {
    use prompter_core::{ScriptTracker, TimelineStep};
    let script = script::parse(MTM_SCRIPT).unwrap();
    let mut tracker = ScriptTracker::new(&script);
    tracker.set_window_radius(10);
    let (mut main, mut yes, mut no) = (Vec::new(), Vec::new(), Vec::new());
    for step in tracker.timeline() {
        match step {
            TimelineStep::Sentence { text, .. } => main.push(text.clone()),
            TimelineStep::BranchSentence { option_label, text } if option_label == "YES" => yes.push(text.clone()),
            TimelineStep::BranchSentence { text, .. } => no.push(text.clone()),
            _ => {}
        }
    }
    (tracker, main, yes, no)
}

/// Read `sentences` word by word as Apple-style partials, collecting updates.
fn read_partials(
    tracker: &mut prompter_core::ScriptTracker,
    spoken: &mut String,
    sentences: &[String],
) -> Vec<prompter_core::TrackUpdate> {
    use prompter_core::{recent_words, SpeechUpdate};
    let mut out = Vec::new();
    for s in sentences {
        for word in s.split_whitespace() {
            spoken.push(' ');
            spoken.push_str(word);
            out.push(tracker.observe(&SpeechUpdate::partial(recent_words(spoken, 10))));
        }
    }
    out
}

/// Reading the NO answer, which opens with a sentence the YES answer also
/// contains: nothing is selected on the shared sentence, NO is selected once
/// its own words are heard, and reading the Closing returns to the main line.
#[test]
fn branch_option_is_picked_from_reading_even_with_shared_text() {
    use prompter_core::TrackState;
    let (mut t, main, _yes, no) = mtm_branch_setup();
    let trigger = main.iter().position(|s| s.contains("Prevnar")).unwrap();
    let mut spoken = String::new();
    read_partials(&mut t, &mut spoken, &main[trigger - 2..=trigger]);
    let shared = read_partials(&mut t, &mut spoken, &no[..1]);
    assert!(
        shared.iter().all(|u| u.branch_choice.is_none()),
        "must not pick an option on text both options share"
    );
    let own = read_partials(&mut t, &mut spoken, &no[1..]);
    let picked = own.iter().find_map(|u| u.branch_choice.clone()).expect("NO should be picked");
    assert_eq!(picked.option_label, "NO");
    assert!(matches!(own.last().unwrap().state, TrackState::InBranch { .. }));
    let back = read_partials(&mut t, &mut spoken, &main[trigger + 1..trigger + 2]);
    let last = back.last().unwrap();
    assert!(matches!(last.state, TrackState::Speaking), "should be back on the main line");
    assert_eq!(last.sentence_index, trigger + 1);
}

/// Skipping the branch and reading straight on into the Closing selects no
/// option and keeps tracking the main line.
#[test]
fn skipping_a_branch_selects_nothing() {
    let (mut t, main, _, _) = mtm_branch_setup();
    let trigger = main.iter().position(|s| s.contains("Prevnar")).unwrap();
    let mut spoken = String::new();
    let ups = read_partials(&mut t, &mut spoken, &main[trigger - 2..trigger + 3]);
    assert!(ups.iter().all(|u| u.branch_choice.is_none()));
    assert!(t.preview_position() >= trigger + 2, "kept tracking the main line");
}

/// Replay of a real live read (Sept 23 retest) that goes into the YES answer
/// without saying the question: YES is picked from the answer's own words, the
/// highlight moves to the answer's second sentence, and the cursor returns to
/// the main line at "Thank you for listening".
#[test]
fn real_read_follows_into_a_branch_answer_and_back() {
    use prompter_core::{recent_words, ScriptTracker, SpeechUpdate, TimelineStep, TrackState};
    let rec = include_str!("fixtures/lighthouse-branch.recording.jsonl");
    let mut tracker: Option<ScriptTracker> = None;
    let mut picked = None;
    let mut answer_timelines = Vec::new();
    let mut seen_in_answer = Vec::new();
    let mut returned_to = None;
    for line in rec.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        match v["type"].as_str() {
            Some("script") => {
                let parsed = script::parse(v["source"].as_str().unwrap()).unwrap();
                let mut t = ScriptTracker::new(&parsed);
                t.set_window_radius(10);
                answer_timelines = t
                    .timeline()
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| matches!(s, TimelineStep::BranchSentence { option_label, .. } if option_label == "YES"))
                    .map(|(i, _)| i)
                    .collect();
                tracker = Some(t);
            }
            Some("asr") => {
                let u = tracker.as_mut().unwrap().observe(&SpeechUpdate {
                    text: recent_words(v["text"].as_str().unwrap(), 10),
                    words: Vec::new(),
                    is_final: v["final"].as_bool().unwrap_or(false),
                });
                if let Some(c) = &u.branch_choice {
                    picked = Some(c.option_label.clone());
                }
                if matches!(u.state, TrackState::InBranch { .. }) {
                    seen_in_answer.push(u.timeline_index);
                } else if picked.is_some() && returned_to.is_none() {
                    returned_to = Some(u.sentence_index);
                }
            }
            _ => {}
        }
    }
    assert_eq!(picked.as_deref(), Some("YES"));
    assert_eq!(seen_in_answer.first(), answer_timelines.first(), "starts on the answer's first sentence");
    assert!(seen_in_answer.contains(&answer_timelines[1]), "follows to the answer's second sentence");
    assert_eq!(returned_to, Some(14), "returns at 'Thank you for listening'");
}

/// Replay of a real live read (Sept 23): after "Years later, the captain of that
/// boat..." the reader skips the rest of the section (and the question) and
/// reads the YES answer. The answer is recognized from a few sentences before
/// the branch and followed to its second sentence.
#[test]
fn real_read_skipping_into_an_answer_is_followed() {
    use prompter_core::{recent_words, ScriptTracker, SpeechUpdate, TrackState};
    let rec = include_str!("fixtures/lighthouse-skip-into-answer.recording.jsonl");
    let mut tracker: Option<ScriptTracker> = None;
    let mut picked = None;
    let mut last_in_answer = None;
    for line in rec.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        match v["type"].as_str() {
            Some("script") => {
                let parsed = script::parse(v["source"].as_str().unwrap()).unwrap();
                let mut t = ScriptTracker::new(&parsed);
                t.set_window_radius(10);
                tracker = Some(t);
            }
            Some("asr") => {
                let u = tracker.as_mut().unwrap().observe(&SpeechUpdate {
                    text: recent_words(v["text"].as_str().unwrap(), 10),
                    words: Vec::new(),
                    is_final: v["final"].as_bool().unwrap_or(false),
                });
                if let Some(c) = &u.branch_choice {
                    picked = Some((c.option_label.clone(), v["t"].as_u64().unwrap()));
                }
                if matches!(u.state, TrackState::InBranch { .. }) {
                    last_in_answer = Some(u.timeline_index);
                }
            }
            _ => {}
        }
    }
    let (label, at) = picked.expect("the YES answer should be recognized");
    assert_eq!(label, "YES");
    assert!(at < 103_000, "picked late, at {at}ms (answer starts ~100.5s)");
    // Timeline of the YES answer's second sentence in this script.
    assert_eq!(last_in_answer, Some(20), "followed to the answer's second sentence");
}

/// The post-session check on a 99 s recording of the lighthouse script (spoken
/// by macOS text-to-speech, with 4-5 s pauses between paragraphs). Before the
/// helper split recordings at silences, Apple's per-chunk final result kept
/// only the text after each chunk's last pause: 36 words, 2 sentences. With
/// the split, every sentence that was spoken counts as delivered, and the
/// three the recording left out are exactly the ones reported missing.
#[test]
fn file_transcript_of_a_full_read_covers_every_sentence() {
    use prompter_core::{realign, ScriptTracker, TimelineStep};
    let rec = include_str!("fixtures/lighthouse-reread.recording.jsonl");
    let header: serde_json::Value = serde_json::from_str(rec.lines().next().unwrap()).unwrap();
    let parsed = script::parse(header["source"].as_str().unwrap()).unwrap();
    let main: Vec<String> = ScriptTracker::new(&parsed)
        .timeline()
        .iter()
        .filter_map(|s| match s {
            TimelineStep::Sentence { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    let full = include_str!("fixtures/lighthouse-tts.file-transcript.txt");
    let r = realign(&main, full);
    let missed: Vec<String> = main
        .iter()
        .zip(&r.covered)
        .filter(|(_, c)| !**c)
        .map(|(s, _)| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(missed, ["The keeper kept", "When he retired,", "If the highlight"]);

    // What the chunked check produced from the same audio.
    let broken = "On the fourth night the main lamp went dark he climbed the spiral stairs with a lantern in one hand and a box of spare wicks in the other Thank you for listening to this short test";
    let r = realign(&main, broken);
    assert!(r.covered.iter().filter(|c| **c).count() <= 3);
}
