use crate::compliance::ComplianceReport;

// ──────────────────────────────────────────────────────────────
// Post-session coaching — data-driven delivery feedback.
//
// No LLM required. Computes actionable insights purely from
// compliance data: pacing analysis, skipped content, pause
// discipline, and section balance.
//
// Future: feed this + transcript to an LLM for deeper analysis
// ("You rushed the safety disclosure", "Patient seemed confused
// at 8:30 — consider rephrasing").
// ──────────────────────────────────────────────────────────────

/// A coaching insight with severity and actionable advice.
#[derive(Debug, Clone)]
pub struct Insight {
    pub category: InsightCategory,
    pub severity: Severity,
    pub message: String,
    pub advice: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InsightCategory {
    Pacing,
    Coverage,
    PausePoints,
    Balance,
    Overall,
    Delivery,
    Interaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Praise,   // Something done well
    Info,     // Neutral observation
    Warning,  // Could improve
    Critical, // Needs attention
}

/// Generate coaching insights from a compliance report.
pub fn analyze(report: &ComplianceReport) -> Vec<Insight> {
    let mut insights = Vec::new();

    analyze_coverage(report, &mut insights);
    analyze_pacing(report, &mut insights);
    analyze_pauses(report, &mut insights);
    analyze_balance(report, &mut insights);
    analyze_speaking_pace(report, &mut insights);
    analyze_miscues(report, &mut insights);
    analyze_interaction(report, &mut insights);
    analyze_overall(report, &mut insights);

    // Sort: critical first, praise last
    insights.sort_by(|a, b| b.severity.cmp(&a.severity));
    insights
}

/// Generate a human-readable coaching summary as markdown.
pub fn coaching_markdown(report: &ComplianceReport) -> String {
    let insights = analyze(report);
    let mut md = String::new();

    md.push_str("# Delivery Coaching\n\n");

    let critical: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Critical).collect();
    let warnings: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Warning).collect();
    let praise: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Praise).collect();
    let info: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Info).collect();

    if !praise.is_empty() {
        md.push_str("## What went well\n\n");
        for i in &praise {
            md.push_str(&format!("- {} {}\n", i.message, i.advice));
        }
        md.push('\n');
    }

    if !critical.is_empty() {
        md.push_str("## Needs attention\n\n");
        for i in &critical {
            md.push_str(&format!("- **{}** — {}\n", i.message, i.advice));
        }
        md.push('\n');
    }

    if !warnings.is_empty() {
        md.push_str("## Could improve\n\n");
        for i in &warnings {
            md.push_str(&format!("- {} — {}\n", i.message, i.advice));
        }
        md.push('\n');
    }

    if !info.is_empty() {
        md.push_str("## Notes\n\n");
        for i in &info {
            md.push_str(&format!("- {}\n", i.message));
        }
        md.push('\n');
    }

    md
}

fn analyze_coverage(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    let total = report.sections_covered.len() + report.sections_skipped.len();
    if total == 0 { return; }

    if report.sections_skipped.is_empty() {
        insights.push(Insight {
            category: InsightCategory::Coverage,
            severity: Severity::Praise,
            message: format!("All {} sections covered.", total),
            advice: "Complete script delivery — every required point was addressed.".into(),
        });
    } else {
        for name in &report.sections_skipped {
            insights.push(Insight {
                category: InsightCategory::Coverage,
                severity: Severity::Critical,
                message: format!("Section \"{}\" was skipped.", name),
                advice: "Review whether this section contains required disclosures that must be covered.".into(),
            });
        }
    }

    let adherence = report.adherence_pct();
    if adherence >= 90.0 {
        insights.push(Insight {
            category: InsightCategory::Coverage,
            severity: Severity::Praise,
            message: format!("{:.0}% script adherence.", adherence),
            advice: "Strong script adherence while maintaining natural delivery.".into(),
        });
    } else if adherence < 70.0 {
        insights.push(Insight {
            category: InsightCategory::Coverage,
            severity: Severity::Warning,
            message: format!("{:.0}% script adherence — significant portions were skipped or ad-libbed.", adherence),
            advice: "Consider practicing sections you tend to skip or rephrase.".into(),
        });
    }
}

