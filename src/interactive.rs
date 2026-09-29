use crate::term::{self, Key, RawTerminal, Screen};
use crate::{
    build_table, display_path, format_local, git, local_offset_seconds, repo_label, scan_repo,
    summary_line, Cell, ChangedFile, Palette, RepoReport, ScanContext, UnpushedBranch,
    WorkingTreeChanges, WorktreeReport,
};
use std::io::Write;
use std::path::Path;
use std::process::Command;

const HELP_REPOS: &[&str] =
    &["z/s move", "space select", "a select all", "d open", "enter act", "q quit"];
const HELP_DETAIL: &[&str] =
    &["z/s move", "space select", "d diff", "enter act", "q back"];
const HELP_SEPARATOR: &str = "   ";
const ACTION_MENU: &str =
    "action: [p] push branches  [c] commit  [b] commit on new branch  [q] cancel > ";
const FLAGS_HELP: &str =
    "push flags? (l = --force-with-lease, n = --no-verify, ln = both, empty = none) > ";
const FLAGS_REJECTED: &str = "expected l, n, ln or nothing";
const ROWS_RESERVED_FOR_CHROME: usize = 4;
const FILES_PREVIEW_LIMIT: usize = 10;
/// Cursor, checkbox, status letter and the gaps around them, before the file path.
const DETAIL_ROW_CHROME_WIDTH: usize = 10;
const COMMITS_PREVIEW_LIMIT: usize = 3;
const COMMIT_PREVIEW_FORMAT: &str = "--format=%h %ad %s";
const COMMIT_PREVIEW_DATE: &str = "--date=format-local:%Y-%m-%d %H:%M";

struct PushOptions {
    force_with_lease: bool,
    no_verify: bool,
}

#[derive(Clone, Copy)]
enum DetailRow {
    AllChanges,
    File(usize),
    Worktree(usize),
    Branch(usize),
}

struct Selection {
    repo: bool,
    files: Vec<bool>,
    worktrees: Vec<bool>,
    branches: Vec<bool>,
}

impl Selection {
    fn for_report(report: &RepoReport) -> Self {
        Self {
            repo: false,
            files: vec![false; report.changes.files.len()],
            worktrees: vec![false; report.worktrees.len()],
            branches: vec![false; report.unpushed_branches.len()],
        }
    }
}

/// The checkouts a commit goes through, each asking for its own message.
struct CommitScope {
    main_checkout: bool,
    worktrees: Vec<usize>,
}

struct Browser {
    reports: Vec<RepoReport>,
    selections: Vec<Selection>,
    context: ScanContext,
    offset: i64,
    palette: Palette,
    cursor: usize,
    detail: Option<(usize, usize)>,
    screen: Screen,
}

pub fn run(reports: Vec<RepoReport>, context: ScanContext) {
    let Some(terminal) = RawTerminal::acquire() else {
        crate::report(&reports, &context);
        return;
    };
    let selections = reports.iter().map(Selection::for_report).collect();
    let mut browser = Browser {
        reports,
        selections,
        context,
        offset: local_offset_seconds(),
        palette: Palette::detect(),
        cursor: 0,
        detail: None,
        screen: Screen::new(),
    };
    browser.browse(&terminal);
}

impl Browser {
    fn browse(&mut self, terminal: &RawTerminal) {
        loop {
            if self.reports.is_empty() {
                break;
            }
            self.paint();
            let Some(key) = term::read_key() else {
                break;
            };
            let keep_browsing = match self.detail {
                None => self.handle_list_key(key, terminal),
                Some((repo, _)) => self.handle_detail_key(key, terminal, repo),
            };
            if !keep_browsing {
                break;
            }
        }
        self.screen.clear();
        crate::report(&self.reports, &self.context);
    }

    fn handle_list_key(&mut self, key: Key, terminal: &RawTerminal) -> bool {
        match key {
            Key::Up | Key::Char('z') => self.cursor = self.cursor.saturating_sub(1),
            Key::Down | Key::Char('s') => {
                self.cursor = (self.cursor + 1).min(self.reports.len() - 1)
            }
            Key::Select => self.selections[self.cursor].repo = !self.selections[self.cursor].repo,
            Key::Char('a') => self.toggle_all(),
            Key::Right | Key::Char('d') => self.detail = Some((self.cursor, 0)),
            Key::Act => self.act(terminal, self.action_targets()),
            Key::Left | Key::Char('q') | Key::Interrupt => return false,
            _ => {}
        }
        true
    }

