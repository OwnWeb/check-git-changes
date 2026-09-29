# cgc

[![CI](https://github.com/OwnWeb/check-git-changes/actions/workflows/ci.yml/badge.svg)](https://github.com/OwnWeb/check-git-changes/actions/workflows/ci.yml)

Shows, across every git repo under a directory: the changes you have not committed, and the
branches you have not pushed. Both filtered to a time window, 24 hours by default.

Built for the "what did I touch today, and what did I forget to push?" question when your work
is spread over dozens of repos. Scans 85 repos in about half a second: repos are walked once,
then each is inspected in parallel with three git calls.

```
REPO                               CHANGES          UNPUSHED                      LAST CHANGE
orgA/repoA                         2 files  +4 -1   -                             2026-09-08 18:46
orgA/repoB                         -                sync/upstream ahead 5         2026-09-08 18:28
orgB/repoC                         1 file  +1 -0    -                             2026-09-08 16:50
orgB/team/repoD-analytics-worker   -                feat/search ahead 2 (new)     2026-09-07 17:46
orgC/repoE                         1 file  +1 -0    chore/deps ahead 1, +1 more   2026-09-03 14:06

5 repo(s) with activity in the last 30d out of 5 scanned: 4 file(s), 4 unpushed branch(es), 49ms
```

## Requirements

* Rust toolchain. Built and tested with 1.95. `std::io::IsTerminal` puts the floor at 1.70;
  older toolchains are untested.
* `git` in `PATH`
* macOS or Linux. The interactive mode drives the terminal through `stty`, so Windows is out.

## Install

Download a binary from the [latest release](https://github.com/OwnWeb/check-git-changes/releases/latest):

```sh
# macOS on Apple silicon; swap for x86_64-apple-darwin or x86_64-unknown-linux-musl
curl -fsSL https://github.com/OwnWeb/check-git-changes/releases/latest/download/cgc-v0.1.0-aarch64-apple-darwin.tar.gz | tar -xz
mv cgc /usr/local/bin/
```

The Linux build is statically linked against musl, so it does not care about the host glibc.

### From source

Build the binary and copy it into `~/.cargo/bin`:

```sh
make install
```

Which is `cargo install --path .`, if you would rather skip make. Other targets: `make build`,
`make test`, `make clean`.

Re-run the same command after pulling changes: it replaces the installed binary.

Prefer to keep the binary in the project? Build it and copy it wherever you like:

```sh
cargo build --release
# binary at ./target/release/cgc, around 600 KB, no runtime dependency beyond git
cp target/release/cgc /usr/local/bin/
```

Tests:

```sh
make test
```

## Usage

```sh
cgc [DIR] [--since PERIOD] [--no-input]
```

| Option | Meaning |
| --- | --- |
| `DIR` | Directory to scan recursively. Defaults to the current directory. |
| `--since PERIOD`, `-s` | How far back to look: `30m`, `24h`, `7d`, `2w`. A bare number means hours. Defaults to `24h`. |
| `--no-input`, `-n` | Print the table and exit, skipping the interactive browser. |
| `--help`, `-h` | Usage. |

Examples:

```sh
cgc ~/dev                 # last 24 hours
cgc ~/dev --since 7d      # last week
cgc ~/dev -n | pbcopy     # table only, no keyboard mode
```

The browser needs a terminal on stdin, so piping or redirecting input skips it automatically:
`--no-input` is only needed when you want the plain table in an interactive shell.

## Interactive mode

Once the table is drawn it becomes a live list. Nothing is written to any repo until you pick an
action and confirm the prompts.

```
        REPO                           CHANGES          UNPUSHED                    LAST CHANGE
  [ ]   orgA/repoA                     2 files  +4 -1   -                           2026-09-08 18:46
> [x]   orgA/repoB                     -                sync/upstream ahead 5       2026-09-08 18:28
  [ ]   orgB/repoC                     1 file  +1 -0    -                           2026-09-08 16:50
  [ ]   …team/repoD-analytics-worker   -                feat/search ahead 2 (new)   2026-09-07 17:46
  [ ]   orgC/repoE                     1 file  +1 -0    chore/deps ahead 1, +1 m…   2026-09-03 14:06

5 repo(s) with activity in the last 30d out of 5 scanned: 4 file(s), 4 unpushed branch(es), 49ms
z/s move   space select   a select all   d open   enter act   q quit
```

| Key | In the repo list | Inside a repo |
| --- | --- | --- |
| `z` / `s`, or up / down | Move the cursor | Move the cursor |
| `space` | Select the repo | Select a file or a branch |
| `a` | Select or clear every repo | |
| `d`, or right | Open the repo | Show the diff of a file, the whole diff on `all changes`, or the unpushed commits of a branch |
| `enter` | Act on the selection | Act on this repo |
| `q`, or left | Quit | Back to the list |
| `ctrl-c` | Quit | Quit |

Diffs and commit logs go through your own git pager, so a `delta` or `less` setup keeps working.

The table fits itself to the terminal, because a wrapped line would break the in place redraw.
Repo names lose their head (`…team/repoD-analytics-worker`), since the tail is what tells two
repos apart; branch names lose their tail. When the two cannot hold a usable width any more, the
`LAST CHANGE` column is dropped, then the summary switches to a short form and the key list wraps
onto a second line. Redirected output is never truncated: only a terminal has an edge to respect.

`enter` first prints what the action would touch, then offers the action:

```
orgC/repoE
  1 file(s) [add --all]
    ? deploy.yml  +1 -0
  chore/deps ahead 1 (origin/chore/deps)
    5b755b1 2026-09-03 14:06 chore: bump dependencies
  fix/request-timeouts ahead 4 (new)
    3ec342d 2026-09-03 12:38 fix: timeout step 3
    5135fe7 2026-09-03 12:38 fix: timeout step 2
    167f6a2 2026-09-03 12:38 fix: timeout step 1
    (1 more)

action: [p] push branches  [c] commit  [b] commit on new branch  [q] cancel >
```

* `p` pushes the selected branches, or every unpushed branch of the selected repos. It then asks
  for optional flags: `l` for `--force-with-lease`, `n` for `--no-verify`, `ln` for both.
* `c` commits, `b` creates a branch first. Each repo asks for its own message, shows
  `git status --short`, and offers to push afterwards.
* Selected files are staged on their own (`git add -- file`), which is how you commit part of a
  repo. With nothing selected the commit stages everything (`git add --all`) except nested repos,
  which would otherwise land as embedded gitlinks, and the preview says which of the two applies.

Every git command is printed before it runs, and git keeps the real terminal, so credential
prompts, hooks and progress output behave normally. A repo that becomes clean after an action
leaves the list. On exit the table is printed once more without the cursor and checkboxes, so it
stays in your scrollback.

## What counts as a change

**Uncommitted**: everything `git diff HEAD` reports plus untracked files that git does not
ignore, kept only if the file was modified inside the window. A deleted file has no mtime of its
own, so its nearest surviving parent directory is used instead: removing an entry updates the
mtime of the directory that held it.

**Worktrees**: each linked worktree of a repo gets its own row, found through the repo's
`.git/worktrees`, so one checked out under `.claude/worktrees` or next to the repo shows up too.
The row is named after the main repo and the branch the worktree has checked out
(`ownweb/claude [OW-701-skill-owasp-mobile]`), or its directory when HEAD is detached. It holds
the worktree's uncommitted changes and that branch's unpushed commits; every other branch stays on
the main repo's row. A nested repo is not a change of the repo around it, so its directory is left
out of that repo's untracked files.

**Unpushed**: a local branch whose last commit falls inside the window and which holds commits
its remote does not. With an upstream, the count comes from `%(upstream:track)`. Without one, from
`git rev-list --count <branch> --not --remotes`, so a branch that was never pushed but exists on
some remote does not show up.

Pushing uses the exact refspec (`local:refs/heads/remote`), so a local branch mapped to a
differently named remote branch goes where it should. A branch with no upstream is pushed with
`--set-upstream` to `origin`, or to the only remote when `origin` does not exist.

## Limits

* `node_modules`, `vendor`, `target`, `venv`, `dist`, `build`, `Pods`, `bower_components`,
  `__pycache__` and dotted directories are not walked. Ignored files never count, since untracked
  files come from `git ls-files --others --exclude-standard`.
* Untracked files over 1 MiB count as a changed file but contribute no line count.
* Binary files count as a changed file with no lines, as git reports no numbers for them.
* Submodules and nested clones are not followed: the walk stops at the first `.git` it finds.
  Linked worktrees are the exception.
* A worktree scanned without its main repo (DIR covers the worktree only) shows its own branch,
  not the repo's other ones.
* Below roughly 50 columns the repo column gets short enough to be ambiguous. Nothing wraps or
  garbles, but a wider terminal is easier to read.
* No stash inspection, no hunk staging, no amend.

## License

MIT. See [LICENSE](LICENSE).
