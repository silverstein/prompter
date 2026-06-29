//! Compliance checklist evaluation (Phase 5 completion).
//!
//! The contact-center copilot pattern (Balto AI-Checklist / Sedric
//! disclosure-detection): a list of required talking points that auto-check as
//! they are covered, producing an exam-ready record. Two evaluators sit behind
//! one trait:
//!
//! - [`KeywordChecklistEvaluator`] -- a deterministic cue matcher. The
//!   always-available floor, fully tested, no network.
//! - [`LlmChecklistEvaluator`] -- semantic matching for paraphrase, with the LLM
//!   call injected via [`LlmClient`] so `prompter-core` stays network-free and
//!   testable (the real HTTP client lives in the app). Falls back to the keyword
//!   floor if the model call fails.
//!
//! Standard MTM required-element sets are provided ([`obra_counseling_checklist`],
//! [`cmr_checklist`]). See `docs/UPGRADE-2026.md`, D6.

use crate::session::TranscriptLine;

/// Whether a required talking point was covered.
#[derive(Debug, Clone, PartialEq)]
pub enum ChecklistStatus {
    Covered,
    Missed,
}

/// A required talking point.
#[derive(Debug, Clone, PartialEq)]
pub struct ChecklistItem {
    pub id: String,
    pub label: String,
    /// Recognition cues. The keyword evaluator marks the item covered when any
    /// cue appears in a recognized line; the LLM evaluator uses them as hints.
    pub cues: Vec<String>,
}

impl ChecklistItem {
    pub fn new(id: &str, label: &str, cues: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            cues: cues.iter().map(|c| c.to_string()).collect(),
        }
    }
}

/// The outcome for one checklist item.
#[derive(Debug, Clone, PartialEq)]
pub struct ChecklistResult {
    pub id: String,
    pub label: String,
    pub status: ChecklistStatus,
    /// The recognized line (or note) that satisfied the item, if covered.
    pub evidence: Option<String>,
}

/// Evaluates required talking points against a session transcript.
pub trait ChecklistEvaluator {
    fn evaluate(
        &self,
        items: &[ChecklistItem],
        transcript: &[TranscriptLine],
    ) -> Vec<ChecklistResult>;
}

fn normalize(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deterministic floor: an item is covered when any of its (normalized) cues is
/// a substring of any (normalized) recognized transcript line.
pub struct KeywordChecklistEvaluator;

impl ChecklistEvaluator for KeywordChecklistEvaluator {
    fn evaluate(
        &self,
        items: &[ChecklistItem],
        transcript: &[TranscriptLine],
    ) -> Vec<ChecklistResult> {
        let normalized: Vec<(String, &str)> = transcript
            .iter()
            .map(|line| (normalize(&line.recognized), line.recognized.as_str()))
            .collect();

        items
            .iter()
            .map(|item| {
                let mut hit: Option<String> = None;
                'cues: for cue in &item.cues {
                    let needle = normalize(cue);
                    if needle.is_empty() {
                        continue;
                    }
                    for (norm_line, raw) in &normalized {
                        if norm_line.contains(&needle) {
                            hit = Some((*raw).to_string());
                            break 'cues;
                        }
                    }
                }
                ChecklistResult {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    status: if hit.is_some() {
                        ChecklistStatus::Covered
                    } else {
                        ChecklistStatus::Missed
                    },
                    evidence: hit,
                }
            })
            .collect()
    }
}

/// A minimal LLM completion seam. The real implementation (an HTTP call to a
/// model API) lives in the app; core depends only on this trait so it stays
/// network-free and testable.
pub trait LlmClient {
    /// Return the model's completion for `prompt`, or an error string.
    fn complete(&self, prompt: &str) -> Result<String, String>;
}

/// Semantic checklist evaluator: asks an [`LlmClient`] which required items the
/// transcript covers, tolerating paraphrase. Falls back to the keyword floor on
/// any model/parse failure so a required-element report is always produced.
pub struct LlmChecklistEvaluator<'a, C: LlmClient> {
    client: &'a C,
}

impl<'a, C: LlmClient> LlmChecklistEvaluator<'a, C> {
    pub fn new(client: &'a C) -> Self {
        Self { client }
    }