    fn handle_detail_key(&mut self, key: Key, terminal: &RawTerminal, repo: usize) -> bool {
        let rows = self.detail_rows(repo);
        let cursor = self.detail.map(|(_, cursor)| cursor).unwrap_or(0).min(rows.len() - 1);
        match key {
            Key::Up | Key::Char('z') => self.detail = Some((repo, cursor.saturating_sub(1))),
            Key::Down | Key::Char('s') => {
                self.detail = Some((repo, (cursor + 1).min(rows.len() - 1)))
            }
            Key::Select => self.toggle_detail_row(repo, rows[cursor]),
            Key::Right | Key::Char('d') => self.show_diff(terminal, repo, rows[cursor]),
            Key::Act => self.act(terminal, vec![repo]),
            Key::Left | Key::Char('q') => self.detail = None,
            Key::Interrupt => return false,
            _ => {}
        }
        true
    }

    fn toggle_all(&mut self) {
        let select_everything = self.selections.iter().any(|selection| !selection.repo);
        for selection in &mut self.selections {
            selection.repo = select_everything;
        }
    }

    fn toggle_detail_row(&mut self, repo: usize, row: DetailRow) {
        let selection = &mut self.selections[repo];
        match row {
            DetailRow::AllChanges => selection.repo = !selection.repo,
            DetailRow::File(index) => selection.files[index] = !selection.files[index],
            DetailRow::Worktree(index) => selection.worktrees[index] = !selection.worktrees[index],
            DetailRow::Branch(index) => selection.branches[index] = !selection.branches[index],
        }
    }

    fn action_targets(&self) -> Vec<usize> {
        let selected: Vec<usize> = self
            .selections
            .iter()
            .enumerate()
            .filter(|(_, selection)| selection.repo)
            .map(|(index, _)| index)
            .collect();
        if selected.is_empty() {
            vec![self.cursor]
        } else {
            selected
        }
    }

    fn detail_rows(&self, repo: usize) -> Vec<DetailRow> {
        let report = &self.reports[repo];
        let mut rows = Vec::new();
        if !report.changes.files.is_empty() {
            rows.push(DetailRow::AllChanges);
            rows.extend((0..report.changes.files.len()).map(DetailRow::File));
        }
        rows.extend((0..report.worktrees.len()).map(DetailRow::Worktree));
        rows.extend((0..report.unpushed_branches.len()).map(DetailRow::Branch));
        if rows.is_empty() {
            rows.push(DetailRow::AllChanges);
        }
        rows
    }

    fn paint(&mut self) {
        let (rows, columns) = term::terminal_size();
        let help = self.help_lines(columns);
        let capacity = rows.saturating_sub(ROWS_RESERVED_FOR_CHROME + help.len());
        let mut frame = match self.detail {
            None => self.list_lines(capacity),
            Some((repo, cursor)) => self.detail_lines(repo, cursor, capacity, columns),
        };
        frame.push(String::new());
        frame.push(summary_line(&self.reports, &self.context, &self.palette));
        frame.extend(help);
        self.screen.repaint(&frame, columns);
    }

    /// Keeps every key visible on a narrow terminal instead of letting the clamp eat them.
    fn help_lines(&self, width: usize) -> Vec<String> {
        let parts = if self.detail.is_some() { HELP_DETAIL } else { HELP_REPOS };
        let mut lines = Vec::new();
        let mut current = String::new();
        for part in parts {
            let candidate = if current.is_empty() {
                (*part).to_string()
            } else {
                format!("{current}{HELP_SEPARATOR}{part}")
            };
            if !current.is_empty() && candidate.chars().count() > width {
                lines.push(std::mem::replace(&mut current, (*part).to_string()));
            } else {
                current = candidate;
            }
        }
        lines.push(current);
        lines
            .into_iter()
            .map(|line| format!("{}{}{}", self.palette.dim, line, self.palette.reset))
            .collect()
    }

