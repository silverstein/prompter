# Integrating with Prompter

## For RxVIP Pharmacy Ecosystem (or any script source)

Prompter picks up consultation scripts from **`~/meetings/scripts/`**. Any `.md` file placed there appears automatically in Prompter's "Recent & Available Scripts" list on launch.

### Export format

Scripts should follow the `.script.md` format (see [SPEC.md](./SPEC.md) for full spec). Minimal example:

```markdown
---
title: MTM Consultation — Jane Smith
type: pharmacy-consultation
version: "1.0"
variables:
  patient_name: Jane Smith
  medications: [Warfarin, Metformin]
  supplements: [Garlic extract, Ginkgo biloba]
  symptoms: [dizziness, bruising]
  insurance: Humana
estimated_duration: 18min
---

# Intro

Hi {{patient_name}}, thanks for meeting with me today...

> PAUSE: Does that sound helpful to you?

# Findings

You are currently taking {{medications}}...

> PAUSE: Does this make sense so far?

# Recommendations

Based on everything we uncovered...

> BRANCH: Would you like me to get this plan organized?
>> YES
Great, let me get started...
>> NO
No problem. May I share with your doctor?

# Closing

Do you have any questions?
```

### Key directives

- `> PAUSE: question text` — teleprompter auto-pauses here, shows the question
- `> BRANCH: question` + `>> LABEL` lines — user selects a path
- `{{variable}}` — substituted from the `variables:` frontmatter at load time
- `# Heading` — creates a section (navigation landmark + compliance checkpoint)

### Frontmatter is optional

Plain text without `---` frontmatter works too. Prompter will derive a title from the first line and treat the whole text as a single section. But frontmatter gives you variables, compliance tracking, and script versioning.

### Where to save

```
~/meetings/scripts/YYYY-MM-DD-patient-slug.script.md
```

Example: `~/meetings/scripts/2026-03-20-jane-smith-mtm.script.md`

Prompter lists all `.md` files in this directory, sorted by modification time (newest first).

### Alternative: Clipboard

Users can also Cmd+V in Prompter to paste a script from their clipboard. The RxVIP app can offer a "Copy Script" button that copies the formatted markdown.

### Coming soon: URL scheme

`prompter://open?file=/path/to/script.md` — will open Prompter with the script pre-loaded. Not yet implemented.
