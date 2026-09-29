# Changelog

Notable changes are recorded here, following [Keep a
Changelog](https://keepachangelog.com/en/1.1.0/) and [Semantic
Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Linked worktrees get their own row, wherever they are checked out, named after the main repo
  and the branch they have checked out (`repo [branch]`). The row holds the worktree's
  uncommitted changes and that branch's unpushed commits; other branches stay on the main repo's
  row.

### Changed

- GitHub actions bumped off the Node 20 runtime.

### Fixed

- A nested repo, such as a worktree under `.claude/worktrees`, no longer shows as an untracked
  `+0 -0` file of the repo around it, and `add --all` no longer stages it as an embedded gitlink.

## [0.1.0] 2026-09-09

First version.

### Added

- Table of every repo under a directory holding uncommitted changes or unpushed branches, within
  a time window (`--since 30m|24h|7d|2w`, 24 hours by default).
- Parallel scan: the tree is walked once, then each repo is inspected with three git calls. About
  half a second for 85 repos.
- Deleted files are dated from their nearest surviving parent directory, since a removed file has
  no mtime of its own.
- Keyboard browser on the table: `z`/`s` or arrows to move, `space` to select, `a` to select
  everything, `d` to open a repo then to show a file diff or a branch log through your own git
  pager, `enter` to act, `q` to go back or quit.
- Commit and push from the browser: optional new branch, per repo message, `--force-with-lease`
  and `--no-verify` offered as prompts, and partial staging from the files you selected.
- A preview of the files, branches and commits an action would touch, printed before the action
  menu.
- Layout that fits the terminal: names trimmed on the side that carries no meaning, `LAST CHANGE`
  dropped when the other columns can no longer hold a usable width, and an ANSI aware clamp so a
  colour code is never sliced in half.
- Every git command is printed before it runs, and git keeps the real terminal, so credential
  prompts, hooks and pagers behave normally.