    fn list_lines(&self, capacity: usize) -> Vec<String> {
        let prefixes: Vec<Cell> = (0..self.reports.len()).map(|index| self.prefix(index)).collect();
        let table =
            build_table(&self.reports, &self.context, &self.palette, self.offset, Some(&prefixes));
        let window = term::visible_window(self.reports.len(), self.cursor, capacity - 1);
        let mut lines = vec![format!(
            "{}{}{}",
            self.palette.dim,
            table.header_line(),
            self.palette.reset
        )];
        lines.extend(window.map(|index| table.row_line(index)));
        lines
    }

    fn prefix(&self, index: usize) -> Cell {
        let marker = self.cursor_marker(index == self.cursor);
        let checkbox = self.checkbox(self.selections[index].repo);
        Cell::colored(
            format!("{} {}", if index == self.cursor { ">" } else { " " }, checkbox),
            format!("{marker} {checkbox}"),
        )
    }

    fn summarize(&self, report: &RepoReport) -> String {
        let mut parts = Vec::new();
        if !report.changes.files.is_empty() {
            parts.push(self.describe_changes(&report.changes));
        }
        if !report.worktrees.is_empty() {
            parts.push(format!(
                "{}{} worktree(s) with changes{}",
                self.palette.yellow,
                report.worktrees.len(),
                self.palette.reset
            ));
        }
        if !report.unpushed_branches.is_empty() {
            parts.push(format!(
                "{}{} branch(es) unpushed{}",
                self.palette.yellow,
                report.unpushed_branches.len(),
                self.palette.reset
            ));
        }
        parts.join("  ")
    }

    fn describe_changes(&self, changes: &WorkingTreeChanges) -> String {
        format!(
            "{} file(s) {}+{}{} {}-{}{}",
            changes.files.len(),
            self.palette.green,
            changes.added_lines(),
            self.palette.reset,
            self.palette.red,
            changes.removed_lines(),
            self.palette.reset
        )
    }

    fn detail_lines(
        &self,
        repo: usize,
        cursor: usize,
        capacity: usize,
        columns: usize,
    ) -> Vec<String> {
        let report = &self.reports[repo];
        let rows = self.detail_rows(repo);
        let cursor = cursor.min(rows.len() - 1);
        let window = term::visible_window(rows.len(), cursor, capacity.saturating_sub(1));
        let header = format!(
            "{}{}{}",
            self.palette.bold,
            repo_label(&report.path, &self.context.root),
            self.palette.reset
        );
        let mut lines = vec![header];
        lines.extend(window.map(|index| {
            let selection = &self.selections[repo];
            let (checkbox, text) = match rows[index] {
                DetailRow::AllChanges => (selection.repo, self.summarize(report)),
                DetailRow::File(file) => (
                    selection.files[file],
                    self.describe_file(&report.changes.files[file], columns),
                ),
                DetailRow::Worktree(worktree) => (
                    selection.worktrees[worktree],
                    self.describe_worktree_row(&report.worktrees[worktree]),
                ),
                DetailRow::Branch(branch) => (
                    selection.branches[branch],
                    self.describe_branch_row(&report.unpushed_branches[branch]),
                ),
            };
            format!(
                "{} {} {}",
                self.cursor_marker(index == cursor),
                self.checkbox(checkbox),
                text
            )
        }));
        lines
    }

    fn describe_file(&self, file: &ChangedFile, columns: usize) -> String {
        let counts = format!("+{} -{}", file.added_lines, file.removed_lines);
        let room = columns
            .saturating_sub(DETAIL_ROW_CHROME_WIDTH + counts.chars().count());
        format!(
            "{} {}  {}+{}{} {}-{}{}",
            file.status(),
            crate::truncate_start(&file.path, room),
            self.palette.green,
            file.added_lines,
            self.palette.reset,
            self.palette.red,
            file.removed_lines,
            self.palette.reset
        )
    }

