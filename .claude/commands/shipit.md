# Ship It

Update `CHANGELOG.md`, commit, push, and open a pull request for the pending changes on the current branch.

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

### 3. Commit

- Stage `CHANGELOG.md` plus any uncommitted code changes that belong with this ship. Add files by name; never `git add -A` / `git add .`.
- If there are already commits on the branch and the only new change is the changelog, make a single `docs: update changelog` commit.
- Otherwise, bundle code + changelog into one commit with a conventional message that matches the repo's style (see `git log` — short imperative subject, optional body explaining *why*).
- HEREDOC for the message, ending with the `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>` trailer.
- Never `--amend`, never `--no-verify`. If a pre-commit hook fails, fix the underlying issue and create a new commit.

### 4. Push

- If the branch has no upstream, `git push -u origin HEAD`.
- Otherwise `git push`. Never force-push from this command — if the push is rejected, stop and tell the user.

### 5. Open the PR

Use `gh pr create`. If a PR already exists for this branch (`gh pr view --json url 2>/dev/null`), skip creation and just report the existing URL.

- Title: short (<70 chars), imperative, matches commit style. Use `$ARGUMENTS` as a hint if given.
- Body (HEREDOC): a `## Summary` section (1–3 bullets covering the *user-visible* change, mirroring the changelog entries) and a `## Test plan` section (markdown checklist of what the reviewer / you should verify — at minimum `cargo build` and `cargo test`, plus any UI flows you touched).
- End the body with the `🤖 Generated with [Claude Code](https://claude.com/claude-code)` footer.

### 6. Report

Print the PR URL and a one-line summary of what shipped. Nothing else.

## Notes

- **Don't run `cargo test` / `cargo build` as part of this command** — the user will have done that already, and shipit is a packaging step, not a validation step. If the commit hook runs them, fine.
- **Respect the "no surrounding refactors" rule from `CLAUDE.md`.** If you notice unrelated issues while reading the diff, mention them in the final report but don't fix them here.
- **Don't update version numbers in `Cargo.toml`** — releases are tag-driven via cargo-dist.