fn analyze_pacing(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    if report.section_times.is_empty() || report.duration_secs == 0 { return; }

    let avg_secs = report.duration_secs / report.sections_covered.len().max(1) as u64;

    for name in &report.sections_covered {
        if let Some(&secs) = report.section_times.get(name) {
            // Flag sections that took less than 30% of average (rushing)
            if secs > 0 && (secs as f64) < (avg_secs as f64 * 0.3) && avg_secs > 30 {
                insights.push(Insight {
                    category: InsightCategory::Pacing,
                    severity: Severity::Warning,
                    message: format!("\"{}\" was delivered in {} — much faster than average ({}).",
                        name, fmt_duration(secs), fmt_duration(avg_secs)),
                    advice: "You may be rushing this section. Slow down to ensure the patient absorbs the information.".into(),
                });
            }
            // Flag sections that took more than 2.5x average (dragging)
            if secs > 0 && (secs as f64) > (avg_secs as f64 * 2.5) && avg_secs > 30 {
                insights.push(Insight {
                    category: InsightCategory::Pacing,
                    severity: Severity::Info,
                    message: format!("\"{}\" took {} — longer than average ({}).",
                        name, fmt_duration(secs), fmt_duration(avg_secs)),
                    advice: "This is fine if the patient had questions, but consider if you're over-explaining.".into(),
                });
            }
        }
    }
}

/// Comfortable counselling pace, in words per minute. PowerPoint Speaker Coach
/// treats 100-165 wpm as a good presenting rate; patient counselling should sit
/// in (and ideally toward the slower half of) that band.
const PACE_SLOW_WPM: f32 = 100.0;
const PACE_FAST_WPM: f32 = 165.0;

fn analyze_speaking_pace(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    let d = &report.delivery;
    if let Some(wpm) = d.speaking_wpm {
        if wpm > PACE_FAST_WPM {
            insights.push(Insight {
                category: InsightCategory::Pacing,
                severity: Severity::Warning,
                message: format!("You spoke at {:.0} words per minute.", wpm),
                advice: format!(
                    "That's above the comfortable {:.0}-{:.0} range. Slow down around drug names and instructions.",
                    PACE_SLOW_WPM, PACE_FAST_WPM
                ),
            });
        } else if wpm < PACE_SLOW_WPM {
            insights.push(Insight {
                category: InsightCategory::Pacing,
                severity: Severity::Info,
                message: format!("You spoke at {:.0} words per minute.", wpm),
                advice: "Slower than typical. Fine if deliberate; check you aren't losing the patient's attention.".into(),
            });
        } else {
            insights.push(Insight {
                category: InsightCategory::Pacing,
                severity: Severity::Praise,
                message: format!("Comfortable pace: {:.0} words per minute.", wpm),
                advice: "Right in the range patients can follow.".into(),
            });
        }
        return;
    }
    // No speaking-time measurement: overall words per minute includes listening
    // time, so it can only tell us when delivery was clearly rushed.
    if report.duration_secs >= 60 && report.words_delivered > 0 {
        let overall = report.words_delivered as f32 / (report.duration_secs as f32 / 60.0);
        if overall > PACE_FAST_WPM {
            insights.push(Insight {
                category: InsightCategory::Pacing,
                severity: Severity::Warning,
                message: format!("Overall pace was {:.0} words per minute, including pauses.", overall),
                advice: "Even counting pauses that's fast. Give the patient time to take things in.".into(),
            });
        }
    }
}

