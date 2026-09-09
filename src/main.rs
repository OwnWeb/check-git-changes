mod interactive;
mod term;

use rayon::prelude::*;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "venv",
    "bower_components",
    "__pycache__",
    "Pods",
    "dist",
    "build",
];
const UNTRACKED_SIZE_LIMIT_BYTES: u64 = 1 << 20;
/// Bounds the recursion: raise it if repos ever nest deeper than this.
const MAX_SCAN_DEPTH: usize = 32;
const SECONDS_PER_MINUTE: i64 = 60;
const SECONDS_PER_HOUR: i64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: i64 = 24 * SECONDS_PER_HOUR;
const SECONDS_PER_WEEK: i64 = 7 * SECONDS_PER_DAY;
const DEFAULT_PERIOD: &str = "24h";
const TABLE_HEADERS: [&str; 4] = ["REPO", "CHANGES", "UNPUSHED", "LAST CHANGE"];
const TABLE_GAP: &str = "   ";
const EMPTY_CELL: &str = "-";
const UNPUSHED_CELL_MAX_WIDTH: usize = 44;
const MIN_REPO_COLUMN_WIDTH: usize = 16;
const MIN_UNPUSHED_COLUMN_WIDTH: usize = 12;
const PREFIX_COLUMN_WIDTH: usize = 5;
const DATE_COLUMN_WIDTH: usize = 16;
const UNLIMITED_WIDTH: usize = 10_000;
const ELLIPSIS: char = '\u{2026}';
const BRANCH_FORMAT: &str = "--format=%(refname:short)%09%(upstream:remotename)%09%(upstream:remoteref)%09%(upstream:track)%09%(committerdate:unix)";
const USAGE: &str = "\
cgc: git changes not committed and branches not pushed, across every repo under a directory.

usage: cgc [DIR] [--since PERIOD] [--no-input]

  DIR                   directory to scan recursively (default: current directory)
  -s, --since PERIOD    how far back to look: 30m, 24h, 7d, 2w (default: 24h)
  -n, --no-input        print the table and exit, skipping the interactive browser
  -h, --help            this message
";

struct Upstream {
    remote: String,
    remote_ref: String,
}

struct UnpushedBranch {
    name: String,
    ahead: u64,
    last_commit: u64,
    upstream: Option<Upstream>,
}

struct ChangedFile {
    path: String,
    added_lines: u64,
    removed_lines: u64,
    untracked: bool,
    deleted: bool,
}

impl ChangedFile {
    fn status(&self) -> char {
        match (self.untracked, self.deleted) {
            (true, _) => '?',
            (_, true) => 'D',
            _ => 'M',
        }
    }
}

struct WorkingTreeChanges {
    files: Vec<ChangedFile>,
    last_change: Option<u64>,
}

impl WorkingTreeChanges {
    fn added_lines(&self) -> u64 {
        self.files.iter().map(|file| file.added_lines).sum()
    }

    fn removed_lines(&self) -> u64 {
        self.files.iter().map(|file| file.removed_lines).sum()
    }
}

struct RepoReport {
    path: PathBuf,
    changes: WorkingTreeChanges,
    unpushed_branches: Vec<UnpushedBranch>,
}

impl RepoReport {
    fn is_clean(&self) -> bool {
        self.changes.files.is_empty() && self.unpushed_branches.is_empty()
    }

    fn last_activity(&self) -> Option<u64> {
        let last_commit = self.unpushed_branches.iter().map(|branch| branch.last_commit).max();
        self.changes.last_change.max(last_commit)
    }
}

struct Palette {
    bold: &'static str,
    dim: &'static str,
    green: &'static str,
    red: &'static str,
    yellow: &'static str,
    reset: &'static str,
}

impl Palette {
    fn detect() -> Self {
        let colored = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        if !colored {
            return Self { bold: "", dim: "", green: "", red: "", yellow: "", reset: "" };
        }
        Self {
            bold: "\x1b[1m",
            dim: "\x1b[2m",
            green: "\x1b[32m",
            red: "\x1b[31m",
            yellow: "\x1b[33m",
            reset: "\x1b[0m",
        }
    }
}