    fn describe_worktree_row(&self, worktree: &WorktreeReport) -> String {
        format!(
            "{}{}{} worktree  {}  {}{}{}",
            self.palette.yellow,
            worktree.name(),
            self.palette.reset,
            self.describe_changes(&worktree.changes),
            self.palette.dim,
            display_path(&worktree.path),
            self.palette.reset
        )
    }

    fn describe_branch_row(&self, branch: &UnpushedBranch) -> String {
        let upstream = match &branch.upstream {
            Some(upstream) => format!("{}/{}", upstream.remote, branch.name),
            None => "new".to_string(),
        };
        format!(
            "{}{}{} ahead {} ({})  {}{}{}",
            self.palette.yellow,
            branch.name,
            self.palette.reset,
            branch.ahead,
            upstream,
            self.palette.dim,
            format_local(branch.last_commit, self.offset),
            self.palette.reset
        )
    }

    fn cursor_marker(&self, active: bool) -> String {
        if active {
            format!("{}>{}", self.palette.bold, self.palette.reset)
        } else {
            " ".to_string()
        }
    }

    fn checkbox(&self, selected: bool) -> &'static str {
        if selected {
            "[x]"
        } else {
            "[ ]"
        }
    }

    fn show_diff(&mut self, terminal: &RawTerminal, repo: usize, row: DetailRow) {
        let report = &self.reports[repo];
        let path = match row {
            DetailRow::Worktree(index) => report.worktrees[index].path.clone(),
            _ => report.path.clone(),
        };
        let args = self.diff_args(repo, row);
        self.screen.clear();
        terminal.suspended(|| {
            // git spawns the user's own pager, which restores the screen when it exits.
            let _ = Command::new("git").arg("-C").arg(&path).args(&args).status();
        });
    }

    fn diff_args(&self, repo: usize, row: DetailRow) -> Vec<String> {
        let report = &self.reports[repo];
        match row {
            DetailRow::AllChanges | DetailRow::Worktree(_) => {
                vec!["diff".to_string(), "HEAD".to_string()]
            }
            DetailRow::File(index) => diff_file_args(&report.changes.files[index]),
            DetailRow::Branch(index) => log_branch_args(&report.unpushed_branches[index]),
        }
    }

    fn act(&mut self, terminal: &RawTerminal, targets: Vec<usize>) {
        self.screen.clear();
        terminal.suspended(|| self.preview(&targets));
        print!("{ACTION_MENU}");
        let _ = std::io::stdout().flush();
        let action = term::read_key();
        print!("\r\n");
        let done = match action {
            Some(Key::Char('p')) => {
                terminal.suspended(|| self.push_targets(&targets));
                true
            }
            Some(Key::Char('c')) => {
                terminal.suspended(|| self.commit_targets(&targets, false));
                true
            }
            Some(Key::Char('b')) => {
                terminal.suspended(|| self.commit_targets(&targets, true));
                true
            }
            _ => false,
        };
        if done {
            self.rescan(&targets);
        }
    }

    fn push_targets(&self, targets: &[usize]) {
        let options = ask_push_options();
        for &target in targets {
            let report = &self.reports[target];
            for branch in self.branches_to_push(target) {
                push_branch(&report.path, branch, &options);
            }
        }
    }

    fn branches_to_push(&self, target: usize) -> Vec<&UnpushedBranch> {
        push_scope(&self.reports[target], &self.selections[target])
    }

    fn commit_targets(&self, targets: &[usize], on_new_branch: bool) {
        for &target in targets {
            self.commit_repo(target, on_new_branch);
        }
    }

    fn commit_repo(&self, target: usize, on_new_branch: bool) {
        let report = &self.reports[target];
        let scope = commit_scope(report, &self.selections[target]);
        if scope.main_checkout {
            commit_checkout(&report.path, &self.stage_args(target), on_new_branch);
        }
        for &index in &scope.worktrees {
            let worktree = &report.worktrees[index];
            let stage_args = stage_all_args(&worktree.changes.nested_repos);
            commit_checkout(&worktree.path, &stage_args, on_new_branch);
        }
    }

    fn stage_args(&self, target: usize) -> Vec<String> {
        if !self.has_selected_files(target) {
            return stage_all_args(&self.reports[target].changes.nested_repos);
        }
        let mut args = vec!["add".to_string(), "--".to_string()];
        args.extend(self.files_to_commit(target).iter().map(|file| file.path.clone()));
        args
    }

    /// What the next action would touch, printed before the action menu.
    fn preview(&self, targets: &[usize]) {
        for &target in targets {
            let report = &self.reports[target];
            println!("\n{}", repo_label(&report.path, &self.context.root));
            let scope = commit_scope(report, &self.selections[target]);
            if scope.main_checkout {
                self.preview_files(target);
            }
            for &index in &scope.worktrees {
                self.preview_worktree(&report.worktrees[index]);
            }
            self.preview_branches(target);
        }
    }

    fn preview_files(&self, target: usize) {
        let staging = if self.has_selected_files(target) { "selected" } else { "add --all" };
        self.print_files(&self.files_to_commit(target), staging);
    }

    fn preview_worktree(&self, worktree: &WorktreeReport) {
        println!(
            "  {}{}{} worktree  {}{}{}",
            self.palette.yellow,
            worktree.name(),
            self.palette.reset,
            self.palette.dim,
            display_path(&worktree.path),
            self.palette.reset
        );
        let files: Vec<&ChangedFile> = worktree.changes.files.iter().collect();
        self.print_files(&files, "add --all");
    }

    fn print_files(&self, files: &[&ChangedFile], staging: &str) {
        if files.is_empty() {
            return;
        }
        println!("  {} file(s) [{}]", files.len(), staging);
        for file in files.iter().take(FILES_PREVIEW_LIMIT) {
            println!(
                "    {} {}  {}+{}{} {}-{}{}",
                file.status(),
                file.path,
                self.palette.green,
                file.added_lines,
                self.palette.reset,
                self.palette.red,
                file.removed_lines,
                self.palette.reset
            );
        }
        self.print_hidden_count(files.len().saturating_sub(FILES_PREVIEW_LIMIT));
    }

    fn preview_branches(&self, target: usize) {
        let repo = &self.reports[target].path;
        for branch in self.branches_to_push(target) {
            println!(
                "  {}{}{}",
                self.palette.yellow,
                self.plain_branch_label(branch),
                self.palette.reset
            );
            let subjects = commit_subjects(repo, branch);
            for subject in &subjects {
                println!("    {}{}{}", self.palette.dim, subject, self.palette.reset);
            }
            self.print_hidden_count(branch.ahead.saturating_sub(subjects.len() as u64) as usize);
        }
    }

    fn print_hidden_count(&self, hidden: usize) {
        if hidden == 0 {
            return;
        }
        println!("    {}({hidden} more){}", self.palette.dim, self.palette.reset);
    }

    fn plain_branch_label(&self, branch: &UnpushedBranch) -> String {
        let target = match &branch.upstream {
            Some(upstream) => format!("{}/{}", upstream.remote, branch.name),
            None => "new".to_string(),
        };
        format!("{} ahead {} ({})", branch.name, branch.ahead, target)
    }

    fn has_selected_files(&self, target: usize) -> bool {
        self.selections[target].files.iter().any(|selected| *selected)
    }

    fn files_to_commit(&self, target: usize) -> Vec<&ChangedFile> {
        let files = &self.reports[target].changes.files;
        if !self.has_selected_files(target) {
            return files.iter().collect();
        }
        files
            .iter()
            .zip(&self.selections[target].files)
            .filter(|(_, selected)| **selected)
            .map(|(file, _)| file)
            .collect()
    }

    fn rescan(&mut self, targets: &[usize]) {
        for &target in targets {
            self.reports[target] = scan_repo(&self.reports[target].path, self.context.cutoff);
            self.selections[target] = Selection::for_report(&self.reports[target]);
        }
        self.forget_clean_repos();
        self.detail = None;
    }

    fn forget_clean_repos(&mut self) {
        let (reports, selections) = std::mem::take(&mut self.reports)
            .into_iter()
            .zip(std::mem::take(&mut self.selections))
            .filter(|(report, _)| !report.is_clean())
            .unzip();
        self.reports = reports;
        self.selections = selections;
        self.cursor = self.cursor.min(self.reports.len().saturating_sub(1));
    }
}

