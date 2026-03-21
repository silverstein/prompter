use crate::error::ParseError;
use serde::Deserialize;
use std::collections::HashMap;

// ──────────────────────────────────────────────────────────────
// Script model — the output of parsing a .script.md file.
//
//   .script.md text
//        │
//        ├─ YAML frontmatter ──▶ Frontmatter (title, variables, version)
//        │
//        └─ Markdown body ──▶ Vec<Section>
//             │
//             ├─ # Heading ──▶ Section boundary
//             ├─ Plain text ──▶ split into Sentences
//             ├─ > PAUSE: ──▶ Directive::Pause
//             └─ > BRANCH: ──▶ Directive::Branch
//                  >> YES ──▶ BranchOption
//                  >> NO  ──▶ BranchOption
// ──────────────────────────────────────────────────────────────

/// Parsed script ready for the teleprompter.
#[derive(Debug, Clone)]
pub struct Script {
    pub frontmatter: Frontmatter,
    pub sections: Vec<Section>,
    /// Total word count across all sections (for time estimates).
    pub word_count: usize,
}

/// YAML frontmatter from the script header.
#[derive(Debug, Clone, Deserialize)]
pub struct Frontmatter {
    pub title: String,
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub variables: HashMap<String, serde_yaml::Value>,
    #[serde(default)]
    pub estimated_duration: Option<String>,
}

/// A named section of the script (delimited by # headings).
#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub elements: Vec<Element>,
    pub word_count: usize,
}

/// An element within a section — either spoken text or a directive.
#[derive(Debug, Clone)]
pub enum Element {
    /// A block of spoken text, split into individual sentences.
    Text(Vec<Sentence>),
    /// A directive (pause point, branch, etc.).
    Directive(Directive),
}

/// A single sentence — the unit of teleprompter highlighting.
#[derive(Debug, Clone)]
pub struct Sentence {
    pub text: String,
    pub word_count: usize,
}

/// A script directive that controls teleprompter behavior.
#[derive(Debug, Clone)]
pub enum Directive {
    /// Pause point — teleprompter waits for the other person to respond.
    Pause { prompt: String },
    /// Branch — pharmacist selects one of several paths.
    Branch {
        question: String,
        options: Vec<BranchOption>,
    },
}

/// One option in a branch directive.
#[derive(Debug, Clone)]
pub struct BranchOption {
    pub label: String,
    pub sentences: Vec<Sentence>,
}

/// Parse a .script.md file into a structured Script.
///
/// This is a pure function: input string → Result<Script, ParseError>.
/// Variables in frontmatter are substituted into the body at parse time.
///
/// Frontmatter is optional. Plain text without `---` is accepted and gets
/// a default title derived from the first line of content.
pub fn parse(input: &str) -> Result<Script, ParseError> {
    let (fm, body) = match split_frontmatter(input) {
        Ok((yaml_str, body)) => {
            let fm: Frontmatter = serde_yaml::from_str(&yaml_str)
                .map_err(|e| ParseError::Yaml(e.to_string()))?;
            (fm, body)
        }
        Err(ParseError::MissingFrontmatter) => {
            // No frontmatter — treat entire input as body with default metadata.
            let first_line = input.lines().find(|l| !l.trim().is_empty()).unwrap_or("Untitled");
            let title = first_line
                .trim()
                .trim_start_matches("# ")
                .chars()
                .take(60)
                .collect::<String>();
            let fm = Frontmatter {
                title: if title.is_empty() {
                    "Untitled Script".to_string()
                } else {
                    title
                },
                r#type: None,
                version: None,
                variables: HashMap::new(),
                estimated_duration: None,
            };
            (fm, input.to_string())
        }
        Err(e) => return Err(e),
    };

    let body = substitute_variables(&body, &fm.variables);

    let body = body.trim();
    if body.is_empty() {
        return Err(ParseError::EmptyScript);
    }

    let sections = parse_sections(body)?;
    let word_count = sections.iter().map(|s| s.word_count).sum();

    Ok(Script {
        frontmatter: fm,
        sections,
        word_count,
    })
}