    /// Build a deterministic, parseable prompt. The model is asked to emit one
    /// `id: covered|missed` line per item so the response is trivial to parse
    /// and hard to hallucinate around.
    pub fn build_prompt(items: &[ChecklistItem], transcript: &[TranscriptLine]) -> String {
        let mut p = String::from(
            "You are a pharmacy compliance reviewer. Given a consultation transcript and a list \
             of required talking points, decide for EACH point whether the pharmacist covered it \
             (even if paraphrased). Reply with exactly one line per point in the form \
             `<id>: covered` or `<id>: missed`. No other text.\n\nRequired points:\n",
        );
        for item in items {
            p.push_str(&format!("- {} ({})\n", item.id, item.label));
        }
        p.push_str("\nTranscript:\n");
        for line in transcript {
            p.push_str(&format!("- {}\n", line.recognized));
        }
        p
    }

    /// Parse `<id>: covered|missed` lines; any id not present in the response is
    /// left for the caller to fill from the fallback.
    fn parse(response: &str, items: &[ChecklistItem]) -> Option<Vec<ChecklistResult>> {
        let mut covered = std::collections::HashMap::new();
        for line in response.lines() {
            let Some((id, verdict)) = line.split_once(':') else {
                continue;
            };
            let id = id.trim().trim_start_matches('-').trim();
            let verdict = normalize(verdict);
            if verdict.contains("covered") {
                covered.insert(id.to_string(), true);
            } else if verdict.contains("missed") {
                covered.insert(id.to_string(), false);
            }
        }
        if covered.is_empty() {
            return None; // unparseable -> let caller fall back
        }
        Some(
            items
                .iter()
                .map(|item| {
                    let is_covered = covered.get(&item.id).copied().unwrap_or(false);
                    ChecklistResult {
                        id: item.id.clone(),
                        label: item.label.clone(),
                        status: if is_covered {
                            ChecklistStatus::Covered
                        } else {
                            ChecklistStatus::Missed
                        },
                        evidence: None,
                    }
                })
                .collect(),
        )
    }
}

impl<C: LlmClient> ChecklistEvaluator for LlmChecklistEvaluator<'_, C> {
    fn evaluate(
        &self,
        items: &[ChecklistItem],
        transcript: &[TranscriptLine],
    ) -> Vec<ChecklistResult> {
        let prompt = Self::build_prompt(items, transcript);
        match self.client.complete(&prompt) {
            Ok(response) => Self::parse(&response, items)
                .unwrap_or_else(|| KeywordChecklistEvaluator.evaluate(items, transcript)),
            // Fail closed to the deterministic floor: still produce a report.
            Err(_) => KeywordChecklistEvaluator.evaluate(items, transcript),
        }
    }
}

/// OBRA-90 patient-counseling elements (verbal offer to counsel + the matters a
/// pharmacist must address). Cues are starting points; tune per program.
/// Reference: OBRA-90 counseling/DUR requirements.
pub fn obra_counseling_checklist() -> Vec<ChecklistItem> {
    vec![
        ChecklistItem::new(
            "name_use",
            "Medication name and intended use",
            &["used to", "this medication", "treats", "for your"],
        ),
        ChecklistItem::new(
            "dose_route",
            "Dose, route, and how to take",
            &[
                "take",
                "how to take",
                "twice a day",
                "with food",
                "by mouth",
            ],
        ),
        ChecklistItem::new(
            "side_effects",
            "Common side effects and what to watch for",
            &["side effect", "watch for", "may cause", "if you notice"],
        ),
        ChecklistItem::new(
            "interactions",
            "Drug/food interactions and contraindications",
            &["interaction", "avoid", "do not take with", "grapefruit"],
        ),
        ChecklistItem::new(
            "storage_missed",
            "Storage and what to do for a missed dose",
            &["store", "missed dose", "if you forget", "room temperature"],
        ),
        ChecklistItem::new(
            "questions",
            "Offer to answer questions",
            &["any questions", "anything else", "feel free to ask"],
        ),
    ]
}