fn analyze_miscues(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    let d = &report.delivery;
    // Omissions: only worth calling out line by line when the section itself was
    // covered (whole skipped sections are already reported under Coverage).
    if !d.omitted_lines.is_empty() && !report.sections_covered.is_empty() {
        let shown: Vec<String> = d
            .omitted_lines
            .iter()
            .take(3)
            .map(|l| format!("\"{}\"", l))
            .collect();
        let more = d.omitted_lines.len().saturating_sub(3);
        insights.push(Insight {
            category: InsightCategory::Delivery,
            severity: if d.verified_by_recording { Severity::Warning } else { Severity::Info },
            message: format!(
                "{} line{} not delivered: {}{}.",
                d.omitted_lines.len(),
                if d.omitted_lines.len() == 1 { "" } else { "s" },
                shown.join(", "),
                if more > 0 { format!(" and {more} more") } else { String::new() }
            ),
            advice: if d.verified_by_recording {
                "Confirmed against the full recording. Check none of these were required.".into()
            } else {
                "From live tracking only; some may have been said but not recognized.".into()
            },
        });
    }
    if d.repeats >= 3 {
        insights.push(Insight {
            category: InsightCategory::Delivery,
            severity: Severity::Info,
            message: format!("You went back to restart a line {} times.", d.repeats),
            advice: "Those spots may be worth rehearsing, or rewording in the script.".into(),
        });
    }
    if d.off_script_episodes >= 3 {
        insights.push(Insight {
            category: InsightCategory::Delivery,
            severity: Severity::Info,
            message: format!("{} stretches of off-script speech.", d.off_script_episodes),
            advice: "Normal in a real conversation. If they cluster in one section, the script may need a better line there.".into(),
        });
    }
}

/// A comprehensive medication review must be an interactive consultation (CMS).
/// With call audio captured separately we can show the patient took part.
fn analyze_interaction(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    let Some(patient) = report.delivery.patient_talk_secs else { return };
    if report.duration_secs < 120 {
        return;
    }
    let share = patient as f32 / report.duration_secs as f32;
    if share < 0.10 {
        insights.push(Insight {
            category: InsightCategory::Interaction,
            severity: Severity::Warning,
            message: format!("The patient spoke for {} ({:.0}% of the session).", fmt_duration(patient), share * 100.0),
            advice: "CMRs must be interactive. Use the pause points to invite questions.".into(),
        });
    } else if share >= 0.20 {
        insights.push(Insight {
            category: InsightCategory::Interaction,
            severity: Severity::Praise,
            message: format!("The patient spoke for {} ({:.0}% of the session).", fmt_duration(patient), share * 100.0),
            advice: "A genuinely two-way consultation.".into(),
        });
    }
}

fn analyze_pauses(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    if report.pause_points_total == 0 { return; }

    let ratio = report.pause_points_reached as f64 / report.pause_points_total as f64;

    if ratio >= 1.0 {
        insights.push(Insight {
            category: InsightCategory::PausePoints,
            severity: Severity::Praise,
            message: format!("All {} pause points reached.", report.pause_points_total),
            advice: "You checked in with the patient at every recommended moment.".into(),
        });
    } else if ratio < 0.5 {
        let missed = report.pause_points_total - report.pause_points_reached;
        insights.push(Insight {
            category: InsightCategory::PausePoints,
            severity: Severity::Warning,
            message: format!("Missed {} of {} check-in points.", missed, report.pause_points_total),
            advice: "These pause points let the patient process information and ask questions. Try to hit them all.".into(),
        });
    }
}

fn analyze_balance(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    if report.section_times.len() < 2 { return; }

    let times: Vec<u64> = report.sections_covered.iter()
        .filter_map(|name| report.section_times.get(name).copied())
        .collect();

    if times.is_empty() { return; }

    let max = *times.iter().max().unwrap_or(&0);
    let min = *times.iter().min().unwrap_or(&0);

    if min > 0 && max > min * 5 && max > 60 {
        insights.push(Insight {
            category: InsightCategory::Balance,
            severity: Severity::Info,
            message: "Significant time variation across sections.".into(),
            advice: "Some sections took much longer than others. This is normal if the patient had questions in certain areas.".into(),
        });
    }
}

