# .script.md Format Specification

**Version:** 1.0
**Status:** Draft

## Overview

`.script.md` is a lightweight format for structured conversation scripts — consultations, sales pitches, presentations, or any high-stakes dialogue where the speaker needs guided delivery with interactive pause points and conditional branching.

The format is intentionally simple: annotated Markdown that any text editor can create and any Markdown renderer can display (directives render as blockquotes). The annotations add structure that teleprompter software can use for intelligent scroll control, compliance tracking, and session reporting.

## Structure

A `.script.md` file has two parts:

1. **Frontmatter** (optional) — YAML metadata between `---` delimiters
2. **Body** — Markdown text with optional section headings and directives

If frontmatter is omitted, the parser treats the entire file as body text and derives the title from the first line.

## Frontmatter

```yaml
---
title: MTM Consultation — Jane Smith
type: pharmacy-consultation
version: "2.1"
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
  supplements: [Garlic extract, Ginkgo biloba, St. John's Wort]
  symptoms: [dizziness, unusual bruising]
  insurance: Humana
estimated_duration: 18min
---
```

### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `title` | string | Yes (if frontmatter present) | Display title for the script |
| `type` | string | No | Script category (e.g., `pharmacy-consultation`, `sales-pitch`, `presentation`) |
| `version` | string | No | Script version for tracking iterations |
| `variables` | map | No | Key-value pairs for `{{variable}}` substitution in the body |
| `estimated_duration` | string | No | Human-readable estimate (e.g., `18min`, `1h`) |

### Variable Substitution

Variables defined in frontmatter are substituted into the body at parse time:

```
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
```

In the body:
- `{{patient_name}}` → `Jane Smith`
- `{{medications}}` → `Warfarin, Metformin` (arrays join with `, `)

Unresolved variables (no matching key in frontmatter) are left as-is: `{{undefined}}` renders literally.

## Sections

Sections are delimited by `# Heading` (H1 Markdown headings):

```markdown
# Intro

Script text here...

# Findings

More script text...
```

If no headings are present, the entire body is treated as a single section named "Main".

Sections serve as:
- **Navigation landmarks** in the teleprompter UI
- **Compliance checkpoints** (was this section covered?)
- **Jump targets** (keyboard shortcuts, section nav bar)

## Directives

Directives are special lines that control teleprompter behavior. They use Markdown blockquote syntax (`> `) so they render readably in any Markdown viewer.

### PAUSE

```markdown
> PAUSE: Does that sound helpful to you?
```

A pause point where the teleprompter stops scrolling and waits. The text after `PAUSE:` is the question or prompt being asked — it's displayed as a visual cue to the speaker.

**Behavior:** The teleprompter pauses until the user explicitly resumes (Space key or tap). In VAD mode, the pause also serves as a natural boundary where the other person is expected to speak.

### BRANCH

```markdown
> BRANCH: Would you like me to get this plan organized?
>> YES
Is there anyone else who should know about this? What's the best way to reach them?
>> NO
May I have your permission to share with your doctor? Thank you, I hope this helped.
```

A conditional branch where the speaker selects one of several paths based on the other person's response.

**Syntax:**
- `> BRANCH: question text` — the branch question
- `>> LABEL` — an option label (must immediately follow the BRANCH or another option's text)
- Lines after `>> LABEL` until the next `>>` or directive are that option's script text

**Behavior:** The teleprompter displays the option labels as tap targets. The speaker selects one, and only that option's text is shown.

## Body Text

All non-directive, non-heading text is spoken content. The parser splits it into sentences for teleprompter highlighting.

**Sentence boundaries:** Period, question mark, or exclamation mark followed by a space and an uppercase letter. This handles common abbreviations (Mr., Dr., etc.) and decimal numbers without false splits.

**Paragraph breaks:** Empty lines separate paragraph blocks. Each paragraph block is rendered as a visual group in the teleprompter.

## Complete Example

```markdown
---
title: MTM Consultation — Jane Smith
type: pharmacy-consultation
version: "2.1"
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
estimated_duration: 18min
---

# Intro

Hi {{patient_name}}, thanks for meeting with me today. My goal is simple: to make sure every medication you're taking is safe and working as well as possible.

> PAUSE: Does that sound helpful to you?

# Findings

You are currently taking {{medications}}. Let me walk you through what I found.

> PAUSE: Does this make sense so far?

# Recommendations

Based on everything we uncovered, I have a specific plan.

> BRANCH: Would you like me to get this plan started?
>> YES
Great. Is there anyone else who should know about this plan?
>> NO
No problem. May I share what we discussed with your doctor?

# Closing

Do you have any questions about what we discussed today?
```

## Compliance Output

After a teleprompter session, the compliance tracker can produce a YAML report:

```yaml
compliance:
  script_title: "MTM Consultation — Jane Smith"
  script_version: "2.1"
  sections_covered: [Intro, Findings, Recommendations, Closing]
  sections_skipped: []
  duration: "17:42"
  section_times:
    Intro: "2:15"
    Findings: "6:30"
    Recommendations: "7:12"
    Closing: "1:45"
  pause_points_reached: 3
  pause_points_total: 3
  branches_taken:
    "Would you like me to get this plan started?": "YES"
```

## Design Principles

1. **Plain text first** — Any text editor can create a `.script.md`. No special tooling required.
2. **Graceful degradation** — In a standard Markdown renderer, directives display as blockquotes. The script is readable without a teleprompter.
3. **Frontmatter is optional** — Paste plain text and it works. Frontmatter adds structure for compliance tracking and variable substitution.
4. **Sections are navigation** — H1 headings create landmarks, not hierarchy. H2+ headings are treated as body text.
5. **Sentences are the unit** — The teleprompter highlights and advances one sentence at a time, not paragraphs or lines.