/// CMS Part D Comprehensive Medication Review (CMR) elements that map to spoken
/// talking points. Reference: NBMTM CMS CMR/TMR requirements.
pub fn cmr_checklist() -> Vec<ChecklistItem> {
    vec![
        ChecklistItem::new(
            "med_list_review",
            "Review the personal medication list",
            &[
                "medications you",
                "currently taking",
                "your medication list",
            ],
        ),
        ChecklistItem::new(
            "action_plan",
            "Discuss the medication action plan",
            &[
                "action plan",
                "what i need you to do",
                "next steps",
                "i recommend",
            ],
        ),
        ChecklistItem::new(
            "adherence",
            "Address adherence / how it is going",
            &[
                "taking it as prescribed",
                "remembering to take",
                "how is it going",
            ],
        ),
        ChecklistItem::new(
            "follow_up",
            "Set a follow-up plan",
            &["follow up", "check back", "next visit", "call you"],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> TranscriptLine {
        TranscriptLine {
            sentence_index: 0,
            recognized: text.to_string(),
            branch_option: None,
        }
    }

    #[test]
    fn keyword_evaluator_covers_and_misses() {
        let items = vec![
            ChecklistItem::new("se", "Side effects", &["side effect"]),
            ChecklistItem::new("fu", "Follow up", &["follow up"]),
        ];
        let transcript = vec![
            line("a common side effect is mild nausea"),
            line("you are taking warfarin and metformin"),
        ];
        let results = KeywordChecklistEvaluator.evaluate(&items, &transcript);
        assert_eq!(results[0].status, ChecklistStatus::Covered);
        assert!(results[0].evidence.is_some());
        assert_eq!(results[1].status, ChecklistStatus::Missed);
        assert!(results[1].evidence.is_none());
    }

    #[test]
    fn keyword_evaluator_normalizes_punctuation_and_case() {
        let items = vec![ChecklistItem::new("q", "Questions", &["any questions"])];
        let transcript = vec![line("Do you have ANY Questions?!")];
        let results = KeywordChecklistEvaluator.evaluate(&items, &transcript);
        assert_eq!(results[0].status, ChecklistStatus::Covered);
    }

    struct MockLlm(String);
    impl LlmClient for MockLlm {
        fn complete(&self, _prompt: &str) -> Result<String, String> {
            Ok(self.0.clone())
        }
    }

    struct FailingLlm;
    impl LlmClient for FailingLlm {
        fn complete(&self, _prompt: &str) -> Result<String, String> {
            Err("network down".into())
        }
    }

    #[test]
    fn llm_evaluator_parses_structured_response() {
        let items = vec![
            ChecklistItem::new("se", "Side effects", &["side effect"]),
            ChecklistItem::new("fu", "Follow up", &["follow up"]),
        ];
        // Model recognizes a paraphrase the keyword floor would miss.
        let client = MockLlm("se: covered\nfu: missed\n".into());
        let eval = LlmChecklistEvaluator::new(&client);
        let results = eval.evaluate(&items, &[line("we should keep an eye on stomach upset")]);
        assert_eq!(results[0].status, ChecklistStatus::Covered);
        assert_eq!(results[1].status, ChecklistStatus::Missed);
    }

    #[test]
    fn llm_evaluator_falls_back_to_keywords_on_failure() {
        let items = vec![ChecklistItem::new("se", "Side effects", &["side effect"])];
        let client = FailingLlm;
        let eval = LlmChecklistEvaluator::new(&client);
        let results = eval.evaluate(&items, &[line("a side effect to watch for is dizziness")]);
        // Fell back to the deterministic floor, which finds the cue.
        assert_eq!(results[0].status, ChecklistStatus::Covered);
    }

    #[test]
    fn llm_evaluator_falls_back_on_unparseable_response() {
        let items = vec![ChecklistItem::new("se", "Side effects", &["side effect"])];
        let client = MockLlm("I think the pharmacist did a great job overall!".into());
        let eval = LlmChecklistEvaluator::new(&client);
        let results = eval.evaluate(&items, &[line("the main side effect is drowsiness")]);
        assert_eq!(results[0].status, ChecklistStatus::Covered);
    }

    #[test]
    fn standard_checklists_are_nonempty() {
        assert!(!obra_counseling_checklist().is_empty());
        assert!(!cmr_checklist().is_empty());
    }
}