fn analyze_overall(report: &ComplianceReport, insights: &mut Vec<Insight>) {
    if report.duration_secs < 120 && report.sections_covered.len() >= 3 {
        insights.push(Insight {
            category: InsightCategory::Overall,
            severity: Severity::Warning,
            message: format!("Total session was only {} for {} sections.",
                fmt_duration(report.duration_secs), report.sections_covered.len()),
            advice: "This may be too fast for the patient to absorb. Aim for at least 10-15 minutes for a full MTM consultation.".into(),
        });
    }

    if report.duration_secs > 1800 {
        insights.push(Insight {
            category: InsightCategory::Overall,
            severity: Severity::Info,
            message: format!("Session lasted {} — on the longer side.", fmt_duration(report.duration_secs)),
            advice: "Long sessions can lose patient attention. Consider whether you can tighten the delivery.".into(),
        });
    }
}

fn fmt_duration(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{}:{:02}", m, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_report() -> ComplianceReport {
        ComplianceReport {
            script_title: "Test".into(),
            script_version: None,
            sections_covered: vec!["Intro".into(), "Findings".into(), "Recommendations".into(), "Closing".into()],
            sections_skipped: vec![],
            duration_secs: 900, // 15 minutes
            section_times: HashMap::from([
                ("Intro".into(), 120),
                ("Findings".into(), 360),
                ("Recommendations".into(), 300),
                ("Closing".into(), 120),
            ]),
            pause_points_reached: 4,
            pause_points_total: 4,
            branches_taken: HashMap::from([("Question?".into(), "YES".into())]),
            total_words: 2000,
            words_delivered: 1800,
            ..Default::default()
        }
    }

    #[test]
    fn good_session_gets_praise() {
        let insights = analyze(&sample_report());
        let praise: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Praise).collect();
        assert!(!praise.is_empty(), "Good session should get praise");
    }

    #[test]
    fn skipped_section_is_critical() {
        let mut report = sample_report();
        report.sections_covered = vec!["Intro".into(), "Findings".into()];
        report.sections_skipped = vec!["Recommendations".into(), "Closing".into()];

        let insights = analyze(&report);
        let critical: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Critical).collect();
        assert_eq!(critical.len(), 2, "Each skipped section should be critical");
    }

    #[test]
    fn rushed_session_gets_warning() {
        let mut report = sample_report();
        report.duration_secs = 90; // 1.5 minutes for 4 sections

        let insights = analyze(&report);
        let warnings: Vec<_> = insights.iter().filter(|i| i.severity == Severity::Warning).collect();
        assert!(!warnings.is_empty(), "Rushed session should get warnings");
    }

    #[test]
    fn coaching_markdown_output() {
        let report = sample_report();
        let md = coaching_markdown(&report);
        assert!(md.contains("# Delivery Coaching"));
        assert!(md.contains("What went well"));
    }

    #[test]
    fn fast_speaking_pace_is_flagged_and_normal_is_praised() {
        let mut r = sample_report();
        r.delivery.speaking_wpm = Some(190.0);
        assert!(analyze(&r).iter().any(|i| i.category == InsightCategory::Pacing && i.severity == Severity::Warning));
        r.delivery.speaking_wpm = Some(130.0);
        assert!(analyze(&r).iter().any(|i| i.category == InsightCategory::Pacing && i.severity == Severity::Praise));
    }

    #[test]
    fn omitted_lines_and_low_patient_talk_are_reported() {
        let mut r = sample_report();
        r.delivery.omitted_lines = vec!["garlic extract can thin your blood".into()];
        r.delivery.verified_by_recording = true;
        r.delivery.patient_talk_secs = Some(30);
        let ins = analyze(&r);
        assert!(ins.iter().any(|i| i.category == InsightCategory::Delivery && i.severity == Severity::Warning));
        assert!(ins.iter().any(|i| i.category == InsightCategory::Interaction && i.severity == Severity::Warning));
    }
}
