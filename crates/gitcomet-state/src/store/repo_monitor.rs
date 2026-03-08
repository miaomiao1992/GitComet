use crate::model::RepoId;
use crate::msg::{Msg, RepoExternalChange};
use notify::event::{AccessKind, AccessMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rustc_hash::FxHashMap as HashMap;
use std::any::Any;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::send_diagnostics::{SendFailureKind, send_or_log};

enum MonitorMsg {
    Event(notify::Result<notify::Event>),
    Stop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MonitorFailureKind {
    Start,
    Stop,
    Join,
}

static REPO_MONITOR_START_FAILURES: AtomicU64 = AtomicU64::new(0);
static REPO_MONITOR_STOP_FAILURES: AtomicU64 = AtomicU64::new(0);
static REPO_MONITOR_JOIN_FAILURES: AtomicU64 = AtomicU64::new(0);

fn monitor_failure_counter(kind: MonitorFailureKind) -> &'static AtomicU64 {
    match kind {
        MonitorFailureKind::Start => &REPO_MONITOR_START_FAILURES,
        MonitorFailureKind::Stop => &REPO_MONITOR_STOP_FAILURES,
        MonitorFailureKind::Join => &REPO_MONITOR_JOIN_FAILURES,
    }
}

fn record_monitor_failure(
    kind: MonitorFailureKind,
    context: &'static str,
    detail: impl std::fmt::Display,
) {
    let count = monitor_failure_counter(kind).fetch_add(1, Ordering::Relaxed) + 1;
    eprintln!(
        "gitcomet-state: repo monitor failure ({kind:?}) in {context}: {detail}; total_failures={count}"
    );
}

fn send_stop_or_log(tx: &mpsc::Sender<MonitorMsg>, repo_id: RepoId, context: &'static str) {
    if let Err(error) = tx.send(MonitorMsg::Stop) {
        record_monitor_failure(
            MonitorFailureKind::Stop,
            context,
            format!("repo_id={repo_id:?}; send failed: {error}"),
        );
    }
}

fn panic_payload_to_string(payload: Box<dyn Any + Send + 'static>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

fn join_monitor_or_log(join: thread::JoinHandle<()>, repo_id: RepoId, context: &'static str) {
    if let Err(error) = join.join() {
        record_monitor_failure(
            MonitorFailureKind::Join,
            context,
            format!(
                "repo_id={repo_id:?}; join failed: {}",
                panic_payload_to_string(error)
            ),
        );
    }
}

#[cfg(test)]
pub(super) fn monitor_failure_count(kind: MonitorFailureKind) -> u64 {
    monitor_failure_counter(kind).load(Ordering::Relaxed)
}

#[cfg(test)]
pub(super) fn record_stop_send_failure_for_test(repo_id: RepoId, context: &'static str) {
    let (tx, rx) = mpsc::channel::<MonitorMsg>();
    drop(rx);
    send_stop_or_log(&tx, repo_id, context);
}

#[cfg(test)]
pub(super) fn join_monitor_for_test(
    join: thread::JoinHandle<()>,
    repo_id: RepoId,
    context: &'static str,
) {
    join_monitor_or_log(join, repo_id, context);
}

fn canonicalize_path(path: PathBuf) -> PathBuf {
    strip_windows_verbatim_prefix(path.canonicalize().unwrap_or(path))
}

#[cfg(windows)]
fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix(Path::new(r"\\?\UNC\")) {
        return Path::new(r"\\").join(stripped);
    }
    if let Ok(stripped) = path.strip_prefix(Path::new(r"\\?\")) {
        return stripped.to_path_buf();
    }
    path
}

#[cfg(not(windows))]
fn strip_windows_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DebouncedChange {
    pending: Option<RepoExternalChange>,
    first_event_at: Option<Instant>,
    last_event_at: Option<Instant>,
    debounce: Duration,
    max_delay: Duration,
}

impl DebouncedChange {
    fn new(debounce: Duration, max_delay: Duration) -> Self {
        Self {
            pending: None,
            first_event_at: None,
            last_event_at: None,
            debounce,
            max_delay,
        }
    }

    fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn push(&mut self, change: RepoExternalChange, now: Instant) -> Option<RepoExternalChange> {
        self.pending = Some(merge_change(self.pending.unwrap_or(change), change));
        self.first_event_at.get_or_insert(now);
        self.last_event_at = Some(now);
        self.take_if_max_delay_elapsed(now)
    }

    fn take_if_max_delay_elapsed(&mut self, now: Instant) -> Option<RepoExternalChange> {
        let first = self.first_event_at?;
        if now.duration_since(first) >= self.max_delay {
            self.take()
        } else {
            None
        }
    }

    fn next_timeout(&self, now: Instant) -> Option<Duration> {
        let (first, last) = (self.first_event_at?, self.last_event_at?);
        let due_by_debounce = last + self.debounce;
        let due_by_max = first + self.max_delay;
        let due = if due_by_debounce <= due_by_max {
            due_by_debounce
        } else {
            due_by_max
        };
        Some(due.saturating_duration_since(now))
    }

    fn take_if_due(&mut self, now: Instant) -> Option<RepoExternalChange> {
        if !self.is_pending() {
            return None;
        }
        let timeout = self.next_timeout(now).unwrap_or(Duration::from_secs(0));
        if timeout.is_zero() { self.take() } else { None }
    }

    fn take(&mut self) -> Option<RepoExternalChange> {
        let pending = self.pending.take();
        self.first_event_at = None;
        self.last_event_at = None;
        pending
    }
}

pub(super) struct RepoMonitorManager {
    handles: HashMap<RepoId, RepoMonitorHandle>,
}

impl RepoMonitorManager {
    pub(super) fn new() -> Self {
        Self {
            handles: HashMap::default(),
        }
    }

    pub(super) fn stop_all(&mut self) {
        for (repo_id, handle) in self.handles.drain() {
            send_stop_or_log(&handle.msg_tx, repo_id, "RepoMonitorManager::stop_all");
            join_monitor_or_log(handle.join, repo_id, "RepoMonitorManager::stop_all");
        }
    }

    pub(super) fn stop(&mut self, repo_id: RepoId) {
        let Some(handle) = self.handles.remove(&repo_id) else {
            return;
        };
        send_stop_or_log(&handle.msg_tx, repo_id, "RepoMonitorManager::stop");
        join_monitor_or_log(handle.join, repo_id, "RepoMonitorManager::stop");
    }

    pub(super) fn running_repo_ids(&self) -> Vec<RepoId> {
        self.handles.keys().copied().collect()
    }

    pub(super) fn start(
        &mut self,
        repo_id: RepoId,
        workdir: PathBuf,
        msg_tx: mpsc::Sender<Msg>,
        active_repo_id: Arc<AtomicU64>,
    ) {
        let std::collections::hash_map::Entry::Vacant(entry) = self.handles.entry(repo_id) else {
            return;
        };
        let (monitor_tx, monitor_rx) = mpsc::channel::<MonitorMsg>();
        let monitor_tx_for_notify = monitor_tx.clone();
        let join = thread::spawn(move || {
            repo_monitor_thread(
                repo_id,
                workdir,
                msg_tx,
                monitor_rx,
                monitor_tx_for_notify,
                active_repo_id,
            )
        });
        entry.insert(RepoMonitorHandle {
            msg_tx: monitor_tx,
            join,
        });
    }
}

struct RepoMonitorHandle {
    msg_tx: mpsc::Sender<MonitorMsg>,
    join: thread::JoinHandle<()>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct IgnoreCacheKey {
    rel: PathBuf,
    is_dir_hint: Option<bool>,
}

#[derive(Clone, Default)]
struct GitignoreRules {
    workdir: Option<PathBuf>,
    cache: HashMap<IgnoreCacheKey, bool>,
}

impl GitignoreRules {
    fn load(workdir: &Path, _git_dir: Option<&Path>) -> Self {
        Self {
            workdir: Some(workdir.to_path_buf()),
            cache: HashMap::default(),
        }
    }

    fn is_ignored_rel(&mut self, rel: &Path, is_dir_hint: Option<bool>) -> bool {
        let Some(workdir) = &self.workdir else {
            return false;
        };

        let key = IgnoreCacheKey {
            rel: rel.to_path_buf(),
            is_dir_hint,
        };
        if let Some(ignored) = self.cache.get(&key) {
            return *ignored;
        }

        let ignored = query_git_check_ignore(workdir, rel, is_dir_hint).unwrap_or(false);
        self.cache.insert(key, ignored);
        ignored
    }
}

fn query_git_check_ignore(workdir: &Path, rel: &Path, is_dir_hint: Option<bool>) -> Option<bool> {
    let exact = run_git_check_ignore(workdir, rel)?;
    if exact {
        return Some(true);
    }

    if is_dir_hint != Some(true) {
        return Some(false);
    }

    let rel_dir = rel_path_with_trailing_separator(rel);
    if rel_dir == rel {
        let rel_child = rel.join(".gitcomet-ignore-probe");
        return run_git_check_ignore(workdir, &rel_child);
    }
    if run_git_check_ignore(workdir, &rel_dir)? {
        return Some(true);
    }

    // Directory-only patterns (e.g. `target/`) don't always match the directory
    // path itself in `git check-ignore`; probing a synthetic child path mirrors
    // how git applies the rule to contents.
    let rel_child = rel.join(".gitcomet-ignore-probe");
    run_git_check_ignore(workdir, &rel_child)
}

fn run_git_check_ignore(workdir: &Path, rel: &Path) -> Option<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("check-ignore")
        .arg("--quiet")
        .arg("--")
        .arg(rel)
        .output()
        .ok()?;

    match output.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn rel_path_with_trailing_separator(rel: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let mut bytes = rel.as_os_str().as_bytes().to_vec();
        if bytes.last() == Some(&b'/') {
            return rel.to_path_buf();
        }
        bytes.push(b'/');
        PathBuf::from(OsString::from_vec(bytes))
    }

    #[cfg(not(unix))]
    {
        let mut rel_with_sep = rel.as_os_str().to_os_string();
        rel_with_sep.push("/");
        PathBuf::from(rel_with_sep)
    }
}

fn repo_monitor_thread(
    repo_id: RepoId,
    workdir: PathBuf,
    msg_tx: mpsc::Sender<Msg>,
    monitor_rx: mpsc::Receiver<MonitorMsg>,
    monitor_tx: mpsc::Sender<MonitorMsg>,
    active_repo_id: Arc<AtomicU64>,
) {
    let workdir = canonicalize_path(workdir);
    let git_dir = resolve_git_dir(&workdir);
    let mut gitignore = GitignoreRules::load(&workdir, git_dir.as_deref());

    let watcher = notify::recommended_watcher({
        let monitor_tx = monitor_tx.clone();
        move |res| {
            send_or_log(
                &monitor_tx,
                MonitorMsg::Event(res),
                SendFailureKind::RepoMonitorMessage,
                "repo monitor watcher callback",
            );
        }
    });

    let mut watcher: RecommendedWatcher = match watcher {
        Ok(w) => w,
        Err(error) => {
            record_monitor_failure(
                MonitorFailureKind::Start,
                "repo_monitor_thread initialize watcher",
                format!(
                    "repo_id={repo_id:?}, workdir={}: {error}",
                    workdir.display()
                ),
            );
            return;
        }
    };

    if let Err(error) = watcher
        .watch(&workdir, RecursiveMode::Recursive)
        .or_else(|_| watcher.watch(&workdir, RecursiveMode::NonRecursive))
    {
        record_monitor_failure(
            MonitorFailureKind::Start,
            "repo_monitor_thread watch workdir",
            format!(
                "repo_id={repo_id:?}, workdir={}: {error}",
                workdir.display()
            ),
        );
        return;
    }

    if let Some(git_dir) = &git_dir
        && let Err(error) = watcher
            .watch(git_dir, RecursiveMode::Recursive)
            .or_else(|_| watcher.watch(git_dir, RecursiveMode::NonRecursive))
        {
            record_monitor_failure(
                MonitorFailureKind::Start,
                "repo_monitor_thread watch git dir",
                format!(
                    "repo_id={repo_id:?}, workdir={}, git_dir={}: {error}",
                    workdir.display(),
                    git_dir.display()
                ),
            );
        }

    let debounce = Duration::from_millis(250);
    let max_delay = Duration::from_secs(2);
    let idle_tick = Duration::from_secs(30);

    let mut debouncer = DebouncedChange::new(debounce, max_delay);

    let flush = |change: RepoExternalChange| {
        if active_repo_id.load(Ordering::Relaxed) == repo_id.0 {
            send_or_log(
                &msg_tx,
                Msg::RepoExternallyChanged { repo_id, change },
                SendFailureKind::RepoMonitorMessage,
                "repo monitor flush",
            );
        }
    };

    let flush_if_active = |pending: Option<RepoExternalChange>| {
        if let Some(change) = pending
            && active_repo_id.load(Ordering::Relaxed) == repo_id.0
        {
            send_or_log(
                &msg_tx,
                Msg::RepoExternallyChanged { repo_id, change },
                SendFailureKind::RepoMonitorMessage,
                "repo monitor flush_if_active",
            );
        }
    };

    loop {
        let now = Instant::now();
        let timeout = debouncer.next_timeout(now).unwrap_or(idle_tick);

        match monitor_rx.recv_timeout(timeout) {
            Ok(MonitorMsg::Stop) => break,
            Ok(MonitorMsg::Event(Ok(event))) => {
                if let Some(change) =
                    classify_repo_event(&workdir, git_dir.as_deref(), &mut gitignore, &event)
                {
                    let now = Instant::now();
                    if let Some(to_flush) = debouncer.push(change, now) {
                        flush(to_flush);
                    }
                }
            }
            Ok(MonitorMsg::Event(Err(_))) => {
                let now = Instant::now();
                if let Some(to_flush) = debouncer.push(RepoExternalChange::Both, now) {
                    flush(to_flush);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                flush_if_active(debouncer.take_if_due(now));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn resolve_git_dir(workdir: &Path) -> Option<PathBuf> {
    let dot_git = workdir.join(".git");
    let md = fs::metadata(&dot_git).ok()?;

    if md.is_dir() {
        return Some(dot_git);
    }

    if !md.is_file() {
        return None;
    }

    let contents = fs::read_to_string(&dot_git).ok()?;
    let line = contents.lines().next()?.trim();
    let gitdir = line.strip_prefix("gitdir:")?.trim();
    if gitdir.is_empty() {
        return None;
    }

    let path = PathBuf::from(gitdir);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(workdir.join(path))
    }
}

fn merge_change(a: RepoExternalChange, b: RepoExternalChange) -> RepoExternalChange {
    use RepoExternalChange::*;
    match (a, b) {
        (Both, _) | (_, Both) => Both,
        (Worktree, GitState) | (GitState, Worktree) => Both,
        (Worktree, Worktree) => Worktree,
        (GitState, GitState) => GitState,
    }
}

fn classify_repo_event(
    workdir: &Path,
    git_dir: Option<&Path>,
    gitignore: &mut GitignoreRules,
    event: &notify::Event,
) -> Option<RepoExternalChange> {
    if should_ignore_event_kind(event) {
        return None;
    }

    // If notify indicates a rescan is needed, assume anything could have changed.
    if event.need_rescan() {
        return Some(RepoExternalChange::Both);
    }

    // Update ignore rules if the ignore config itself changes.
    if event
        .paths
        .iter()
        .any(|p| is_gitignore_config_path(workdir, git_dir, p))
    {
        *gitignore = GitignoreRules::load(workdir, git_dir);
        return Some(RepoExternalChange::Worktree);
    }

    if event.paths.is_empty() {
        return Some(RepoExternalChange::Both);
    }

    let mut saw_worktree = false;
    let mut saw_git = false;
    let is_dir_hint = path_dir_hint(event);

    for path in &event.paths {
        if is_git_related_path(workdir, git_dir, path) {
            // Treat `.git/index` updates like worktree changes: they typically reflect staging
            // operations and should not trigger branch list refreshes.
            if is_git_index_path(workdir, git_dir, path) {
                saw_worktree = true;
            } else {
                saw_git = true;
            }
        } else {
            if is_ignored_worktree_path_with_hint(workdir, gitignore, path, is_dir_hint) {
                continue;
            }
            saw_worktree = true;
        }
        if saw_git && saw_worktree {
            return Some(RepoExternalChange::Both);
        }
    }

    if saw_git {
        Some(RepoExternalChange::GitState)
    } else if saw_worktree {
        Some(RepoExternalChange::Worktree)
    } else {
        None
    }
}

fn is_git_related_path(workdir: &Path, git_dir: Option<&Path>, path: &Path) -> bool {
    let dot_git = workdir.join(".git");
    if path == dot_git || path.starts_with(&dot_git) {
        return true;
    }
    git_dir.is_some_and(|git_dir| path.starts_with(git_dir))
}

fn is_git_index_path(workdir: &Path, git_dir: Option<&Path>, path: &Path) -> bool {
    let dot_git = workdir.join(".git");
    if path == dot_git.join("index") || path == dot_git.join("index.lock") {
        return true;
    }

    if let Some(git_dir) = git_dir
        && (path == git_dir.join("index") || path == git_dir.join("index.lock"))
    {
        return true;
    }

    false
}

fn should_ignore_event_kind(event: &notify::Event) -> bool {
    match &event.kind {
        // Reading repo state should not cause a refresh loop; ignore access events except
        // close-after-write which indicates a write has completed.
        notify::EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
        notify::EventKind::Access(_) => true,
        _ => false,
    }
}

fn is_gitignore_config_path(workdir: &Path, git_dir: Option<&Path>, path: &Path) -> bool {
    if path == workdir.join(".gitignore") {
        return true;
    }
    git_dir.is_some_and(|git_dir| path == git_dir.join("info").join("exclude"))
}

fn is_ignored_worktree_path_with_hint(
    workdir: &Path,
    gitignore: &mut GitignoreRules,
    path: &Path,
    is_dir_hint: Option<bool>,
) -> bool {
    let Ok(rel) = path.strip_prefix(workdir) else {
        return false;
    };
    gitignore.is_ignored_rel(rel, is_dir_hint)
}

fn path_dir_hint(event: &notify::Event) -> Option<bool> {
    match &event.kind {
        notify::EventKind::Create(kind) => match kind {
            notify::event::CreateKind::Folder => Some(true),
            notify::event::CreateKind::File => Some(false),
            _ => None,
        },
        notify::EventKind::Remove(kind) => match kind {
            notify::event::RemoveKind::Folder => Some(true),
            notify::event::RemoveKind::File => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;
    use notify::event::{AccessKind, AccessMode, CreateKind};
    use std::process::Command;
    use std::time::SystemTime;

    #[cfg(windows)]
    const NULL_DEVICE: &str = "NUL";
    #[cfg(not(windows))]
    const NULL_DEVICE: &str = "/dev/null";

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .expect("run git command");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo_for_ignore_tests(workdir: &Path) {
        let _ = fs::create_dir_all(workdir);
        run_git(workdir, &["init"]);
        // Keep tests deterministic and independent from host global excludes.
        run_git(workdir, &["config", "core.excludesFile", NULL_DEVICE]);
        run_git(workdir, &["config", "core.fileMode", "false"]);
    }

    #[test]
    fn resolve_git_dir_handles_dot_git_directory() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        let _ = fs::create_dir_all(workdir.join(".git"));

        assert_eq!(resolve_git_dir(&workdir), Some(workdir.join(".git")));
    }

    #[test]
    fn resolve_git_dir_parses_dot_git_file() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        let gitdir = dir.join("actual-git-dir");
        let _ = fs::create_dir_all(&workdir);
        let _ = fs::create_dir_all(&gitdir);

        fs::write(
            workdir.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect("write .git file");

        assert_eq!(resolve_git_dir(&workdir), Some(gitdir));
    }

    #[test]
    fn merge_change_coalesces_to_both() {
        assert_eq!(
            merge_change(RepoExternalChange::Worktree, RepoExternalChange::GitState),
            RepoExternalChange::Both
        );
        assert_eq!(
            merge_change(RepoExternalChange::GitState, RepoExternalChange::Worktree),
            RepoExternalChange::Both
        );
        assert_eq!(
            merge_change(RepoExternalChange::Both, RepoExternalChange::Worktree),
            RepoExternalChange::Both
        );
        assert_eq!(
            merge_change(RepoExternalChange::GitState, RepoExternalChange::GitState),
            RepoExternalChange::GitState
        );
    }

    #[test]
    fn classify_repo_change_distinguishes_gitdir_from_worktree() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        let _ = fs::create_dir_all(workdir.join(".git"));

        let event = notify::Event {
            kind: EventKind::Any,
            paths: vec![workdir.join(".git").join("index")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            Some(RepoExternalChange::Worktree)
        );

        let event = notify::Event {
            kind: EventKind::Any,
            paths: vec![workdir.join("file.txt")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            Some(RepoExternalChange::Worktree)
        );

        let event = notify::Event {
            kind: EventKind::Any,
            paths: vec![workdir.join(".git").join("HEAD"), workdir.join("file.txt")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            Some(RepoExternalChange::Both)
        );
    }

    #[test]
    fn debouncer_flushes_on_debounce_or_max_delay() {
        let base = Instant::now();
        let mut d = DebouncedChange::new(Duration::from_millis(100), Duration::from_millis(250));

        assert_eq!(d.push(RepoExternalChange::Worktree, base), None);
        assert!(d.is_pending());

        // Another event resets debounce window.
        assert_eq!(
            d.push(
                RepoExternalChange::Worktree,
                base + Duration::from_millis(50)
            ),
            None
        );
        assert!(d.next_timeout(base + Duration::from_millis(50)).is_some());

        // Not yet due at 149ms from base.
        assert_eq!(d.take_if_due(base + Duration::from_millis(149)), None);

        // Due by debounce at 150ms from base (last at 50ms + 100ms).
        assert_eq!(
            d.take_if_due(base + Duration::from_millis(150)),
            Some(RepoExternalChange::Worktree)
        );
        assert!(!d.is_pending());

        // Continuous events should flush by max_delay.
        assert_eq!(d.push(RepoExternalChange::GitState, base), None);
        assert_eq!(
            d.push(
                RepoExternalChange::GitState,
                base + Duration::from_millis(300)
            ),
            Some(RepoExternalChange::GitState)
        );
        assert!(!d.is_pending());
    }

    #[test]
    fn access_events_do_not_trigger_refresh_loops() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        let _ = fs::create_dir_all(workdir.join(".git"));

        let event = notify::Event {
            kind: EventKind::Access(AccessKind::Open(AccessMode::Read)),
            paths: vec![workdir.join(".git").join("index")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            None
        );

        let event = notify::Event {
            kind: EventKind::Access(AccessKind::Close(AccessMode::Read)),
            paths: vec![workdir.join("file.txt")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            None
        );

        let event = notify::Event {
            kind: EventKind::Access(AccessKind::Close(AccessMode::Write)),
            paths: vec![workdir.join("file.txt")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(
                &workdir,
                Some(&workdir.join(".git")),
                &mut GitignoreRules::default(),
                &event
            ),
            Some(RepoExternalChange::Worktree)
        );
    }

    #[test]
    fn gitignore_rules_match_git_semantics_for_nested_negation_and_anchoring() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        init_repo_for_ignore_tests(&workdir);
        let git_dir = resolve_git_dir(&workdir);

        fs::write(
            workdir.join(".gitignore"),
            "target/\n*.gitcomet-log\n!keep.gitcomet-log\n/build/output\nlogs/*.tmp\n",
        )
        .expect("write .gitignore");
        fs::create_dir_all(workdir.join("logs")).expect("create logs directory");
        fs::write(workdir.join("logs/.gitignore"), "!keep.tmp\n").expect("write nested .gitignore");
        fs::write(
            git_dir
                .as_ref()
                .expect("git dir")
                .join("info")
                .join("exclude"),
            "info-excluded.gitcomet\n",
        )
        .expect("write .git/info/exclude");
        fs::create_dir_all(workdir.join("target")).expect("create target directory");

        let mut rules = GitignoreRules::load(&workdir, git_dir.as_deref());
        assert!(rules.is_ignored_rel(Path::new("target/debug/app"), Some(false)));
        assert!(rules.is_ignored_rel(Path::new("foo.gitcomet-log"), Some(false)));
        assert!(!rules.is_ignored_rel(Path::new("keep.gitcomet-log"), Some(false)));
        assert!(rules.is_ignored_rel(Path::new("build/output"), Some(false)));
        assert!(!rules.is_ignored_rel(Path::new("nested/build/output"), Some(false)));
        assert!(rules.is_ignored_rel(Path::new("logs/drop.tmp"), Some(false)));
        assert!(!rules.is_ignored_rel(Path::new("logs/keep.tmp"), Some(false)));
        assert!(rules.is_ignored_rel(Path::new("info-excluded.gitcomet"), Some(false)));
        assert!(rules.is_ignored_rel(Path::new("target"), Some(true)));

        // Ensure folder create events for ignored directories are treated as ignorable worktree
        // changes.
        let event = notify::Event {
            kind: EventKind::Create(CreateKind::Folder),
            paths: vec![workdir.join("target")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(&workdir, git_dir.as_deref(), &mut rules, &event),
            None
        );
    }

    #[test]
    fn tracked_paths_are_not_treated_as_ignored() {
        let dir = std::env::temp_dir().join(format!(
            "gitcomet-monitor-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let workdir = dir.join("repo");
        init_repo_for_ignore_tests(&workdir);
        let git_dir = resolve_git_dir(&workdir);

        fs::write(
            workdir.join(".gitignore"),
            "*.tracked-ignore\n*.untracked-ignore\n",
        )
        .expect("write .gitignore");
        fs::write(workdir.join("tracked.tracked-ignore"), "tracked\n").expect("write tracked file");
        fs::write(workdir.join("new.untracked-ignore"), "untracked\n").expect("write ignored file");

        run_git(&workdir, &["add", "-f", "tracked.tracked-ignore"]);

        let mut rules = GitignoreRules::load(&workdir, git_dir.as_deref());
        assert!(
            !rules.is_ignored_rel(Path::new("tracked.tracked-ignore"), Some(false)),
            "tracked paths must not be treated as ignored"
        );
        assert!(rules.is_ignored_rel(Path::new("new.untracked-ignore"), Some(false)));

        let tracked_event = notify::Event {
            kind: EventKind::Any,
            paths: vec![workdir.join("tracked.tracked-ignore")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(&workdir, git_dir.as_deref(), &mut rules, &tracked_event),
            Some(RepoExternalChange::Worktree)
        );

        let ignored_event = notify::Event {
            kind: EventKind::Any,
            paths: vec![workdir.join("new.untracked-ignore")],
            attrs: Default::default(),
        };
        assert_eq!(
            classify_repo_event(&workdir, git_dir.as_deref(), &mut rules, &ignored_event),
            None
        );
    }
}