/// Split input into YAML frontmatter and markdown body.
fn split_frontmatter(input: &str) -> Result<(String, String), ParseError> {
    let trimmed = input.trim_start();
    if !trimmed.starts_with("---") {
        return Err(ParseError::MissingFrontmatter);
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let close_pos = after_first
        .find("\n---")
        .ok_or(ParseError::MissingFrontmatter)?;

    let yaml = after_first[..close_pos].trim().to_string();
    let body = after_first[close_pos + 4..].to_string();

    Ok((yaml, body))
}

/// Replace {{variable}} placeholders with values from frontmatter.
fn substitute_variables(body: &str, variables: &HashMap<String, serde_yaml::Value>) -> String {
    let mut result = body.to_string();
    for (key, value) in variables {
        let placeholder = format!("{{{{{}}}}}", key);
        let replacement = match value {
            serde_yaml::Value::String(s) => s.clone(),
            serde_yaml::Value::Sequence(seq) => seq
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            other => format!("{:?}", other),
        };
        result = result.replace(&placeholder, &replacement);
    }
    result
}

/// Parse markdown body into sections delimited by # headings.
fn parse_sections(body: &str) -> Result<Vec<Section>, ParseError> {
    let mut sections = Vec::new();
    let mut current_name = String::new();
    let mut current_lines: Vec<(usize, &str)> = Vec::new();
    let mut in_default_section = true;

    for (line_num, line) in body.lines().enumerate() {
        let line_num = line_num + 1; // 1-indexed for error messages
        if let Some(heading) = line.strip_prefix("# ") {
            // Flush previous section
            if !current_lines.is_empty() || !in_default_section {
                sections.push(build_section(&current_name, &current_lines)?);
            }
            current_name = heading.trim().to_string();
            current_lines = Vec::new();
            in_default_section = false;
        } else {
            current_lines.push((line_num, line));
        }
    }

    // Flush final section
    if !current_lines.is_empty() {
        if in_default_section && current_name.is_empty() {
            current_name = "Main".to_string();
        }
        sections.push(build_section(&current_name, &current_lines)?);
    }

    Ok(sections)
}

/// Build a Section from its name and raw lines.
fn build_section(name: &str, lines: &[(usize, &str)]) -> Result<Section, ParseError> {
    let mut elements: Vec<Element> = Vec::new();
    let mut text_buffer = String::new();
    let mut i = 0;

    while i < lines.len() {
        let (line_num, line) = lines[i];
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("> PAUSE:") {
            // Flush text buffer
            flush_text(&mut text_buffer, &mut elements);
            elements.push(Element::Directive(Directive::Pause {
                prompt: rest.trim().to_string(),
            }));
            i += 1;
        } else if let Some(rest) = trimmed.strip_prefix("> BRANCH:") {
            // Flush text buffer
            flush_text(&mut text_buffer, &mut elements);

            let question = rest.trim().to_string();
            let mut options = Vec::new();

            // Collect branch options (lines starting with >>)
            i += 1;
            while i < lines.len() {
                let (_opt_line_num, opt_line) = lines[i];
                let opt_trimmed = opt_line.trim();

                if let Some(label) = opt_trimmed.strip_prefix(">> ") {
                    let label = label.trim().to_string();
                    let mut option_text = String::new();

                    // Collect lines until next >> or non-branch content
                    i += 1;
                    while i < lines.len() {
                        let (_, next_line) = lines[i];
                        let next_trimmed = next_line.trim();
                        if next_trimmed.starts_with(">> ")
                            || next_trimmed.starts_with("> PAUSE:")
                            || next_trimmed.starts_with("> BRANCH:")
                            || next_trimmed.starts_with("# ")
                        {
                            break;
                        }
                        if !next_trimmed.is_empty() {
                            if !option_text.is_empty() {
                                option_text.push(' ');
                            }
                            option_text.push_str(next_trimmed);
                        }
                        i += 1;
                    }

                    options.push(BranchOption {
                        label,
                        sentences: split_sentences(&option_text),
                    });
                } else {
                    // End of branch options
                    break;
                }
            }

            if options.is_empty() {
                return Err(ParseError::EmptyBranch { line: line_num });
            }

            elements.push(Element::Directive(Directive::Branch { question, options }));
            // Don't increment i — we already advanced past the branch
        } else {
            // Regular text line
            if !trimmed.is_empty() {
                if !text_buffer.is_empty() {
                    text_buffer.push(' ');
                }
                text_buffer.push_str(trimmed);
            } else if !text_buffer.is_empty() {
                // Empty line = paragraph break, flush
                flush_text(&mut text_buffer, &mut elements);
            }
            i += 1;
        }
    }

    // Flush remaining text
    flush_text(&mut text_buffer, &mut elements);

    let word_count = elements
        .iter()
        .map(|e| match e {
            Element::Text(sentences) => sentences.iter().map(|s| s.word_count).sum(),
            Element::Directive(Directive::Branch { options, .. }) => options
                .iter()
                .flat_map(|o| &o.sentences)
                .map(|s| s.word_count)
                .sum(),
            Element::Directive(Directive::Pause { .. }) => 0,
        })
        .sum();

    Ok(Section {
        name: name.to_string(),
        elements,
        word_count,
    })
}

/// Flush accumulated text into sentences and add to elements.
fn flush_text(buffer: &mut String, elements: &mut Vec<Element>) {
    if buffer.is_empty() {
        return;
    }
    let sentences = split_sentences(buffer);
    if !sentences.is_empty() {
        elements.push(Element::Text(sentences));
    }
    buffer.clear();
}

/// Split a text block into individual sentences.
/// Handles: periods, question marks, exclamation marks.
/// Preserves abbreviations (Mr., Dr., etc.) and decimal numbers.
fn split_sentences(text: &str) -> Vec<Sentence> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len {
        current.push(chars[i]);

        if (chars[i] == '.' || chars[i] == '?' || chars[i] == '!') && i + 1 < len {
            let next = chars[i + 1];
            // End of sentence if followed by space + uppercase, or end of string
            if next == ' ' && i + 2 < len && chars[i + 2].is_uppercase() {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    let word_count = trimmed.split_whitespace().count();
                    sentences.push(Sentence {
                        text: trimmed,
                        word_count,
                    });
                }
                current.clear();
                i += 1; // skip the space
            }
        }

        i += 1;
    }

    // Flush remaining
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        let word_count = trimmed.split_whitespace().count();
        sentences.push(Sentence {
            text: trimmed,
            word_count,
        });
    }

    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE_SCRIPT: &str = r#"---
