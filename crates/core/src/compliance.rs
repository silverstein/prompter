use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ──────────────────────────────────────────────────────────────
// Compliance report — written after each teleprompter session.
//
//   Session events ──▶ ComplianceReport ──▶ YAML frontmatter + transcript
//                                          saved to ~/meetings/consults/
//
// The report includes: sections covered, time per section,
// pause points reached, branches taken, total duration.
// ──────────────────────────────────────────────────────────────

/// A completed session's compliance data.
#[derive(Debug, Clone, Serialize)]
pub struct ComplianceReport {
    pub script_title: String,
    pub script_version: Option<String>,
    pub sections_covered: Vec<String>,
    pub sections_skipped: Vec<String>,
    pub duration_secs: u64,
    pub section_times: HashMap<String, u64>,
    pub pause_points_reached: usize,
    pub pause_points_total: usize,
    pub branches_taken: HashMap<String, String>,
    pub total_words: usize,
    pub words_delivered: usize,
}

impl ComplianceReport {
    /// Format duration as "M:SS".
    pub fn duration_display(&self) -> String {
        fmt_duration(self.duration_secs)
    }

    /// Adherence percentage (words delivered / total words).
    pub fn adherence_pct(&self) -> f64 {
        if self.total_words == 0 {
            return 0.0;
        }
        (self.words_delivered as f64 / self.total_words as f64 * 100.0).min(100.0)
    }

    /// Write the compliance report as a Minutes-compatible markdown file.
    /// Returns the path where the file was written.
    pub fn write_to_dir(&self, dir: &Path) -> Result<PathBuf, std::io::Error> {
        std::fs::create_dir_all(dir)?;

        // Generate filename from title + date
        let date = chrono_lite_date();
        let slug = slugify(&self.script_title);
        let filename = format!("{}-{}.md", date, slug);
        let path = dir.join(&filename);

        let mut content = String::new();

        // YAML frontmatter
        content.push_str("---\n");
        content.push_str(&format!("title: \"{}\"\n", self.script_title));
        content.push_str("type: consultation\n");
        if let Some(v) = &self.script_version {
            content.push_str(&format!("script_version: \"{}\"\n", v));
        }
        content.push_str(&format!("date: \"{}\"\n", date));
        content.push_str(&format!("duration: \"{}\"\n", self.duration_display()));

        // Compliance block
        content.push_str("compliance:\n");
        content.push_str(&format!(
            "  sections_covered: [{}]\n",
            self.sections_covered.join(", ")
        ));
        if !self.sections_skipped.is_empty() {
            content.push_str(&format!(
                "  sections_skipped: [{}]\n",
                self.sections_skipped.join(", ")
            ));
        }
        content.push_str(&format!(
            "  adherence: {:.0}%\n",
            self.adherence_pct()
        ));
        content.push_str(&format!(
            "  pause_points: {}/{}\n",
            self.pause_points_reached, self.pause_points_total
        ));

        // Section times
        if !self.section_times.is_empty() {
            content.push_str("  section_times:\n");
            for name in &self.sections_covered {
                if let Some(&secs) = self.section_times.get(name) {
                    content.push_str(&format!("    {}: \"{}\"\n", name, fmt_duration(secs)));
                }
            }
        }

        // Branches taken
        if !self.branches_taken.is_empty() {
            content.push_str("  branches:\n");
            for (q, a) in &self.branches_taken {
                content.push_str(&format!("    \"{}\": \"{}\"\n", q, a));
            }
        }

        content.push_str("---\n\n");

        // Body: summary
        content.push_str(&format!("# {}\n\n", self.script_title));
        content.push_str(&format!(
            "Consultation completed in {}. {} of {} sections covered ({:.0}% adherence).\n\n",
            self.duration_display(),
            self.sections_covered.len(),
            self.sections_covered.len() + self.sections_skipped.len(),
            self.adherence_pct()
        ));

        // Section breakdown
        content.push_str("## Sections\n\n");
        for name in &self.sections_covered {
            let time = self
                .section_times
                .get(name)
                .map(|s| fmt_duration(*s))
                .unwrap_or_default();
            content.push_str(&format!("- [x] {} ({})\n", name, time));
        }
        for name in &self.sections_skipped {
            content.push_str(&format!("- [ ] {} (skipped)\n", name));
        }

        // Write with restrictive permissions (0600)
        std::fs::write(&path, &content)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }

        Ok(path)
    }
}

fn fmt_duration(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    format!("{}:{:02}", m, s)
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Simple date string without chrono dependency.
fn chrono_lite_date() -> String {
    // Use std::time for a basic YYYY-MM-DD
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Simple unix timestamp to date conversion
    let days = now / 86400;
    let mut y = 1970i64;
    let mut remaining = days as i64;

    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }

    let months_days: [i64; 12] = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut m = 1;
    for &md in &months_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    let d = remaining + 1;

    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adherence_calculation() {
        let report = ComplianceReport {
            script_title: "Test".into(),
            script_version: Some("1.0".into()),
            sections_covered: vec!["A".into(), "B".into()],
            sections_skipped: vec![],
            duration_secs: 120,
            section_times: HashMap::from([("A".into(), 60), ("B".into(), 60)]),
            pause_points_reached: 3,
            pause_points_total: 3,
            branches_taken: HashMap::new(),
            total_words: 1000,
            words_delivered: 850,
        };

        assert_eq!(report.adherence_pct(), 85.0);
        assert_eq!(report.duration_display(), "2:00");
    }

    #[test]
    fn slugify_title() {
        assert_eq!(slugify("MTM Consultation — Jane Smith"), "mtm-consultation-jane-smith");
        assert_eq!(slugify("Hello World!"), "hello-world");
    }

    #[test]
    fn write_report_to_temp_dir() {
        let dir = std::env::temp_dir().join("prompter-test-compliance");
        let _ = std::fs::remove_dir_all(&dir);

        let report = ComplianceReport {
            script_title: "Test Consultation".into(),
            script_version: Some("1.0".into()),
            sections_covered: vec!["Intro".into(), "Findings".into()],
            sections_skipped: vec!["Closing".into()],
            duration_secs: 300,
            section_times: HashMap::from([("Intro".into(), 120), ("Findings".into(), 180)]),
            pause_points_reached: 2,
            pause_points_total: 3,
            branches_taken: HashMap::from([("Question?".into(), "YES".into())]),
            total_words: 500,
            words_delivered: 400,
        };

        let path = report.write_to_dir(&dir).expect("write failed");
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("title: \"Test Consultation\""));
        assert!(content.contains("adherence: 80%"));
        assert!(content.contains("- [x] Intro"));
        assert!(content.contains("- [ ] Closing (skipped)"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