/// With nothing selected a commit goes through every checkout showing changes, otherwise
/// through the selected files and worktrees only.
fn commit_scope(report: &RepoReport, selection: &Selection) -> CommitScope {
    let files_selected = selection.files.contains(&true);
    if !files_selected && !selection.worktrees.contains(&true) {
        return CommitScope {
            main_checkout: !report.changes.files.is_empty(),
            worktrees: (0..report.worktrees.len()).collect(),
        };
    }
    let selected_worktrees = selection
        .worktrees
        .iter()
        .enumerate()
        .filter(|(_, selected)| **selected)
        .map(|(index, _)| index)
        .collect();
    CommitScope { main_checkout: files_selected, worktrees: selected_worktrees }
}

/// With nothing selected a push covers every unpushed branch, otherwise the selected branches
/// and the ones the selected worktrees have checked out.
fn push_scope<'a>(report: &'a RepoReport, selection: &Selection) -> Vec<&'a UnpushedBranch> {
    let nothing_selected =
        !selection.branches.contains(&true) && !selection.worktrees.contains(&true);
    let held_by_selected_worktree = |branch: &UnpushedBranch| {
        report
            .worktrees
            .iter()
            .zip(&selection.worktrees)
            .any(|(worktree, selected)| *selected && worktree.holds(branch))
    };
    report
        .unpushed_branches
        .iter()
        .zip(&selection.branches)
        .filter(|(branch, selected)| {
            nothing_selected || **selected || held_by_selected_worktree(branch)
        })
        .map(|(branch, _)| branch)
        .collect()
}

