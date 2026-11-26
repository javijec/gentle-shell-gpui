//! The Git pane: a repository's commits as a graph, with their authors,
//! refs and changes, and the actions on them (fetch, pull, push, branches,
//! tags, stash, checkout, cherry-pick, revert, reset, rewording HEAD,
//! remotes, Git LFS), in a pane of its own.
//!
//! `git::history` reads the commits; `gentle_core::git::log` parses them and
//! lays out the graph's lanes; `graph` paints them; `view` renders the pane;
//! `ops` runs the actions and the prompts and confirmations they need.

mod avatars;
mod contributions;
mod graph;
mod ops;
mod view;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gentle_core::git::activity::{Activity, activity, count_days, local_day};
use gentle_core::git::ops::{CommitFile, PullMode};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use crate::features::git::{self, Commit, Graph, History, Scope};
use crate::platform::ides;

/// Commits read at first, and how many more "Earlier history" adds.
const PAGE: usize = 200;

/// One row of the list.
#[derive(Clone, Copy)]
enum Line {
    /// The work tree has changes on top of HEAD.
    Uncommitted,
    /// A commit by its index in the loaded history.
    Commit(usize),
    /// Older commits not loaded yet, by count.
    Earlier(usize),
}

/// The selected commit's message and changed files, read when it is picked.
struct Detail {
    hash: String,
    /// `None` while reading.
    message: Option<String>,
    files: Option<Rc<Vec<CommitFile>>>,
}

/// The last action's outcome, shown in the footer until the next one.
enum Outcome {
    Done(String),
    Failed(String),
}

/// What the activity strip shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// This repository's commits.
    Repository,
    /// The user's GitHub contribution calendar.
    GitHub,
}

