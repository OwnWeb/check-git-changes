# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Single-binary Rust CLI, one dependency (`rayon`). README covers usage, flags and limits: read it
before changing behaviour. This file is the parts the README does not say.

## Commands

```sh
make build | make test | make install    # cargo build --release | cargo test | cargo install --path .
cargo test parses_periods                # single test, matched by name
cargo run -- ~/dev --since 7d -n         # run against a real tree, table only
```

CI runs `cargo test` alone on ubuntu and macos: no clippy or fmt gate, no `rustfmt.toml`, and
`rustfmt` is not installed here. Match surrounding style by hand. A `v*` release tag must match the
`Cargo.toml` version or the workflow fails.

## Architecture

`main.rs` scan + table, `interactive.rs` browser + git writes, `term.rs` raw mode, keys, redraw.

`main()`: `collect_repos` walks sequentially and stops at the first `.git`, then `rayon` runs
`scan_repo` per repo (3 git calls each: `diff --numstat HEAD`, `ls-files --others`, `for-each-ref`).
A fourth call multiplies across every repo.

Two git paths, on purpose. Reads go through `git()` (`main.rs:248`), which captures stdout and
returns `None` on non-zero exit. Writes go through `run_git()` (`interactive.rs:700`), which prints
`$ git -C …` and uses `.status()` inside `RawTerminal::suspended()` so git inherits the real
terminal: credential prompts, hooks and pagers keep working.

Window filtering is per file, by mtime; a deleted file is dated from its nearest surviving ancestor
directory (`change_time`, `main.rs:315`).

`Cell` carries `plain` and `colored` side by side and every width reads `plain`, which is what keeps
ANSI out of the layout maths. `Screen::repaint` rewinds with `\x1b[{n}A` on the normal screen
buffer, so a wrapped line would break the redraw: nothing may exceed the width.

`Browser.reports` and `Browser.selections` are index aligned. Every mutation keeps them so
(`forget_clean_repos` unzips them together).

No `clap`, no `chrono`, no `crossterm`: args, `civil_from_days` and `stty` are hand-rolled. Keep it.

## Conventions

Git commands are built by pure functions returning `Vec<String>` (`push_args`, `branch_range_args`,
`diff_file_args`, `stage_args`), which is the only reason they are testable. Add new invocations the
same way and test the argument vector, not a real repo.

Tests are in-module `#[cfg(test)] mod tests`, pure functions only, no `tests/` directory.

Numeric literals are named `const`s at the top of the module. A comment on a deliberate shortcut
names its ceiling and how to lift it (`term.rs:82`).

`interactive.rs` imports crate-private items from the root; `main.rs` items stay non-`pub`.

Behaviour change means a README table update and a `CHANGELOG.md` `[Unreleased]` entry.