struct ScanContext {
    root: PathBuf,
    cutoff: u64,
    scanned: usize,
    period: String,
    elapsed: std::time::Duration,
}

struct Options {
    root: PathBuf,
    period: String,
    no_input: bool,
}

fn main() {
    let Some(options) = parse_args() else {
        print!("{USAGE}");
        return;
    };
    let Some(period_seconds) = parse_period(&options.period) else {
        eprintln!(
            "cgc: invalid period {:?}, expected something like 30m, 24h, 7d or 2w",
            options.period
        );
        std::process::exit(2);
    };
    let cutoff = now_unix().saturating_sub(period_seconds);

    let started = Instant::now();
    let mut repos = Vec::new();
    collect_repos(&options.root, MAX_SCAN_DEPTH, &mut repos);
    let mut reports: Vec<RepoReport> = repos
        .par_iter()
        .map(|repo| scan_repo(repo, cutoff))
        .filter(|report| !report.is_clean())
        .collect();
    reports.sort_by_key(|report| std::cmp::Reverse(report.last_activity()));

    let context = ScanContext {
        root: options.root,
        cutoff,
        scanned: repos.len(),
        period: options.period,
        elapsed: started.elapsed(),
    };
    if options.no_input || reports.is_empty() || !std::io::stdin().is_terminal() {
        report(&reports, &context);
        return;
    }
    interactive::run(reports, context);
}

fn parse_args() -> Option<Options> {
    let mut root = None;
    let mut period = DEFAULT_PERIOD.to_string();
    let mut no_input = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return None,
            "-s" | "--since" => period = args.next()?,
            "-n" | "--no-input" => no_input = true,
            flag if flag.starts_with("--since=") => {
                period = flag.trim_start_matches("--since=").to_string()
            }
            flag if flag.starts_with('-') => return None,
            path => root = Some(PathBuf::from(path)),
        }
    }
    Some(Options { root: root.unwrap_or_else(|| PathBuf::from(".")), period, no_input })
}

fn parse_period(spec: &str) -> Option<u64> {
    let unit_start = spec.find(|c: char| !c.is_ascii_digit()).unwrap_or(spec.len());
    let (amount, unit) = spec.split_at(unit_start);
    let seconds_per_unit = match unit {
        "" | "h" => SECONDS_PER_HOUR,
        "m" => SECONDS_PER_MINUTE,
        "d" => SECONDS_PER_DAY,
        "w" => SECONDS_PER_WEEK,
        _ => return None,
    };
    amount.parse::<u64>().ok()?.checked_mul(seconds_per_unit as u64)
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|since| since.as_secs()).unwrap_or(0)
}