pub struct HistoryView {
    toplevel: PathBuf,
    scope: Scope,
    limit: usize,
    /// `None` until the first read, `Some(None)` outside a repository.
    history: Option<Option<History>>,
    commits: Rc<Vec<Commit>>,
    graph: Rc<Graph>,
    lines: Rc<Vec<Line>>,
    /// Commit rows in `lines`, counted when they are laid out.
    matched: usize,
    selected: Option<String>,
    detail: Option<Detail>,
    loading: bool,
    /// Wall clock of the last read, Unix ms, for relative times.
    now_ms: i64,
    scroll: UniformListScrollHandle,
    /// The pane's width in the last frame: narrow panes drop columns.
    width: Rc<Cell<f32>>,
    focus: FocusHandle,
    /// Narrows the loaded commits by subject, author, hash or ref.
    filter: Entity<InputState>,
    query: String,
    pull_mode: PullMode,
    /// The action running, as the footer says it ("Pulling…").
    busy: Option<&'static str>,
    outcome: Option<Outcome>,
    prompt: Option<ops::Prompt>,
    /// Git LFS is installed; its commands show in the ⋯ menu.
    lfs: bool,
    /// Code editors installed, for "Open in …".
    ides: Rc<Vec<ides::Installed>>,
    /// What the activity strip shows.
    activity_source: Source,
    /// This repository's commits per local day; `None` until read.
    repo_days: Option<Rc<Vec<(i64, u32)>>>,
    /// The user's GitHub calendar: `None` until asked for, else what came
    /// back (or why it didn't).
    github: Option<Result<Rc<contributions::Contributions>, String>>,
    github_loading: bool,
    /// The strip, laid out from the shown source.
    activity: Option<Rc<Activity>>,
    /// The local UTC offset (seconds), for the strip's days.
    offset: i64,
    _load: Task<()>,
    _detail: Task<()>,
    _activity: Task<()>,
    _github: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for HistoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl HistoryView {
    /// `cwd` is any folder inside the repository.
    pub fn new(cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter commits"));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, input, event, cx| {
                if let InputEvent::Change = event {
                    this.query = input.read(cx).value().trim().to_lowercase();
                    this.relayout();
                    cx.notify();
                }
            }),
            // A picture arrived: the rows showing its author redraw.
            cx.observe_global::<avatars::Avatars>(|_, cx| cx.notify()),
        ];
        let mut this = Self {
            toplevel: cwd.clone(),
            scope: Scope::Head,
            limit: PAGE,
            history: None,
            commits: Rc::new(Vec::new()),
            graph: Rc::new(Graph::default()),
            lines: Rc::new(Vec::new()),
            matched: 0,
            selected: None,
            detail: None,
            loading: false,
            now_ms: 0,
            scroll: UniformListScrollHandle::new(),
            width: Rc::default(),
            focus: cx.focus_handle(),
            filter,
            query: String::new(),
            pull_mode: PullMode::default(),
            busy: None,
            outcome: None,
            prompt: None,
            lfs: false,
            ides: Rc::default(),
            activity_source: Source::Repository,
            repo_days: None,
            github: None,
            github_loading: false,
            activity: None,
            offset: 0,
            _load: Task::ready(()),
            _detail: Task::ready(()),
            _activity: Task::ready(()),
            _github: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        cx.spawn(async move |this, cx| {
            let (lfs, ides) = cx
                .background_executor()
                .spawn(async move {
                    let lfs = git::toplevel(&cwd).is_some_and(|top| git::ops::lfs_available(&top));
                    (lfs, ides::detect())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.lfs = lfs;
                this.ides = Rc::new(ides);
                cx.notify();
            });
        })
        .detach();
        this
    }

    /// The tab and pane label: `Git · <repository>`.
    pub fn title(&self) -> SharedString {
        match self.toplevel.file_name() {
            Some(name) => format!("Git · {}", name.to_string_lossy()).into(),
            None => "Git".into(),
        }
    }

    fn read(&self) -> Option<&History> {
        self.history.as_ref().and_then(|h| h.as_ref())
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let cwd = self.toplevel.clone();
        let scope = self.scope.clone();
        let limit = self.limit;
        self.loading = true;
        self.read_activity(cx);
        self._load = cx.spawn(async move |this, cx| {
            let history = cx
                .background_executor()
                .spawn(async move {
                    let toplevel = git::toplevel(&cwd)?;
                    git::history(&toplevel, &scope, limit).map(|h| (toplevel, h))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.apply(history, cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    /// Read the last year's commits for the activity strip (and the GitHub
    /// calendar again, when it is shown: the reload button refreshes it).
    fn read_activity(&mut self, cx: &mut Context<Self>) {
        let cwd = self.toplevel.clone();
        self._activity = cx.spawn(async move |this, cx| {
            let (times, offset) = cx
                .background_executor()
                .spawn(async move {
                    let times = git::toplevel(&cwd).map(|top| git::activity(&top));
                    (times, crate::features::usage::local_offset_secs())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.offset = offset;
                this.repo_days = times.map(|times| Rc::new(count_days(&times, offset)));
                this.layout_activity();
                cx.notify();
            });
        });
        if self.activity_source == Source::GitHub && self.github.is_some() {
            self.read_github(cx);
        }
    }

    /// Read the user's GitHub contribution calendar.
    fn read_github(&mut self, cx: &mut Context<Self>) {
        let cwd = self.toplevel.clone();
        let github = self.read().and_then(|h| h.github.clone());
        self.github_loading = true;
        self._github = cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move {
                    let top = git::toplevel(&cwd).unwrap_or(cwd);
                    contributions::read(&top, github)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.github_loading = false;
                this.github = Some(read.map(Rc::new));
                this.layout_activity();
                cx.notify();
            });
        });
    }

    /// Lay the strip out from the shown source.
    fn layout_activity(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let today = local_day(now, self.offset);
        let days = match self.activity_source {
            Source::Repository => self.repo_days.as_ref().map(|d| d.as_slice()),
            Source::GitHub => match &self.github {
                Some(Ok(calendar)) => Some(calendar.days.as_slice()),
                _ => None,
            },
        };
        self.activity = days.map(|days| Rc::new(activity(days, today)));
    }

    fn set_activity_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.activity_source == source {
            return;
        }
        self.activity_source = source;
        if source == Source::GitHub && self.github.is_none() && !self.github_loading {
            self.read_github(cx);
        }
        self.layout_activity();
        cx.notify();
    }

    fn apply(&mut self, read: Option<(PathBuf, History)>, cx: &mut Context<Self>) {
        self.now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let Some((toplevel, mut history)) = read else {
            self.history = Some(None);
            self.commits = Rc::new(Vec::new());
            self.graph = Rc::new(Graph::default());
            self.lines = Rc::new(Vec::new());
            self.detail = None;
            return;
        };
        self.toplevel = toplevel;
        self.scope = history.scope.clone();
        // The list reads the commits from `self.commits`; the rest of the
        // read stays in `self.history`.
        let commits = std::mem::take(&mut history.commits);
        self.history = Some(Some(history));
        self.show_commits(commits, cx);
        match &self.selected {
            Some(hash) if self.commits.iter().any(|c| &c.hash == hash) => {
                // The same commit, re-read: an action may have changed it.
                let hash = hash.clone();
                self.read_detail(hash, cx);
            }
            _ => {
                self.selected = None;
                self.detail = None;
            }
        }
    }

    /// List `commits` with their graph, and look up their authors' pictures.
    fn show_commits(&mut self, commits: Vec<Commit>, cx: &mut Context<Self>) {
        self.graph = Rc::new(gentle_core::git::log::layout(&commits));
        self.commits = Rc::new(commits);
        self.relayout();
        let github = self.read().and_then(|h| h.github.clone());
        let mut emails: Vec<&str> = self.commits.iter().map(|c| c.email.as_str()).collect();
        emails.sort_unstable();
        emails.dedup();
        avatars::request(emails, github, cx);
    }

    /// Rebuild the rows from the loaded commits and the filter.
    fn relayout(&mut self) {
        let Some(history) = self.read() else {
            self.lines = Rc::new(Vec::new());
            return;
        };
        let filtering = !self.query.is_empty();
        let mut lines = Vec::with_capacity(self.commits.len() + 2);
        if history.dirty && !self.commits.is_empty() && !filtering {
            lines.push(Line::Uncommitted);
        }
        lines.extend(
            self.commits
                .iter()
                .enumerate()
                .filter(|(_, c)| !filtering || c.matches(&self.query))
                .map(|(i, _)| Line::Commit(i)),
        );
        let earlier = history.total.saturating_sub(self.commits.len());
        if earlier > 0 && !filtering {
            lines.push(Line::Earlier(earlier));
        }
        self.matched = lines
            .iter()
            .filter(|l| matches!(l, Line::Commit(_)))
            .count();
        self.lines = Rc::new(lines);
    }

    fn filtering(&self) -> bool {
        !self.query.is_empty()
    }

    fn show(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.scope = scope;
        self.limit = PAGE;
        self.selected = None;
        self.detail = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.reload(cx);
    }

    /// Show `PAGE` more commits. Only the commits after the loaded ones are
    /// read, from the commit the last read started at, and added only if they
    /// continue it; else (every ref, a moved tip) everything is read again.
    /// The rest of the last read (total, branches, upstream) stays.
    fn load_earlier(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.limit += PAGE;
        let Some(tip) = self.commits.first().map(|c| c.hash.clone()) else {
            self.reload(cx);
            return;
        };
        let top = self.toplevel.clone();
        let scope = self.scope.clone();
        let skip = self.commits.len();
        let n = self.limit.saturating_sub(skip);
        self.loading = true;
        self._load = cx.spawn(async move |this, cx| {
            let page = cx
                .background_executor()
                .spawn(async move { git::earlier(&top, &scope, &tip, skip, n) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                let commits = page
                    .and_then(|page| gentle_core::git::log::append_page(&this.commits, skip, page));
                match commits {
                    Some(commits) => this.show_commits(commits, cx),
                    None => this.reload(cx),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn select(&mut self, hash: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() == Some(&hash) {
            self.selected = None;
            self.detail = None;
        } else {
            self.selected = Some(hash.clone());
            self.read_detail(hash, cx);
        }
        cx.notify();
    }

    fn selected_commit(&self) -> Option<&Commit> {
        let hash = self.selected.as_ref()?;
        self.commits.iter().find(|c| &c.hash == hash)
    }

    /// Read `hash`'s full message and changed files for the detail panel.
    fn read_detail(&mut self, hash: String, cx: &mut Context<Self>) {
        let Some(commit) = self.commits.iter().find(|c| c.hash == hash) else {
            return;
        };
        let parent = commit.parents.first().cloned();
        let top = self.toplevel.clone();
        self.detail = Some(Detail {
            hash: hash.clone(),
            message: None,
            files: None,
        });
        self._detail = cx.spawn(async move |this, cx| {
            let job_hash = hash.clone();
            let (message, files) = cx
                .background_executor()
                .spawn(async move {
                    let message = git::ops::message(&top, &job_hash).unwrap_or_default();
                    let files = git::ops::commit_files(&top, &job_hash, parent.as_deref());
                    (message, files)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(detail) = this.detail.as_mut().filter(|d| d.hash == hash) {
                    detail.message = Some(message);
                    detail.files = Some(Rc::new(files));
                    cx.notify();
                }
            });
        });
    }

    /// HEAD's commit selected and scrolled to, showing HEAD's history first
    /// when another branch is shown.
    fn jump_to_head(&mut self, cx: &mut Context<Self>) {
        if matches!(self.scope, Scope::Branch(_)) {
            self.show(Scope::Head, cx);
            return;
        }
        let Some(index) = self.commits.iter().position(|c| c.head) else {
            return;
        };
        let hash = self.commits[index].hash.clone();
        if let Some(row) = self
            .lines
            .iter()
            .position(|l| matches!(l, Line::Commit(i) if *i == index))
        {
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        }
        if self.selected.as_ref() != Some(&hash) {
            self.select(hash, cx);
        }
    }
}