fn commit_checkout(repo: &Path, stage_args: &[String], on_new_branch: bool) {
    println!("\n{}", display_path(repo));
    print!("{}", git(repo, &["status", "--short"]).unwrap_or_default());

    if on_new_branch {
        let branch = ask("new branch name? (empty = skip this repo) > ");
        if branch.is_empty() {
            return;
        }
        if !run_git(repo, &["checkout".to_string(), "-b".to_string(), branch]) {
            return;
        }
    }

    let message = ask("commit message? (empty = skip this repo) > ");
    if message.is_empty() {
        println!("skipped");
        return;
    }
    if !run_git(repo, stage_args) {
        return;
    }
    if !run_git(repo, &["commit".to_string(), "--message".to_string(), message]) {
        return;
    }
    if ask("push now? [y/N] > ").eq_ignore_ascii_case("y") {
        push_head(repo, &ask_push_options());
    }
}

/// `add --all` would record a nested repo as an embedded gitlink, never what a commit of
/// everything means.
fn stage_all_args(nested_repos: &[String]) -> Vec<String> {
    let mut args = vec!["add".to_string(), "--all".to_string(), "--".to_string()];
    args.extend(nested_repos.iter().map(|path| format!(":(exclude,literal){path}")));
    args
}

fn diff_file_args(file: &ChangedFile) -> Vec<String> {
    if file.untracked {
        // An untracked file has nothing to diff against, so show it whole.
        return vec![
            "diff".to_string(),
            "--no-index".to_string(),
            "--".to_string(),
            "/dev/null".to_string(),
            file.path.clone(),
        ];
    }
    vec!["diff".to_string(), "HEAD".to_string(), "--".to_string(), file.path.clone()]
}

fn log_branch_args(branch: &UnpushedBranch) -> Vec<String> {
    let mut args = vec!["log".to_string(), "--stat".to_string()];
    args.extend(branch_range_args(branch));
    args
}

/// The commits a branch holds and the remote does not.
fn branch_range_args(branch: &UnpushedBranch) -> Vec<String> {
    match branch.upstream {
        Some(_) => vec![format!("{name}@{{u}}..{name}", name = branch.name)],
        None => vec![branch.name.clone(), "--not".to_string(), "--remotes".to_string()],
    }
}

