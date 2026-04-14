# Ship It

Update `CHANGELOG.md` and (when relevant) `README.md`, commit, push, and open a pull request for the pending changes on the current branch.

## Input

`$ARGUMENTS` is optional. If provided, treat it as a hint for the PR title / changelog framing (e.g. "analysis tab polish"). If omitted, derive everything from the diff.

## Preconditions

- Refuse to run on `main`. Tell the user to create a feature branch first.
- If the working tree is clean AND there are no commits ahead of `origin/main`, there's nothing to ship — stop and say so.

## Steps

### 1. Survey the change

Run these in parallel:

- `git status` (no `-uall`) — list staged/unstaged/untracked files
- `git diff` — unstaged changes
- `git diff --staged` — staged changes
- `git log origin/main..HEAD --oneline` — commits already on the branch
- `git diff origin/main...HEAD` — full branch diff vs. main

Read the current `CHANGELOG.md` `[Unreleased]` section so you know what's already there and don't duplicate entries.

### 2. Update `CHANGELOG.md`

Add entries to the **`[Unreleased]`** section under the right Keep-a-Changelog buckets (`Added` / `Changed` / `Deprecated` / `Removed` / `Fixed` / `Security`). Create a bucket if it doesn't exist yet; drop empty buckets.

Style rules (match the existing file — read a few recent entries first):

- One bullet per user-visible change. Don't enumerate every file touched.
- Lead with **what the user sees / can now do**, not the internal refactor. If the change is purely internal (no user-visible effect), usually skip it — changelog is for users.
- Use backticks for code identifiers, keybindings (`[a]`, `Alt+Enter`), file paths, and subcommand names.
- Bold the feature name (`**Foo bar**`) only for substantial new surfaces, matching existing convention.
- Don't invent a version header. Leave things under `[Unreleased]`; releases happen via tag, not this command.
- If `$ARGUMENTS` is provided, use it to shape tone but don't paste it verbatim.

### 3. Update `README.md` (when relevant)

Decide whether the change touches anything the README documents. Skim `README.md` first so you know what surfaces it covers (features table, quick start, dedicated sections, CLI subcommands, requirements, data-storage layout, test count).

Update it when the change:

- Adds or removes a feature substantial enough to belong in the **features table** (new screen, new subcommand, new major flow). Don't add a row for every small addition — match the existing bar.
- Adds or changes a **CLI subcommand**, keybinding mentioned in Quick start, or a documented section (e.g. Config editor, Local trace analysis).
- Changes **requirements**, install flow, data-storage layout, or another fact the README states.
- Bumps a stat the README quotes (e.g. test count).

Skip the README when the change is a bug fix, internal refactor, dependency bump, or a small addition that doesn't rise to the README's level of signal — the changelog is enough. When in doubt, leave it alone; README updates are higher-signal than changelog and shouldn't accumulate noise.

Style rules (match the existing file):

- Mirror the surrounding format — feature table rows use an emoji + bold name + one-line description; sections use the same heading depth and tone as their neighbors.
- Don't duplicate every changelog bullet. The README captures *what exists*; the changelog captures *what changed*.
- Use backticks for code identifiers, keybindings, file paths, and subcommand names.

### 4. Commit

- Stage `CHANGELOG.md`, `README.md` (if touched), plus any uncommitted code changes that belong with this ship. Add files by name; never `git add -A` / `git add .`.
- If there are already commits on the branch and the only new changes are docs, make a single `docs: update changelog` (or `docs: update changelog and README`) commit.
- Otherwise, bundle code + docs into one commit with a conventional message that matches the repo's style (see `git log` — short imperative subject, optional body explaining *why*).
- HEREDOC for the message, ending with the `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>` trailer.
- Never `--amend`, never `--no-verify`. If a pre-commit hook fails, fix the underlying issue and create a new commit.

### 5. Push

- If the branch has no upstream, `git push -u origin HEAD`.
- Otherwise `git push`. Never force-push from this command — if the push is rejected, stop and tell the user.

### 6. Open the PR

Use `gh pr create`. If a PR already exists for this branch (`gh pr view --json url 2>/dev/null`), skip creation and just report the existing URL.

- Title: short (<70 chars), imperative, matches commit style. Use `$ARGUMENTS` as a hint if given.
- Body (HEREDOC): a `## Summary` section (1–3 bullets covering the *user-visible* change, mirroring the changelog entries) and a `## Test plan` section (markdown checklist of what the reviewer / you should verify — at minimum `cargo build` and `cargo test`, plus any UI flows you touched).
- End the body with the `🤖 Generated with [Claude Code](https://claude.com/claude-code)` footer.

### 7. Report

Print the PR URL and a one-line summary of what shipped. Nothing else.

## Notes

- **Don't run `cargo test` / `cargo build` as part of this command** — the user will have done that already, and shipit is a packaging step, not a validation step. If the commit hook runs them, fine.
- **Respect the "no surrounding refactors" rule from `CLAUDE.md`.** If you notice unrelated issues while reading the diff, mention them in the final report but don't fix them here.
- **Don't update version numbers in `Cargo.toml`** — releases are tag-driven via cargo-dist.
