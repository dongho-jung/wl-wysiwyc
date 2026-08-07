# Repository conventions

Rules for everyone working in this repository, human or AI agent alike.
CLAUDE.md is a symlink to this file so both read the same document. Edit
AGENTS.md only.

## Language

English only, everywhere. This covers:

- Code: identifiers, string constants, error messages, log output
- Comments and docstrings
- Documentation, README files, design notes
- Commit messages, header and body
- PR titles, PR bodies, review comments
- Issue titles and bodies, TODO markers

Do not mix languages. The only exception is data that must stay in its
original language, such as test fixtures or i18n resources.

## Writing style

Text must read like a person typed it, whether or not a person typed it.
These rules apply to code, comments, docs, commits, and PRs.

Typography:

- Straight ASCII quotes only: `"` and `'`. Curly quotes are banned.
- Plain hyphen `-` only. Em-dashes and en-dashes are banned. Use a
  comma, a colon, parentheses, or split the sentence.
- Write three periods `...`, never the single-character ellipsis.
- ASCII punctuation only, unless the non-ASCII character is itself data.
- No emoji. No decorative Unicode: arrows, check marks, and similar
  symbols are banned outside code that needs them.

Tone:

- No filler phrases: "It is worth noting", "Certainly", "I hope this
  helps", "Let's dive in".
- No hype adjectives: "powerful", "seamless", "robust", "comprehensive",
  "blazing fast".
- Short sentences. Active voice. Concrete claims over hedged ones.
- Use lists for actual enumerations and prose for reasoning. Do not
  turn every paragraph into bullet points.
- Bold sparingly, only for genuinely load-bearing terms.

## Commits

### Header

Format: `type(scope): summary`. Scope is optional: `type: summary`.

- Allowed types: feat, fix, refactor, perf, docs, test, chore, build,
  ci, style, revert.
- Breaking changes: append `!` to the type, as in `feat!: ...` or
  `feat(api)!: ...`, and explain the break in the body.
- Scope is a short noun for the area touched: a module, directory, or
  subsystem. Skip it when the change is repo-wide.
- Summary is imperative mood: "add", not "added" or "adds". Lowercase
  first word unless it is an identifier. No trailing period.
- Aim for 50 characters or fewer; 72 is the hard limit.

Picking the type when in doubt: broken-to-correct is fix, new capability
is feat, same behavior with different code is refactor (perf if speed is
the point), and anything around the code (deps, config, tooling) is
chore, build, or ci.

Good:

```
fix(parser): handle empty input without panicking
feat: add --dry-run flag
refactor(store): extract retry logic into a helper
```

Bad:

```
Fixed the bug            (no type, past tense)
feat: Added new stuff.   (past tense, vague, trailing period)
update code              (says nothing)
```

### Body

- One blank line between header and body.
- Wrap body lines at 72 characters.
- Explain what changed and why. The diff already shows how.
- Call out side effects, tradeoffs, and anything a reviewer would
  otherwise have to ask about.
- Bullets or prose, whichever reads better for the change.
- Reference issues in a footer line: `Fixes: #12` or `Refs: #34`.
- The body is optional only for trivial changes: typos, formatting,
  version bumps. If the header alone cannot explain the change, write
  a body.

### Granularity

- One commit is one logical change. Never mix a refactor with a
  behavior change, or a feature with an unrelated fix.
- Each commit should build and pass tests on its own.
- No commented-out code, debug prints, or scratch files in commits.

## Branches

Name branches `type/short-kebab-description`, reusing the commit types:
`feat/inline-editor`, `fix/empty-selection-crash`. Do not commit
directly to main.

## Pull requests

### Title

- Same format as a commit header: `type(scope): summary`.
- Describe the net effect of the whole PR, not its last commit.
- For a single-commit PR, reuse the commit header verbatim.
- Mark unfinished work as a draft PR instead of writing WIP in the
  title.

### Body

Structure every PR body like this:

```
## Summary

What this PR does and why, in one to three sentences. Link related
issues here (Fixes #12).

## Changes

- Notable change one
- Notable change two

## Testing

How the change was verified: tests added or run, manual steps taken,
or an explicit statement of why it could not be tested.
```

Rules:

- Summary states the motivation, not a restatement of the title.
- Changes lists what a reviewer should focus on, not every file
  touched.
- Testing is mandatory. "Not tested" is an acceptable entry; silence
  is not.
- Add screenshots or output samples when the change is visual or
  CLI-facing.
- Keep it short and specific. Padding a PR body with generated-sounding
  prose is worse than being terse.
- Keep one PR to one topic. Split independent work into separate PRs.