fn commit_subjects(repo: &Path, branch: &UnpushedBranch) -> Vec<String> {
    let mut args = vec![
        "log".to_string(),
        COMMIT_PREVIEW_FORMAT.to_string(),
        COMMIT_PREVIEW_DATE.to_string(),
        format!("--max-count={COMMITS_PREVIEW_LIMIT}"),
    ];
    args.extend(branch_range_args(branch));
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    git(repo, &borrowed)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn push_branch(repo: &Path, branch: &UnpushedBranch, options: &PushOptions) {
    let remote = match &branch.upstream {
        Some(upstream) => upstream.remote.clone(),
        None => match default_remote(repo) {
            Some(remote) => remote,
            None => {
                eprintln!("{}: no remote configured, skipped", display_path(repo));
                return;
            }
        },
    };
    run_git(repo, &push_args(branch, &remote, options));
}

fn push_args(branch: &UnpushedBranch, remote: &str, options: &PushOptions) -> Vec<String> {
    let mut args = vec!["push".to_string()];
    if options.force_with_lease {
        args.push("--force-with-lease".to_string());
    }
    if options.no_verify {
        args.push("--no-verify".to_string());
    }
    match &branch.upstream {
        Some(upstream) => {
            args.push(remote.to_string());
            args.push(format!("{}:{}", branch.name, upstream.remote_ref));
        }
        None => {
            args.push("--set-upstream".to_string());
            args.push(remote.to_string());
            args.push(branch.name.clone());
        }
    }
    args
}

fn push_head(repo: &Path, options: &PushOptions) {
    let mut args = vec!["push".to_string()];
    if options.force_with_lease {
        args.push("--force-with-lease".to_string());
    }
    if options.no_verify {
        args.push("--no-verify".to_string());
    }
    if git(repo, &["rev-parse", "--symbolic-full-name", "@{u}"]).is_none() {
        let (Some(remote), Some(branch)) = (default_remote(repo), current_branch(repo)) else {
            eprintln!("{}: no remote or detached HEAD, not pushed", display_path(repo));
            return;
        };
        args.push("--set-upstream".to_string());
        args.push(remote);
        args.push(branch);
    }
    run_git(repo, &args);
}

fn current_branch(repo: &Path) -> Option<String> {
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    (branch != "HEAD").then_some(branch)
}

fn default_remote(repo: &Path) -> Option<String> {
    let mut remotes: Vec<String> = git(repo, &["remote"])?.lines().map(str::to_string).collect();
    if remotes.is_empty() {
        return None;
    }
    let preferred = remotes.iter().position(|remote| remote == "origin").unwrap_or(0);
    Some(remotes.swap_remove(preferred))
}

fn run_git(repo: &Path, args: &[String]) -> bool {
    println!("$ git -C {} {}", display_path(repo), args.join(" "));
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ask(question: &str) -> String {
    print!("{question}");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return String::new();
    }
    answer.trim().to_string()
}

fn ask_push_options() -> PushOptions {
    loop {
        if let Some(options) = parse_push_options(&ask(FLAGS_HELP)) {
            return options;
        }
        println!("{FLAGS_REJECTED}");
    }
}

/// The whole answer has to be a flag word: a substring match would turn "none" into
/// --no-verify and skip the pre-push hooks without saying so.
fn parse_push_options(answer: &str) -> Option<PushOptions> {
    let (force_with_lease, no_verify) = match answer {
        "" => (false, false),
        "l" => (true, false),
        "n" => (false, true),
        "ln" | "nl" => (true, true),
        _ => return None,
    };
    Some(PushOptions { force_with_lease, no_verify })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Upstream;

    fn branch(name: &str, upstream: Option<Upstream>) -> UnpushedBranch {
        UnpushedBranch { name: name.to_string(), ahead: 1, last_commit: 0, upstream }
    }

    fn tracked(name: &str) -> Upstream {
        Upstream { remote: "origin".to_string(), remote_ref: format!("refs/heads/{name}") }
    }

    #[test]
    fn takes_a_whole_flag_word_only() {
        let none = parse_push_options("").unwrap();
        assert!(!none.force_with_lease && !none.no_verify);
        let both = parse_push_options("nl").unwrap();
        assert!(both.force_with_lease && both.no_verify);
        assert!(parse_push_options("n").unwrap().no_verify);
        assert!(parse_push_options("none").is_none());
        assert!(parse_push_options("lol").is_none());
    }

    #[test]
    fn pushes_to_the_tracked_ref() {
        let options = PushOptions { force_with_lease: true, no_verify: false };
        assert_eq!(
            push_args(&branch("local-main", Some(tracked("main"))), "origin", &options),
            ["push", "--force-with-lease", "origin", "local-main:refs/heads/main"]
        );
    }

    #[test]
    fn sets_upstream_when_branch_has_none() {
        let options = PushOptions { force_with_lease: false, no_verify: true };
        assert_eq!(
            push_args(&branch("feat/x", None), "origin", &options),
            ["push", "--no-verify", "--set-upstream", "origin", "feat/x"]
        );
    }

    #[test]
    fn logs_only_the_unpushed_commits() {
        assert_eq!(
            log_branch_args(&branch("main", Some(tracked("main")))),
            ["log", "--stat", "main@{u}..main"]
        );
        assert_eq!(
            log_branch_args(&branch("feat/x", None)),
            ["log", "--stat", "feat/x", "--not", "--remotes"]
        );
    }

    #[test]
    fn commits_the_selection_or_every_checkout_with_changes() {
        let changes = || {
            let file = ChangedFile {
                path: "a.rs".into(),
                added_lines: 1,
                removed_lines: 0,
                untracked: false,
                deleted: false,
            };
            WorkingTreeChanges { files: vec![file], nested_repos: Vec::new(), last_change: None }
        };
        let worktree = WorktreeReport { path: "/wt".into(), branch: None, changes: changes() };
        let report = RepoReport {
            path: "/repo".into(),
            changes: changes(),
            unpushed_branches: Vec::new(),
            worktrees: vec![worktree],
        };

        let mut selection = Selection::for_report(&report);
        let everything = commit_scope(&report, &selection);
        assert!(everything.main_checkout && everything.worktrees == [0]);

        selection.worktrees[0] = true;
        let worktree_only = commit_scope(&report, &selection);
        assert!(!worktree_only.main_checkout && worktree_only.worktrees == [0]);

        selection = Selection::for_report(&report);
        selection.files[0] = true;
        let files_only = commit_scope(&report, &selection);
        assert!(files_only.main_checkout && files_only.worktrees.is_empty());
    }

    #[test]
    fn pushes_the_branch_of_a_selected_worktree() {
        let no_changes = || WorkingTreeChanges {
            files: Vec::new(),
            nested_repos: Vec::new(),
            last_change: None,
        };
        let worktree = WorktreeReport {
            path: "/wt".into(),
            branch: Some("feat".into()),
            changes: no_changes(),
        };
        let report = RepoReport {
            path: "/repo".into(),
            changes: no_changes(),
            unpushed_branches: vec![branch("main", None), branch("feat", None)],
            worktrees: vec![worktree],
        };
        let names = |selection: &Selection| -> Vec<String> {
            push_scope(&report, selection).iter().map(|branch| branch.name.clone()).collect()
        };

        let mut selection = Selection::for_report(&report);
        assert_eq!(names(&selection), ["main", "feat"]);
        selection.worktrees[0] = true;
        assert_eq!(names(&selection), ["feat"]);
        selection.branches[0] = true;
        assert_eq!(names(&selection), ["main", "feat"]);
        selection.worktrees[0] = false;
        assert_eq!(names(&selection), ["main"]);
    }

    #[test]
    fn stages_everything_but_nested_repos() {
        assert_eq!(stage_all_args(&[]), ["add", "--all", "--"]);
        assert_eq!(
            stage_all_args(&[".claude/worktrees/[wip]".to_string()]),
            ["add", "--all", "--", ":(exclude,literal).claude/worktrees/[wip]"]
        );
    }

    #[test]
    fn shows_untracked_files_whole() {
        let untracked =
            ChangedFile { path: "new.rs".into(), added_lines: 3, removed_lines: 0, untracked: true, deleted: false };
        assert_eq!(diff_file_args(&untracked), ["diff", "--no-index", "--", "/dev/null", "new.rs"]);
    }
}
