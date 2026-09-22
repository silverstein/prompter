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