title: Test Consultation
type: pharmacy-consultation
version: "1.0"
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
estimated_duration: 5min
---

# Intro

Hi {{patient_name}}, thanks for meeting with me today. My goal is simple.

> PAUSE: Does that sound helpful to you?

# Findings

You are currently taking {{medications}}. Let me explain what I found.

> PAUSE: Does this make sense so far?

# Recommendations

Based on everything we just uncovered, I have a specific plan.

> BRANCH: Would you like me to get this plan organized?
>> YES
Is there anyone else who should know about this?
>> NO
May I have your permission to share with your doctor?

# Closing

Before we wrap up, do you have any questions?
"#;

    #[test]
    fn parse_simple_script() {
        let script = parse(SIMPLE_SCRIPT).unwrap();

        assert_eq!(script.frontmatter.title, "Test Consultation");
        assert_eq!(
            script.frontmatter.r#type.as_deref(),
            Some("pharmacy-consultation")
        );
        assert_eq!(script.frontmatter.version.as_deref(), Some("1.0"));
        assert_eq!(script.sections.len(), 4);

        // Check section names
        assert_eq!(script.sections[0].name, "Intro");
        assert_eq!(script.sections[1].name, "Findings");
        assert_eq!(script.sections[2].name, "Recommendations");
        assert_eq!(script.sections[3].name, "Closing");
    }

    #[test]
    fn variable_substitution() {
        let script = parse(SIMPLE_SCRIPT).unwrap();

        // Check that {{patient_name}} was replaced in Intro
        let intro = &script.sections[0];
        if let Element::Text(sentences) = &intro.elements[0] {
            assert!(
                sentences[0].text.contains("Jane Smith"),
                "Expected 'Jane Smith' in: {}",
                sentences[0].text
            );
        } else {
            panic!("Expected Text element, got {:?}", intro.elements[0]);
        }

        // Check that {{medications}} was replaced in Findings
        let findings = &script.sections[1];
        if let Element::Text(sentences) = &findings.elements[0] {
            assert!(
                sentences[0].text.contains("Warfarin, Metformin"),
                "Expected 'Warfarin, Metformin' in: {}",
                sentences[0].text
            );
        } else {
            panic!("Expected Text element");
        }
    }

    #[test]
    fn pause_directives() {
        let script = parse(SIMPLE_SCRIPT).unwrap();
        let intro = &script.sections[0];

        // Intro should have: Text, Pause
        assert_eq!(intro.elements.len(), 2);
        match &intro.elements[1] {
            Element::Directive(Directive::Pause { prompt }) => {
                assert_eq!(prompt, "Does that sound helpful to you?");
            }
            other => panic!("Expected Pause directive, got {:?}", other),
        }
    }

    #[test]
    fn branch_directives() {
        let script = parse(SIMPLE_SCRIPT).unwrap();
        let recs = &script.sections[2];

        // Find the branch directive
        let branch = recs.elements.iter().find_map(|e| match e {
            Element::Directive(Directive::Branch { question, options }) => {
                Some((question, options))
            }
            _ => None,
        });

        let (question, options) = branch.expect("Expected a Branch directive in Recommendations");
        assert_eq!(question, "Would you like me to get this plan organized?");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "YES");
        assert_eq!(options[1].label, "NO");
        assert!(!options[0].sentences.is_empty());
        assert!(!options[1].sentences.is_empty());
    }

    #[test]
    fn sentence_splitting() {
        let sentences = split_sentences(
            "Hi, thanks for meeting with me today. My goal is simple: To make sure every medication you're taking is safe.",
        );
        assert_eq!(sentences.len(), 2);
        assert!(sentences[0].text.starts_with("Hi, thanks"));
        assert!(sentences[1].text.starts_with("My goal"));
    }

    #[test]
    fn word_count() {
        let script = parse(SIMPLE_SCRIPT).unwrap();
        assert!(script.word_count > 0, "Expected non-zero word count");

        // Each section should have a word count
        for section in &script.sections {
            assert!(
                section.word_count > 0,
                "Section '{}' has zero words",
                section.name
            );
        }
    }

    #[test]
    fn plain_text_without_frontmatter() {
        let script = parse("Hi, thanks for meeting with me today. My goal is simple.").unwrap();
        assert_eq!(script.frontmatter.title, "Hi, thanks for meeting with me today. My goal is simple.");
        assert_eq!(script.sections.len(), 1);
        assert_eq!(script.sections[0].name, "Main");
    }

    #[test]
    fn plain_text_with_sections_no_frontmatter() {
        let input = "# Intro\n\nHello there.\n\n# Findings\n\nHere is what I found.";
        let script = parse(input).unwrap();
        assert_eq!(script.frontmatter.title, "Intro");
        assert_eq!(script.sections.len(), 2);
        assert_eq!(script.sections[0].name, "Intro");
        assert_eq!(script.sections[1].name, "Findings");
    }

    #[test]
    fn empty_script_body() {
        let result = parse("---\ntitle: Empty\n---\n");
        assert!(matches!(result, Err(ParseError::EmptyScript)));
    }

    #[test]
    fn missing_title() {
        let result = parse("---\ntype: test\n---\n# Section\nHello.");
        // serde_yaml returns a Yaml parse error when title (required String) is missing
        assert!(matches!(result, Err(ParseError::Yaml(_))));
    }

    #[test]
    fn no_sections_gets_default_name() {
        let script = parse("---\ntitle: No Sections\n---\nJust some text here.").unwrap();
        assert_eq!(script.sections.len(), 1);
        assert_eq!(script.sections[0].name, "Main");
    }

    #[test]
    fn empty_branch_is_error() {
        let input = "---\ntitle: Test\n---\n# Section\n\n> BRANCH: Choose one\n\nSome text.";
        let result = parse(input);
        assert!(
            matches!(result, Err(ParseError::EmptyBranch { .. })),
            "Expected EmptyBranch error, got {:?}",
            result
        );
    }
}