fn collect_repos(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    if dir.join(".git").exists() {
        found.push(dir.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // Symlinked directories report false here, which also keeps the walk out of cycles.
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        collect_repos(&entry.path(), depth - 1, found);
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("-c")
        .arg("core.quotepath=false")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn scan_repo(repo: &Path, cutoff: u64) -> RepoReport {
    RepoReport {
        path: repo.to_path_buf(),
        changes: scan_working_tree(repo, cutoff),
        unpushed_branches: scan_branches(repo, cutoff),
    }
}

fn scan_working_tree(repo: &Path, cutoff: u64) -> WorkingTreeChanges {
    let mut changes = WorkingTreeChanges { files: Vec::new(), last_change: None };

    let tracked =
        git(repo, &["diff", "--numstat", "--no-renames", "-z", "HEAD"]).unwrap_or_default();
    for record in split_nul(&tracked) {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let full_path = repo.join(path);
        let Some(changed_at) = change_time(&full_path, repo, cutoff) else {
            continue;
        };
        changes.files.push(ChangedFile {
            path: path.to_string(),
            added_lines: added.parse().unwrap_or(0),
            removed_lines: removed.parse().unwrap_or(0),
            untracked: false,
            deleted: !full_path.exists(),
        });
        changes.last_change = changes.last_change.max(Some(changed_at));
    }

    let untracked =
        git(repo, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for path in split_nul(&untracked) {
        let full_path = repo.join(path);
        let Some(changed_at) = change_time(&full_path, repo, cutoff) else {
            continue;
        };
        changes.files.push(ChangedFile {
            path: path.to_string(),
            added_lines: count_lines(&full_path),
            removed_lines: 0,
            untracked: true,
            deleted: false,
        });
        changes.last_change = changes.last_change.max(Some(changed_at));
    }

    changes
}

fn change_time(path: &Path, repo: &Path, cutoff: u64) -> Option<u64> {
    // A deleted file has no mtime of its own, so fall back to the nearest surviving
    // ancestor: removing an entry updates the mtime of the directory holding it. The walk
    // stops at the repo, above which a directory says nothing about this repo.
    let modified = path
        .ancestors()
        .take_while(|ancestor| ancestor.starts_with(repo))
        .find_map(|ancestor| std::fs::metadata(ancestor).and_then(|entry| entry.modified()).ok())?;
    let seconds = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    (seconds >= cutoff).then_some(seconds)
}

/// Git leaves a NUL terminated path verbatim, where its line output quotes any path
/// holding a quote, a backslash or a control character.
fn split_nul(output: &str) -> impl Iterator<Item = &str> {
    output.split('\0').filter(|record| !record.is_empty())
}

fn count_lines(path: &Path) -> u64 {
    // Huge untracked blobs count as a file but not as lines: reading them would dominate runtime.
    let Ok(metadata) = std::fs::metadata(path) else {
        return 0;
    };
    if metadata.len() > UNTRACKED_SIZE_LIMIT_BYTES {
        return 0;
    }
    std::fs::read(path)
        .map(|bytes| bytes.iter().filter(|byte| **byte == b'\n').count() as u64)
        .unwrap_or(0)
}

fn scan_branches(repo: &Path, cutoff: u64) -> Vec<UnpushedBranch> {
    let refs = git(repo, &["for-each-ref", BRANCH_FORMAT, "refs/heads"]).unwrap_or_default();
    refs.lines().filter_map(|line| parse_branch(repo, line, cutoff)).collect()
}

fn parse_branch(repo: &Path, line: &str, cutoff: u64) -> Option<UnpushedBranch> {
    let mut fields = line.split('\t');
    let name = fields.next()?;
    let remote = fields.next()?;
    let remote_ref = fields.next()?;
    let track = fields.next()?;
    let last_commit: u64 = fields.next()?.parse().ok()?;
    if last_commit < cutoff {
        return None;
    }
    let upstream = (!remote.is_empty() && !track.contains("gone"))
        .then(|| Upstream { remote: remote.to_string(), remote_ref: remote_ref.to_string() });
    let ahead = match upstream {
        Some(_) => parse_ahead(track),
        None => count_commits_missing_from_remotes(repo, name),
    };
    (ahead > 0).then(|| UnpushedBranch { name: name.to_string(), ahead, last_commit, upstream })
}

fn parse_ahead(track: &str) -> u64 {
    let Some(after_keyword) = track.split("ahead ").nth(1) else {
        return 0;
    };
    after_keyword
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

fn count_commits_missing_from_remotes(repo: &Path, branch: &str) -> u64 {
    git(repo, &["rev-list", "--count", branch, "--not", "--remotes"])
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0)
}

fn repo_label(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(|relative| relative.to_string_lossy().into_owned())
        .unwrap_or_else(|| display_path(path))
}

fn display_path(path: &Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    path.to_string_lossy().replacen(&home, "~", 1)
}

#[derive(Clone)]
struct Cell {
    plain: String,
    colored: String,
}

impl Cell {
    fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        Self { colored: text.clone(), plain: text }
    }

    fn colored(plain: String, colored: String) -> Self {
        Self { plain, colored }
    }

    fn width(&self) -> usize {
        self.plain.chars().count()
    }

    fn pad_to(&self, width: usize) -> String {
        format!("{}{}", self.colored, " ".repeat(width.saturating_sub(self.width())))
    }
}

/// Column widths come from every row, so they stay put while the cursor scrolls.
struct Table {
    header: Vec<Cell>,
    rows: Vec<Vec<Cell>>,
    widths: Vec<usize>,
}

impl Table {
    fn new(header: Vec<Cell>, rows: Vec<Vec<Cell>>) -> Self {
        let widths = (0..header.len())
            .map(|column| {
                rows.iter()
                    .map(|row| row[column].width())
                    .chain(std::iter::once(header[column].width()))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        Self { header, rows, widths }
    }

    fn header_line(&self) -> String {
        render_row(&self.header, &self.widths)
    }

    fn row_line(&self, index: usize) -> String {
        render_row(&self.rows[index], &self.widths)
    }
}

fn render_row(cells: &[Cell], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths)
        .map(|(cell, width)| cell.pad_to(*width))
        .collect::<Vec<_>>()
        .join(TABLE_GAP)
        .trim_end()
        .to_string()
}

fn build_table(
    reports: &[RepoReport],
    context: &ScanContext,
    palette: &Palette,
    offset: i64,
    prefixes: Option<&[Cell]>,
) -> Table {
    let layout = table_layout(reports, context, prefixes.is_some());
    let mut header: Vec<Cell> = TABLE_HEADERS[..layout.column_count()]
        .iter()
        .map(|title| Cell::plain(*title))
        .collect();
    let mut rows: Vec<Vec<Cell>> = reports
        .iter()
        .map(|report| table_row(report, &context.root, palette, offset, &layout))
        .collect();
    if let Some(prefixes) = prefixes {
        header.insert(0, Cell::plain(""));
        for (row, prefix) in rows.iter_mut().zip(prefixes) {
            row.insert(0, prefix.clone());
        }
    }
    Table::new(header, rows)
}

fn table_row(
    report: &RepoReport,
    root: &Path,
    palette: &Palette,
    offset: i64,
    layout: &TableLayout,
) -> Vec<Cell> {
    let mut cells = vec![
        Cell::plain(truncate_start(&repo_label(&report.path, root), layout.limits.repo)),
        changes_cell(&report.changes, palette),
        unpushed_cell(&report.unpushed_branches, palette, layout.limits.unpushed),
    ];
    if layout.with_date {
        let last_activity =
            report.last_activity().map(|at| format_local(at, offset)).unwrap_or_default();
        cells.push(Cell::colored(
            last_activity.clone(),
            format!("{}{}{}", palette.dim, last_activity, palette.reset),
        ));
    }
    cells
}

fn changes_cell(changes: &WorkingTreeChanges, palette: &Palette) -> Cell {
    if changes.files.is_empty() {
        return Cell::plain(EMPTY_CELL);
    }
    let count = changes.files.len();
    let files = format!("{count} file{}", plural(count));
    let added = changes.added_lines();
    let removed = changes.removed_lines();
    Cell::colored(
        changes_text(changes),
        format!(
            "{files}  {}+{added}{} {}-{removed}{}",
            palette.green, palette.reset, palette.red, palette.reset
        ),
    )
}

fn changes_text(changes: &WorkingTreeChanges) -> String {
    if changes.files.is_empty() {
        return EMPTY_CELL.to_string();
    }
    let count = changes.files.len();
    format!(
        "{count} file{}  +{} -{}",
        plural(count),
        changes.added_lines(),
        changes.removed_lines()
    )
}

fn unpushed_cell(branches: &[UnpushedBranch], palette: &Palette, max_width: usize) -> Cell {
    if branches.is_empty() {
        return Cell::plain(EMPTY_CELL);
    }
    let plain = truncate_end(&unpushed_text(branches, max_width), max_width);
    Cell::colored(plain.clone(), format!("{}{}{}", palette.yellow, plain, palette.reset))
}

fn unpushed_text(branches: &[UnpushedBranch], max_width: usize) -> String {
    let labels: Vec<String> = branches.iter().map(describe_branch).collect();
    join_within_width(&labels, max_width)
}

struct ColumnLimits {
    repo: usize,
    unpushed: usize,
}

struct TableLayout {
    limits: ColumnLimits,
    with_date: bool,
}

impl TableLayout {
    fn column_count(&self) -> usize {
        if self.with_date {
            TABLE_HEADERS.len()
        } else {
            TABLE_HEADERS.len() - 1
        }
    }
}

/// Fits the table to the terminal: the date column goes first when the two
/// shortenable columns can no longer hold their minimum, then names get trimmed.
fn table_layout(reports: &[RepoReport], context: &ScanContext, has_prefix: bool) -> TableLayout {
    let natural_repo = widest(
        reports.iter().map(|report| repo_label(&report.path, &context.root).chars().count()),
        TABLE_HEADERS[0],
    );
    let natural_unpushed = widest(
        reports.iter().map(|report| {
            unpushed_text(&report.unpushed_branches, UNPUSHED_CELL_MAX_WIDTH).chars().count()
        }),
        TABLE_HEADERS[2],
    );
    let natural_changes = widest(
        reports.iter().map(|report| changes_text(&report.changes).chars().count()),
        TABLE_HEADERS[1],
    );

    let available = available_width();
    let prefix = if has_prefix { PREFIX_COLUMN_WIDTH + TABLE_GAP.len() } else { 0 };
    let fixed_without_date = prefix + natural_changes + 2 * TABLE_GAP.len();
    let fixed_with_date = fixed_without_date + DATE_COLUMN_WIDTH + TABLE_GAP.len();
    let minimums = MIN_REPO_COLUMN_WIDTH.min(natural_repo)
        + MIN_UNPUSHED_COLUMN_WIDTH.min(natural_unpushed);

    let with_date = fixed_with_date + minimums <= available;
    let fixed = if with_date { fixed_with_date } else { fixed_without_date };
    let (repo, unpushed) = split_flexible_width(
        available.saturating_sub(fixed),
        natural_repo,
        natural_unpushed,
    );
    TableLayout { limits: ColumnLimits { repo, unpushed }, with_date }
}

/// Never returns more than `flexible` in total, so the row cannot wrap.
fn split_flexible_width(
    flexible: usize,
    natural_repo: usize,
    natural_unpushed: usize,
) -> (usize, usize) {
    let wanted = natural_repo + natural_unpushed;
    if wanted == 0 {
        return (0, 0);
    }
    if wanted <= flexible {
        return (natural_repo, natural_unpushed);
    }
    let unpushed_floor =
        MIN_UNPUSHED_COLUMN_WIDTH.min(natural_unpushed).min(flexible / 2);
    let repo_ceiling = (flexible - unpushed_floor).min(natural_repo);
    let repo_floor = MIN_REPO_COLUMN_WIDTH.min(repo_ceiling);
    let repo = (flexible * natural_repo / wanted).clamp(repo_floor, repo_ceiling.max(repo_floor));
    let unpushed = (flexible - repo).min(natural_unpushed);
    (repo, unpushed)
}

fn widest(widths: impl Iterator<Item = usize>, header: &str) -> usize {
    widths.chain(std::iter::once(header.chars().count())).max().unwrap_or(0)
}

/// A redirected table keeps its full names: only a terminal has an edge to respect.
fn available_width() -> usize {
    if std::io::stdout().is_terminal() {
        term::terminal_size().1
    } else {
        UNLIMITED_WIDTH
    }
}

fn truncate_start(text: &str, max_width: usize) -> String {
    let width = text.chars().count();
    if width <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let kept: String = text.chars().skip(width - max_width + 1).collect();
    format!("{ELLIPSIS}{kept}")
}

fn truncate_end(text: &str, max_width: usize) -> String {
    let width = text.chars().count();
    if width <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let kept: String = text.chars().take(max_width - 1).collect();
    format!("{kept}{ELLIPSIS}")
}

fn describe_branch(branch: &UnpushedBranch) -> String {
    let upstream = if branch.upstream.is_some() { "" } else { " (new)" };
    format!("{} ahead {}{}", branch.name, branch.ahead, upstream)
}

/// Names first, so the column says which branches. What does not fit is counted.
fn join_within_width(labels: &[String], max_width: usize) -> String {
    let mut line = String::new();
    for (index, label) in labels.iter().enumerate() {
        let candidate =
            if line.is_empty() { label.clone() } else { format!("{line}, {label}") };
        if !line.is_empty() && candidate.chars().count() > max_width {
            return format!("{line}, +{} more", labels.len() - index);
        }
        line = candidate;
    }
    line
}

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn summary_line(reports: &[RepoReport], context: &ScanContext, palette: &Palette) -> String {
    let changed_files: usize = reports.iter().map(|report| report.changes.files.len()).sum();
    let unpushed_branches: usize = reports.iter().map(|report| report.unpushed_branches.len()).sum();
    let full = format!(
        "{} repo(s) with activity in the last {} out of {} scanned: {} file(s), {} unpushed branch(es), {:.0?}",
        reports.len(),
        context.period,
        context.scanned,
        changed_files,
        unpushed_branches,
        context.elapsed,
    );
    let text = if full.chars().count() <= available_width() {
        full
    } else {
        format!(
            "{} repo(s), {} file(s), {} unpushed, {:.0?}",
            reports.len(),
            changed_files,
            unpushed_branches,
            context.elapsed
        )
    };
    format!("{}{}{}", palette.dim, text, palette.reset)
}

fn report(reports: &[RepoReport], context: &ScanContext) {
    let palette = Palette::detect();
    let offset = local_offset_seconds();
    if !reports.is_empty() {
        let table = build_table(reports, context, &palette, offset, None);
        println!("{}{}{}", palette.dim, table.header_line(), palette.reset);
        for index in 0..reports.len() {
            println!("{}", table.row_line(index));
        }
        println!();
    }
    println!("{}", summary_line(reports, context, &palette));
}

fn local_offset_seconds() -> i64 {
    // std ships no timezone database, and date(1) already knows the local offset.
    Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|output| parse_utc_offset(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or(0)
}

fn parse_utc_offset(raw: &str) -> Option<i64> {
    const DIGITS_IN_OFFSET: usize = 4;
    let raw = raw.trim();
    let (sign, digits) = raw.split_at(raw.len().min(1));
    if digits.len() != DIGITS_IN_OFFSET {
        return None;
    }
    let sign = match sign {
        "+" => 1,
        "-" => -1,
        _ => return None,
    };
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * SECONDS_PER_HOUR + minutes * SECONDS_PER_MINUTE))
}

fn format_local(timestamp: u64, offset_seconds: i64) -> String {
    let local = timestamp as i64 + offset_seconds;
    let (year, month, day) = civil_from_days(local.div_euclid(SECONDS_PER_DAY));
    let seconds_in_day = local.rem_euclid(SECONDS_PER_DAY);
    let hours = seconds_in_day / SECONDS_PER_HOUR;
    let minutes = seconds_in_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE;
    format!("{year:04}-{month:02}-{day:02} {hours:02}:{minutes:02}")
}

/// Howard Hinnant's civil_from_days: https://howardhinnant.github.io/date_algorithms.html
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    const DAYS_FROM_YEAR_ZERO_MARCH_TO_EPOCH: i64 = 719_468;
    const DAYS_PER_ERA: i64 = 146_097;
    const DAYS_PER_YEAR: i64 = 365;

    let days = days_since_epoch + DAYS_FROM_YEAR_ZERO_MARCH_TO_EPOCH;
    let era = days.div_euclid(DAYS_PER_ERA);
    let day_of_era = days - era * DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / DAYS_PER_YEAR;
    let day_of_year =
        day_of_era - (DAYS_PER_YEAR * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_shifted_to_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_shifted_to_march + 2) / 5 + 1;
    let month = if month_shifted_to_march < 10 {
        month_shifted_to_march + 3
    } else {
        month_shifted_to_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_hands_out_more_width_than_it_has() {
        assert_eq!(split_flexible_width(100, 20, 30), (20, 30));
        let (repo, unpushed) = split_flexible_width(36, 53, 44);
        assert_eq!(repo + unpushed, 36);
        assert!(repo >= MIN_REPO_COLUMN_WIDTH && unpushed >= MIN_UNPUSHED_COLUMN_WIDTH);
        let (repo, unpushed) = split_flexible_width(16, 53, 44);
        assert_eq!(repo + unpushed, 16);
        assert_eq!(split_flexible_width(0, 53, 44), (0, 0));
        assert_eq!(split_flexible_width(10, 0, 0), (0, 0));
    }

    #[test]
    fn truncates_from_the_side_that_matters() {
        assert_eq!(truncate_start("orgB/team/repoD", 12), "\u{2026}/team/repoD");
        assert_eq!(truncate_start("short", 12), "short");
        assert_eq!(truncate_end("fix/request-timeouts ahead 4", 12), "fix/request\u{2026}");
        assert_eq!(truncate_end("main", 12), "main");
    }

    #[test]
    fn lists_branch_names_until_the_column_is_full() {
        let short = ["main ahead 1".to_string(), "fix/x ahead 2".to_string()];
        assert_eq!(join_within_width(&short, 44), "main ahead 1, fix/x ahead 2");

        let long = [
            "chore/deps ahead 1".to_string(),
            "fix/request-timeouts ahead 4 (new)".to_string(),
            "feat/search ahead 2".to_string(),
        ];
        assert_eq!(join_within_width(&long, 44), "chore/deps ahead 1, +2 more");

        let single = ["a-very-long-branch-name-on-its-own ahead 12".to_string()];
        assert_eq!(join_within_width(&single, 20), single[0]);
    }

    #[test]
    fn keeps_the_paths_git_would_have_quoted() {
        let numstat = "1\t0\tplain.txt\u{0}2\t1\twe\"ird.txt\u{0}\n";
        assert_eq!(
            split_nul(numstat).collect::<Vec<_>>(),
            ["1\t0\tplain.txt", "2\t1\twe\"ird.txt", "\n"]
        );
        assert_eq!(split_nul("new\"file.txt\u{0}").collect::<Vec<_>>(), ["new\"file.txt"]);
        assert_eq!(split_nul("").next(), None);
    }

    #[test]
    fn parses_periods() {
        assert_eq!(parse_period("24h"), Some(SECONDS_PER_DAY as u64));
        assert_eq!(parse_period("30m"), Some(30 * SECONDS_PER_MINUTE as u64));
        assert_eq!(parse_period("2w"), Some(2 * SECONDS_PER_WEEK as u64));
        assert_eq!(parse_period("12"), Some(12 * SECONDS_PER_HOUR as u64));
        assert_eq!(parse_period("7y"), None);
        assert_eq!(parse_period("h"), None);
    }

    #[test]
    fn parses_upstream_track() {
        assert_eq!(parse_ahead("[ahead 3]"), 3);
        assert_eq!(parse_ahead("[ahead 12, behind 4]"), 12);
        assert_eq!(parse_ahead("[behind 4]"), 0);
        assert_eq!(parse_ahead(""), 0);
    }

    #[test]
    fn parses_utc_offsets() {
        assert_eq!(parse_utc_offset("+0200\n"), Some(2 * SECONDS_PER_HOUR));
        assert_eq!(
            parse_utc_offset("-0330"),
            Some(-(3 * SECONDS_PER_HOUR + 30 * SECONDS_PER_MINUTE))
        );
        assert_eq!(parse_utc_offset("+0000"), Some(0));
        assert_eq!(parse_utc_offset("CEST"), None);
        assert_eq!(parse_utc_offset(""), None);
    }

    #[test]
    fn formats_timestamps_in_local_time() {
        assert_eq!(format_local(0, 0), "1970-01-01 00:00");
        assert_eq!(format_local(1_757_339_040, 2 * SECONDS_PER_HOUR), "2025-09-08 15:44");
        assert_eq!(format_local(1_788_961_140, 0), "2026-09-09 13:39");
        assert_eq!(format_local(1_582_934_400, 0), "2020-02-29 00:00");
    }
}
