//! The project sidebar: the lazily loaded file tree (Tern-style context menu,
//! inline create and rename, keyboard navigation), the working-tree changes
//! and the branches, switched with icon tabs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::changes::{ChangesEvent, ChangesView};
use super::listing::{Entry, list_dir};
use super::ops::{self, ClipMode};
use super::refs::{RefsEvent, RefsView};
use super::{file_icon, status_color};
use crate::features::git::GitSnapshot;
use crate::platform::ides;
use crate::shared::{themes, ui};

const CONTEXT: &str = "FileTree";
const ROW_HEIGHT: f32 = 26.;
const INDENT: f32 = 14.;
const POLL: Duration = Duration::from_secs(3);

struct ShowIgnored {
    value: bool,
    path: Option<PathBuf>,
}

impl Global for ShowIgnored {}

/// The platform's name for showing a file in its folder.
pub const REVEAL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(target_os = "windows") {
    "Reveal in File Explorer"
} else {
    "Reveal in File Manager"
};

actions!(
    file_tree,
    [
        Open,
        OpenToSide,
        OpenInNewTab,
        OpenWithDefaultApp,
        InsertPath,
        CopyPath,
        CopyRelativePath,
        RevealInFinder,
        NewFile,
        NewFolder,
        Rename,
        Duplicate,
        CutEntry,
        CopyEntry,
        PasteEntry,
        MoveToTrash,
        SelectPrev,
        SelectNext,
        CollapseSelected,
        ExpandSelected,
    ]
);

/// The tree's shortcuts, live while it has focus (the context menu shows them).
pub fn init(cx: &mut App) {
    let path = themes::store_path();
    let value = path.as_deref().is_some_and(read_show_ignored);
    cx.set_global(ShowIgnored { value, path });

    // Cmd on macOS, Ctrl elsewhere, as main.rs binds the app shortcuts
    // (this gpui has no `secondary-` modifier: it panics at startup).
    macro_rules! key {
        ($prefix:literal, $rest:literal) => {
            if cfg!(target_os = "macos") {
                concat!($prefix, "cmd-", $rest)
            } else {
                concat!($prefix, "ctrl-", $rest)
            }
        };
    }
    let trash = if cfg!(target_os = "macos") {
        "cmd-backspace"
    } else {
        "delete"
    };
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", Open, ctx),
        KeyBinding::new(key!("", "enter"), OpenToSide, ctx),
        KeyBinding::new(key!("alt-", "enter"), OpenInNewTab, ctx),
        KeyBinding::new("alt-enter", InsertPath, ctx),
        KeyBinding::new(key!("alt-", "c"), CopyPath, ctx),
        KeyBinding::new(key!("alt-shift-", "c"), CopyRelativePath, ctx),
        KeyBinding::new(key!("alt-", "r"), RevealInFinder, ctx),
        KeyBinding::new("f2", Rename, ctx),
        KeyBinding::new(key!("", "x"), CutEntry, ctx),
        KeyBinding::new(key!("", "c"), CopyEntry, ctx),
        KeyBinding::new(key!("", "v"), PasteEntry, ctx),
        KeyBinding::new(trash, MoveToTrash, ctx),
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("left", CollapseSelected, ctx),
        KeyBinding::new("right", ExpandSelected, ctx),
    ]);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileTreeEvent {
    /// Show a file in the tab's preview pane (a single or double click).
    Open(PathBuf),
    /// Show a file in a new split beside the focused pane.
    OpenToSide(PathBuf),
    /// Show a file in a tab of its own.
    OpenInNewTab(PathBuf),
    /// Show a file's diff against HEAD.
    OpenDiff(PathBuf),
    /// Type this at the active agent prompt's cursor.
    InsertPath(String),
    /// Open an agent tab in this folder (a worktree).
    OpenAgent(PathBuf),
    /// Show the Git pane of this repository.
    OpenHistory(PathBuf),
    /// The header's folder name: the owner should let the user pick another
    /// root folder.
    ChooseFolder,
    /// The search field: the owner should open its file finder.
    SearchFiles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Files,
    Changes,
    Branches,
}

#[derive(Clone)]
enum Item {
    Entry(Entry),
    /// The name field of a file or folder being created.
    Draft {
        is_dir: bool,
    },
}

#[derive(Clone)]
struct Row {
    item: Item,
    depth: usize,
}

#[derive(Clone, PartialEq)]
enum EditTarget {
    Rename(PathBuf),
    New { dir: PathBuf, is_dir: bool },
}

/// An inline name field: renaming a row or naming a new entry.
struct Editing {
    target: EditTarget,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The repository the root lives in and its working-tree state.
#[derive(Clone, PartialEq)]
struct GitState {
    /// `git status` paths are relative to this, not to the tree root.
    toplevel: PathBuf,
    snapshot: GitSnapshot,
}

pub struct FileTreeView {
    root: PathBuf,
    name: SharedString,
    tab: Tab,
    last_git_tab: Tab,
    original_tabs: bool,
    /// Loaded listings by folder; folders load when first expanded.
    children: HashMap<PathBuf, Vec<Entry>>,
    expanded: HashSet<PathBuf>,
    /// The row actions apply to; `None` means the root folder.
    selected: Option<PathBuf>,
    /// The visible tree, flattened; rebuilt when listings or expansion change.
    rows: Rc<Vec<Row>>,
    show_ignored: bool,
    clipboard: Option<(PathBuf, ClipMode)>,
    editing: Option<Editing>,
    /// `None` until the first poll, `Some(None)` outside a git work tree.
    git: Option<Option<GitState>>,
    /// Status letter per absolute path, and every folder containing a change.
    changed: HashMap<PathBuf, char>,
    dirty_dirs: HashSet<PathBuf>,
    visible: bool,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    changes: Entity<ChangesView>,
    refs: Entity<RefsView>,
    /// Code editors installed here, for the ⋯ menu's "Open in …".
    ides: Rc<Vec<ides::Installed>>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FileTreeEvent> for FileTreeView {}

impl Focusable for FileTreeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FileTreeView {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string())
            .into();
        let changes = cx.new(|cx| ChangesView::new(root.clone(), window, cx));
        let refs = cx.new(|cx| RefsView::new(root.clone(), window, cx));
        let subscriptions = vec![
            cx.observe_global::<ShowIgnored>(|this, cx| {
                let value = cx.global::<ShowIgnored>().value;
                this.apply_show_ignored(value, cx);
            }),
            cx.subscribe(&changes, |_, _, event: &ChangesEvent, cx| match event {
                ChangesEvent::OpenDiff(path) => cx.emit(FileTreeEvent::OpenDiff(path.clone())),
                ChangesEvent::OpenFile(path) => cx.emit(FileTreeEvent::Open(path.clone())),
                ChangesEvent::Changed => cx.notify(),
            }),
            cx.subscribe(&refs, |_, _, event: &RefsEvent, cx| match event {
                RefsEvent::OpenAgent(path) => cx.emit(FileTreeEvent::OpenAgent(path.clone())),
                RefsEvent::OpenHistory(path) => cx.emit(FileTreeEvent::OpenHistory(path.clone())),
                RefsEvent::Changed => cx.notify(),
            }),
        ];
        let mut this = Self {
            root,
            name,
            tab: Tab::Files,
            last_git_tab: Tab::Changes,
            original_tabs: true,
            children: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            rows: Rc::default(),
            show_ignored: cx.global::<ShowIgnored>().value,
            clipboard: None,
            editing: None,
            git: None,
            changed: HashMap::new(),
            dirty_dirs: HashSet::new(),
            visible: true,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            changes,
            refs,
            ides: Rc::default(),
            _poll: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.sync_panels(cx);
        this.load_dir(this.root.clone(), cx);
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async { ides::detect() })
                .await;
            let _ = this.update(cx, |this, _| this.ides = Rc::new(found));
        })
        .detach();
        this._poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                // The status decorates the Files tab: only while it is shown
                // and the window is in front.
                let Ok(due) = this.update_in(cx, |this, window, _| {
                    (this.visible && this.tab == Tab::Files && window.is_window_active())
                        .then(|| this.git_job())
                }) else {
                    break;
                };
                if let Some(job) = due {
                    let state = cx.background_executor().spawn(job).await;
                    if this
                        .update(cx, |this, cx| this.apply_git(state, cx))
                        .is_err()
                    {
                        break;
                    }
                }
                cx.background_executor().timer(POLL).await;
                crate::shared::diagnostics::tick();
            }
        });
        this
    }

    /// Re-list the root and every expanded folder, and re-read git status.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload_listings(cx);
        self.refresh_git(cx);
    }

    /// Pause polling while the sidebar is hidden; showing it refreshes.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let shown = visible && !self.visible;
        self.visible = visible;
        if shown && self.tab == Tab::Files {
            self.refresh(cx);
        }
        self.sync_panels(cx);
    }

    /// Only the shown Git panel polls.
    fn sync_panels(&mut self, cx: &mut Context<Self>) {
        let changes = self.visible && self.tab == Tab::Changes;
        let refs = self.visible && self.tab == Tab::Branches;
        self.changes
            .update(cx, |view, cx| view.set_visible(changes, cx));
        self.refs.update(cx, |view, cx| view.set_visible(refs, cx));
    }

    /// The workspace activity rail selects the existing Files or Git view.
    pub fn set_original_tabs(&mut self, original: bool, cx: &mut Context<Self>) {
        if self.original_tabs != original {
            self.original_tabs = original;
            cx.notify();
        }
    }

    pub fn is_git_tab(&self) -> bool {
        self.tab != Tab::Files
    }

    pub fn select_sidebar_tool(
        &mut self,
        git: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = if git { self.last_git_tab } else { Tab::Files };
        self.set_tab(tab, window, cx);
    }

    fn set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.finish_edit(false, window, cx);
        self.tab = tab;
        if tab != Tab::Files {
            self.last_git_tab = tab;
        }
        if tab == Tab::Files {
            self.refresh(cx);
        }
        self.sync_panels(cx);
        cx.notify();
    }

    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let job = self.git_job();
        cx.spawn(async move |this, cx| {
            let state = cx.background_executor().spawn(job).await;
            let _ = this.update(cx, |this, cx| this.apply_git(state, cx));
        })
        .detach();
    }

    fn reload_listings(&mut self, cx: &mut Context<Self>) {
        let mut dirs: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        dirs.push(self.root.clone());
        for dir in dirs {
            self.load_dir(dir, cx);
        }
    }

    /// Re-list the folders among `dirs` the tree shows.
    fn reload_dirs(&mut self, dirs: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for dir in dirs {
            if dir == self.root || self.expanded.contains(&dir) {
                self.load_dir(dir, cx);
            }
        }
    }

    fn load_dir(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let show_ignored = self.show_ignored;
        cx.spawn(async move |this, cx| {
            let listed = dir.clone();
            let entries = cx
                .background_executor()
                .spawn(async move { list_dir(&listed, show_ignored) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A listing from before the ignored toggle flipped is stale.
                if this.show_ignored == show_ignored && this.children.get(&dir) != Some(&entries) {
                    this.children.insert(dir, entries);
                    this.rebuild_rows();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn rebuild_rows(&mut self) {
        fn walk(
            this: &FileTreeView,
            dir: &Path,
            depth: usize,
            draft: Option<(&Path, bool)>,
            out: &mut Vec<Row>,
        ) {
            if let Some((at, is_dir)) = draft
                && at == dir
            {
                out.push(Row {
                    item: Item::Draft { is_dir },
                    depth,
                });
            }
            let Some(entries) = this.children.get(dir) else {
                return;
            };
            for entry in entries {
                out.push(Row {
                    item: Item::Entry(entry.clone()),
                    depth,
                });
                if entry.is_dir && this.expanded.contains(&entry.path) {
                    walk(this, &entry.path, depth + 1, draft, out);
                }
            }
        }
        let draft = match self.editing.as_ref().map(|e| &e.target) {
            Some(EditTarget::New { dir, is_dir }) => Some((dir.as_path(), *is_dir)),
            _ => None,
        };
        let mut rows = Vec::new();
        walk(self, &self.root, 0, draft, &mut rows);
        self.rows = Rc::new(rows);
    }

    fn git_job(&self) -> impl Future<Output = Option<GitState>> + Send + 'static + use<> {
        let root = self.root.clone();
        let toplevel = self
            .git
            .as_ref()
            .and_then(|g| g.as_ref())
            .map(|g| g.toplevel.clone());
        async move {
            let toplevel = toplevel.or_else(|| show_toplevel(&root))?;
            let snapshot = crate::features::git::snapshot(&root)?;
            Some(GitState { toplevel, snapshot })
        }
    }

    fn apply_git(&mut self, state: Option<GitState>, cx: &mut Context<Self>) {
        if self.git.as_ref() == Some(&state) {
            return;
        }
        let paths_changed = self.git.as_ref().and_then(|g| g.as_ref()).map(|g| paths(g))
            != state.as_ref().map(paths);
        self.changed.clear();
        self.dirty_dirs.clear();
        if let Some(state) = &state {
            for file in &state.snapshot.files {
                let abs = state.toplevel.join(&file.path);
                for dir in abs.ancestors().skip(1) {
                    if !dir.starts_with(&self.root) || !self.dirty_dirs.insert(dir.to_path_buf()) {
                        break;
                    }
                }
                self.changed.insert(abs, file.status);
            }
        }
        let first = self.git.is_none();
        self.git = Some(state);
        // Files appearing or disappearing show up in git status first.
        if paths_changed && !first {
            self.reload_listings(cx);
        }
        cx.notify();
    }

    // ── Selection ─────────────────────────────────────────────────────────

    /// The listed entry at `path`, wherever it is loaded.
    fn entry(&self, path: &Path) -> Option<&Entry> {
        self.children
            .get(path.parent()?)?
            .iter()
            .find(|e| e.path == path)
    }

    /// What actions apply to: the selected entry, else the root folder.
    fn target(&self) -> (PathBuf, bool) {
        self.selected
            .as_deref()
            .and_then(|p| self.entry(p))
            .map(|e| (e.path.clone(), e.is_dir))
            .unwrap_or_else(|| (self.root.clone(), true))
    }

    /// The selected entry, unless the root is the target.
    fn target_entry(&self) -> Option<(PathBuf, bool)> {
        let (path, is_dir) = self.target();
        (path != self.root).then_some((path, is_dir))
    }

    fn row_index(&self, path: &Path) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| matches!(&r.item, Item::Entry(e) if e.path == path))
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(Row {
            item: Item::Entry(entry),
            ..
        }) = self.rows.get(ix)
        {
            self.selected = Some(entry.path.clone());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected.as_deref().and_then(|p| self.row_index(p));
        let ix = match current {
            Some(ix) => ix.saturating_sub(1),
            None => self.rows.len().saturating_sub(1),
        };
        self.select_row(ix, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        let current = self.selected.as_deref().and_then(|p| self.row_index(p));
        let ix = match current {
            Some(ix) => (ix + 1).min(self.rows.len().saturating_sub(1)),
            None => 0,
        };
        self.select_row(ix, cx);
    }

    /// ←: fold an open folder, else go to the parent folder.
    fn collapse_selected(&mut self, _: &CollapseSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.target_entry() else {
            return;
        };
        if is_dir && self.expanded.contains(&path) {
            self.toggle_dir(&path, cx);
        } else if let Some(ix) = path.parent().and_then(|p| self.row_index(p)) {
            self.select_row(ix, cx);
        }
    }

    /// →: unfold a folder, else step into its first child.
    fn expand_selected(&mut self, _: &ExpandSelected, _: &mut Window, cx: &mut Context<Self>) {
        let Some((path, true)) = self.target_entry() else {
            return;
        };
        if !self.expanded.contains(&path) {
            self.toggle_dir(&path, cx);
        } else if let Some(ix) = self.row_index(&path)
            && self
                .rows
                .get(ix + 1)
                .is_some_and(|r| r.depth > self.rows[ix].depth)
        {
            self.select_row(ix + 1, cx);
        }
    }

    fn toggle_dir(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
            if !self.children.contains_key(path) {
                self.load_dir(path.to_path_buf(), cx);
            }
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.rebuild_rows();
        cx.notify();
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        let value = !cx.global::<ShowIgnored>().value;
        let path = cx.global::<ShowIgnored>().path.clone();
        cx.set_global(ShowIgnored {
            value,
            path: path.clone(),
        });
        if let Some(path) = path
            && let Err(err) = write_show_ignored(&path, value)
        {
            eprintln!("gentle-shell-gpui: could not save Show Ignored Files: {err}");
        }
        self.apply_show_ignored(value, cx);
    }

    fn apply_show_ignored(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.show_ignored == value {
            return;
        }
        self.show_ignored = value;
        // Closed folders must not reuse listings from the previous setting.
        self.children
            .retain(|dir, _| dir == &self.root || self.expanded.contains(dir));
        self.reload_listings(cx);
        cx.notify();
    }

    /// A click on a row: folders toggle; a file opens in the tab's preview
    /// pane. A double click's second press needs no second open.
    fn click_entry(
        &mut self,
        entry: &Entry,
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(entry.path.clone());
        self.focus.focus(window, cx);
        match click_outcome(entry.is_dir, clicks) {
            ClickOutcome::Toggle => self.toggle_dir(&entry.path, cx),
            ClickOutcome::Open => cx.emit(FileTreeEvent::Open(entry.path.clone())),
            ClickOutcome::Nothing => {}
        }
        cx.notify();
    }

    // ── Actions ───────────────────────────────────────────────────────────

    fn open(&mut self, _: &Open, _: &mut Window, cx: &mut Context<Self>) {
        match self.target_entry() {
            Some((path, true)) => self.toggle_dir(&path, cx),
            Some((path, false)) => cx.emit(FileTreeEvent::Open(path)),
            None => {}
        }
    }

    fn open_to_side(&mut self, _: &OpenToSide, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, false)) = self.target_entry() {
            cx.emit(FileTreeEvent::OpenToSide(path));
        }
    }

    fn open_in_new_tab(&mut self, _: &OpenInNewTab, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, false)) = self.target_entry() {
            cx.emit(FileTreeEvent::OpenInNewTab(path));
        }
    }

    fn open_with_default_app(
        &mut self,
        _: &OpenWithDefaultApp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (path, _) = self.target();
        cx.open_with_system(&path);
    }

    fn insert_path(&mut self, _: &InsertPath, _: &mut Window, cx: &mut Context<Self>) {
        let (path, is_dir) = self.target();
        let mut text = ops::relative_path(&self.root, &path);
        if is_dir {
            text.push('/');
        }
        cx.emit(FileTreeEvent::InsertPath(text));
    }

    fn copy_path(&mut self, _: &CopyPath, _: &mut Window, cx: &mut Context<Self>) {
        let (path, _) = self.target();
        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
    }

    fn copy_relative_path(&mut self, _: &CopyRelativePath, _: &mut Window, cx: &mut Context<Self>) {
        let (path, _) = self.target();
        cx.write_to_clipboard(ClipboardItem::new_string(ops::relative_path(
            &self.root, &path,
        )));
    }

    fn reveal(&mut self, _: &RevealInFinder, _: &mut Window, cx: &mut Context<Self>) {
        let (path, _) = self.target();
        cx.reveal_path(&path);
    }

    fn new_file(&mut self, _: &NewFile, window: &mut Window, cx: &mut Context<Self>) {
        self.start_new(false, window, cx);
    }

    fn new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.start_new(true, window, cx);
    }

    fn rename(&mut self, _: &Rename, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.target_entry() else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Like Finder: the name is selected, the extension is not.
        let stem = match name.rfind('.') {
            Some(dot) if !is_dir && dot > 0 => dot,
            _ => name.len(),
        };
        self.begin_edit(EditTarget::Rename(path), &name, "", window, cx);
        if let Some(editing) = &self.editing {
            editing
                .input
                .update(cx, |state, cx| state.set_selected_range(0..stem, cx));
        }
    }

    fn duplicate(&mut self, _: &Duplicate, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, is_dir)) = self.target_entry() else {
            return;
        };
        let parent = parent_of(&path);
        self.run(
            vec![parent],
            move || {
                let dest = ops::copy_name(&path, is_dir, |p| p.symlink_metadata().is_ok());
                ops::copy_all(&path, &dest)
                    .map(|_| dest)
                    .map_err(|e| format!("Couldn't duplicate “{}”: {e}", file_name(&path)))
            },
            |this, dest, _, cx| {
                this.selected = Some(dest);
                cx.notify();
            },
            window,
            cx,
        );
    }

    fn cut(&mut self, _: &CutEntry, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, _)) = self.target_entry() {
            self.clipboard = Some((path, ClipMode::Cut));
            cx.notify();
        }
    }

    fn copy(&mut self, _: &CopyEntry, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((path, _)) = self.target_entry() {
            self.clipboard = Some((path, ClipMode::Copy));
            cx.notify();
        }
    }

    fn paste(&mut self, _: &PasteEntry, window: &mut Window, cx: &mut Context<Self>) {
        let Some((source, mode)) = self.clipboard.clone() else {
            return;
        };
        let (target, target_is_dir) = self.target();
        let dir = ops::paste_dir(&target, target_is_dir, &source);
        let mut dirs = vec![dir.clone()];
        if mode == ClipMode::Cut {
            dirs.push(parent_of(&source));
        }
        let into = dir.clone();
        self.run(
            dirs,
            move || {
                let exists = |p: &Path| p.symlink_metadata().is_ok();
                if !exists(&source) {
                    return Err(format!("“{}” no longer exists", file_name(&source)));
                }
                let Some(dest) =
                    ops::paste_destination(&source, source.is_dir(), &into, mode, exists)?
                else {
                    return Ok(None);
                };
                match mode {
                    ClipMode::Copy => ops::copy_all(&source, &dest),
                    ClipMode::Cut => ops::move_entry(&source, &dest),
                }
                .map_err(|e| format!("Couldn't paste “{}”: {e}", file_name(&source)))?;
                Ok(Some((source, dest)))
            },
            move |this, moved, _, cx| {
                let Some((source, dest)) = moved else {
                    return;
                };
                if mode == ClipMode::Cut {
                    this.moved(&source, &dest);
                    this.clipboard = None;
                }
                if dir != this.root && this.expanded.insert(dir.clone()) {
                    this.load_dir(dir, cx);
                }
                this.selected = Some(dest);
                this.rebuild_rows();
                cx.notify();
            },
            window,
            cx,
        );
    }

    fn move_to_trash(&mut self, _: &MoveToTrash, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, _)) = self.target_entry() else {
            return;
        };
        let parent = parent_of(&path);
        // Keep the place in the list: the next row, else the previous one.
        let next = self.row_index(&path).and_then(|ix| {
            let depth = self.rows[ix].depth;
            let past = self.rows[ix + 1..]
                .iter()
                .find(|r| r.depth <= depth)
                .or_else(|| self.rows[..ix].last());
            match past.map(|r| &r.item) {
                Some(Item::Entry(e)) => Some(e.path.clone()),
                _ => None,
            }
        });
        self.run(
            vec![parent],
            move || {
                ops::trash(&path)
                    .map(|_| path.clone())
                    .map_err(|e| format!("Couldn't move “{}” to the Trash: {e}", file_name(&path)))
            },
            move |this, path, _, cx| {
                this.forget(&path);
                this.selected = next;
                this.rebuild_rows();
                cx.notify();
            },
            window,
            cx,
        );
    }

    /// Run a filesystem `job` off the UI thread, then re-list `dirs` and
    /// re-read git status. Failures show as a warning toast.
    fn run<T: Send + 'static>(
        &mut self,
        dirs: Vec<PathBuf>,
        job: impl FnOnce() -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(value) => done(this, value, window, cx),
                    Err(message) => window.push_notification(Notification::warning(message), cx),
                }
                this.reload_dirs(dirs, cx);
                this.refresh_git(cx);
            });
        })
        .detach();
    }

    /// `from` now lives at `to`: open folders, the selection and the
    /// clipboard follow it; its cached listings are dropped and re-read.
    fn moved(&mut self, from: &Path, to: &Path) {
        let follow = |path: &Path| -> Option<PathBuf> {
            let rest = path.strip_prefix(from).ok()?;
            Some(if rest.as_os_str().is_empty() {
                to.to_path_buf()
            } else {
                to.join(rest)
            })
        };
        self.expanded = self
            .expanded
            .drain()
            .map(|p| follow(&p).unwrap_or(p))
            .collect();
        self.children.retain(|dir, _| !dir.starts_with(from));
        if let Some(path) = self.selected.as_deref().and_then(follow) {
            self.selected = Some(path);
        }
        if let Some((path, mode)) = &self.clipboard
            && let Some(path) = follow(path)
        {
            self.clipboard = Some((path, *mode));
        }
    }

    /// `path` is gone: drop what the tree remembers under it.
    fn forget(&mut self, path: &Path) {
        self.expanded.retain(|p| !p.starts_with(path));
        self.children.retain(|dir, _| !dir.starts_with(path));
        if self
            .selected
            .as_deref()
            .is_some_and(|p| p.starts_with(path))
        {
            self.selected = None;
        }
        if self
            .clipboard
            .as_ref()
            .is_some_and(|(p, _)| p.starts_with(path))
        {
            self.clipboard = None;
        }
    }

    // ── Inline name field ─────────────────────────────────────────────────

    /// A name field for a new file or folder, in the selected folder (or the
    /// selected file's).
    fn start_new(&mut self, is_dir: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (target, target_is_dir) = self.target();
        let dir = if target_is_dir {
            target
        } else {
            parent_of(&target)
        };
        if dir != self.root
            && self.expanded.insert(dir.clone())
            && !self.children.contains_key(&dir)
        {
            self.load_dir(dir.clone(), cx);
        }
        let placeholder = if is_dir { "Folder name" } else { "File name" };
        self.begin_edit(EditTarget::New { dir, is_dir }, "", placeholder, window, cx);
        if let Some(ix) = self
            .rows
            .iter()
            .position(|r| matches!(r.item, Item::Draft { .. }))
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
        }
    }

    fn begin_edit(
        &mut self,
        target: EditTarget,
        value: &str,
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_edit(false, window, cx);
        let value = value.to_string();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(value)
                .placeholder(placeholder)
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                // Enter commits, and so does clicking away (like editors do).
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.finish_edit(true, window, cx)
                }
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.editing = Some(Editing {
            target,
            input,
            _subscription: subscription,
        });
        self.rebuild_rows();
        cx.notify();
    }

    fn cancel_edit(&mut self, _: &Escape, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_edit(false, window, cx);
    }

    fn finish_edit(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        // Enter and Escape hand the keyboard back to the tree; a click
        // elsewhere keeps its new focus.
        if editing.input.focus_handle(cx).is_focused(window) {
            self.focus.focus(window, cx);
        }
        let typed = editing.input.read(cx).value().to_string();
        self.rebuild_rows();
        cx.notify();
        if !commit {
            return;
        }
        match editing.target {
            EditTarget::Rename(from) => {
                let name = match ops::check_name(&typed) {
                    Ok(name) => name,
                    Err(message) => {
                        window.push_notification(Notification::warning(message), cx);
                        return;
                    }
                };
                if from.file_name().is_some_and(|n| n == name) {
                    return;
                }
                let to = from.with_file_name(name);
                self.run(
                    vec![parent_of(&from)],
                    move || {
                        ops::rename(&from, &to)
                            .map(|_| (from.clone(), to))
                            .map_err(|e| format!("Couldn't rename “{}”: {e}", file_name(&from)))
                    },
                    |this, (from, to), _, cx| {
                        this.moved(&from, &to);
                        this.selected = Some(to.clone());
                        let reopened: Vec<PathBuf> = this
                            .expanded
                            .iter()
                            .filter(|p| p.starts_with(&to))
                            .cloned()
                            .collect();
                        this.reload_dirs(reopened, cx);
                        this.rebuild_rows();
                        cx.notify();
                    },
                    window,
                    cx,
                );
            }
            EditTarget::New { dir, is_dir } => {
                if typed.trim().is_empty() {
                    return;
                }
                let path = match ops::new_entry_path(&dir, &typed) {
                    Ok(path) => path,
                    Err(message) => {
                        window.push_notification(Notification::warning(message), cx);
                        return;
                    }
                };
                let created = path.clone();
                self.run(
                    vec![dir.clone()],
                    move || {
                        ops::create(&created, is_dir)
                            .map(|_| created.clone())
                            .map_err(|e| format!("Couldn't create “{}”: {e}", file_name(&created)))
                    },
                    move |this, path, _, cx| {
                        // `a/b/c.rs` shows with its new folders open.
                        let folders: Vec<PathBuf> = path
                            .ancestors()
                            .skip(1)
                            .take_while(|p| *p != dir)
                            .map(Path::to_path_buf)
                            .collect();
                        for folder in folders {
                            if this.expanded.insert(folder.clone()) {
                                this.load_dir(folder, cx);
                            }
                        }
                        this.selected = Some(path.clone());
                        if !is_dir {
                            cx.emit(FileTreeEvent::Open(path));
                        }
                        cx.notify();
                    },
                    window,
                    cx,
                );
            }
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    fn render_row(&self, row: &Row, cx: &mut Context<Self>) -> Stateful<Div> {
        let entry = match &row.item {
            Item::Entry(entry) => entry,
            Item::Draft { is_dir } => {
                let icon = if *is_dir {
                    (IconName::Folder, ui::muted())
                } else {
                    (IconName::File, ui::muted())
                };
                return row_shell("file-tree-draft".into(), row.depth, true)
                    .child(div().w(rems(14. / 16.)))
                    .child(Icon::new(icon.0).small().text_color(icon.1))
                    .children(self.render_name_field(cx));
            }
        };
        let selected = self.selected.as_ref() == Some(&entry.path);
        let open = entry.is_dir && self.expanded.contains(&entry.path);
        let status = self.changed.get(&entry.path).copied();
        let dirty = entry.is_dir && self.dirty_dirs.contains(&entry.path);
        let cut = matches!(&self.clipboard, Some((p, ClipMode::Cut)) if *p == entry.path);
        let renaming = matches!(
            self.editing.as_ref().map(|e| &e.target),
            Some(EditTarget::Rename(p)) if *p == entry.path
        );
        let dim = entry.hidden || entry.ignored || cut;
        let (icon, tint) = if entry.is_dir {
            (
                if open {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                },
                ui::muted(),
            )
        } else {
            file_icon(&entry.name)
        };
        let name_color = match status {
            Some(s) if !cut => status_color(s),
            _ if dim => ui::faint(),
            _ => ui::fg(),
        };
        let id = SharedString::from(entry.path.to_string_lossy().into_owned());
        let shell = row_shell(id, row.depth, selected || renaming)
            .child(
                div()
                    .w(rems(14. / 16.))
                    .flex()
                    .justify_center()
                    .when(entry.is_dir, |d| {
                        d.child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall()
                            .text_color(ui::faint()),
                        )
                    }),
            )
            .child(
                Icon::new(icon)
                    .small()
                    .text_color(if dim { tint.opacity(0.55) } else { tint }),
            );
        if renaming {
            return shell.children(self.render_name_field(cx));
        }
        let click = entry.clone();
        let menu_target = entry.path.clone();
        let full_name = entry.name.clone();
        shell
            .tooltip(move |window, cx| Tooltip::new(full_name.clone()).build(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(rems(13. / 16.))
                    .text_color(name_color)
                    .when(selected, |d| d.font_weight(FontWeight::MEDIUM))
                    .child(entry.name.clone()),
            )
            .when(dirty, |d| {
                d.child(
                    div()
                        .size(rems(5. / 16.))
                        .rounded_full()
                        .bg(ui::warning().opacity(0.8)),
                )
            })
            .when_some(status, |d, s| {
                d.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(10.5))
                        .text_color(status_color(s))
                        .child(status_letter(s).to_string()),
                )
            })
            .cursor_pointer()
            // Bubbles before the context menu opens, so it acts on this row.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.selected = Some(menu_target.clone());
                    this.focus.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_entry(&click, event.click_count(), window, cx)
            }))
    }

    /// The inline name field of the edit in progress.
    fn render_name_field(&self, cx: &mut Context<Self>) -> Option<Div> {
        let editing = self.editing.as_ref()?;
        Some(
            div()
                .flex_1()
                .min_w_0()
                .h(rems(20. / 16.))
                .px_1()
                .flex()
                .items_center()
                .rounded(rems(5. / 16.))
                .bg(ui::well())
                .text_size(rems(13. / 16.))
                .on_action(cx.listener(Self::cancel_edit))
                .child(Input::new(&editing.input).appearance(false).xsmall()),
        )
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = if self.rows.is_empty() {
            let text = if self.children.contains_key(&self.root) {
                "This folder is empty"
            } else {
                "Loading…"
            };
            empty_state(IconName::FolderOpen, text).into_any_element()
        } else {
            uniform_list(
                "file-tree-rows",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let rows = this.rows.clone();
                    rows[range]
                        .iter()
                        .map(|row| this.render_row(row, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .p_1p5()
            .track_scroll(&self.scroll)
            .into_any_element()
        };
        let tree = cx.entity().downgrade();
        div()
            .id("file-tree-files")
            .track_focus(&self.focus)
            // Not while a name field is open: its keys are its own.
            .when(self.editing.is_none(), |d| d.key_context(CONTEXT))
            .on_action(cx.listener(Self::open))
            .on_action(cx.listener(Self::open_to_side))
            .on_action(cx.listener(Self::open_in_new_tab))
            .on_action(cx.listener(Self::open_with_default_app))
            .on_action(cx.listener(Self::insert_path))
            .on_action(cx.listener(Self::copy_path))
            .on_action(cx.listener(Self::copy_relative_path))
            .on_action(cx.listener(Self::reveal))
            .on_action(cx.listener(Self::new_file))
            .on_action(cx.listener(Self::new_folder))
            .on_action(cx.listener(Self::rename))
            .on_action(cx.listener(Self::duplicate))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::move_to_trash))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::collapse_selected))
            .on_action(cx.listener(Self::expand_selected))
            .relative()
            .flex_1()
            .min_h_0()
            // A right click off the rows targets the root; a row's own
            // handler (bubble phase, later) retargets it.
            .capture_any_mouse_down(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right {
                    this.selected = None;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus.focus(window, cx)),
            )
            .child(body)
            .vertical_scrollbar(&self.scroll)
            .context_menu(move |menu, window, cx| match tree.upgrade() {
                Some(tree) => tree.read(cx).context_menu(menu, window.rem_size()),
                None => menu,
            })
            .into_any_element()
    }

    /// The right-click menu for the target entry, Tern's layout: open,
    /// paths, reveal, edit, trash. Items dispatch the tree's actions, so the
    /// shortcuts show beside them.
    fn context_menu(&self, menu: PopupMenu, rem_size: Pixels) -> PopupMenu {
        let (_, is_dir) = self.target();
        let entry = self.target_entry().is_some();
        let item = |label: &'static str, icon: IconName, action: Box<dyn Action>| {
            PopupMenuItem::new(label)
                .icon(Icon::new(icon))
                .action(action)
        };
        let menu = menu
            .action_context(self.focus.clone())
            .min_w(rems(250. / 16.).to_pixels(rem_size));
        let menu = if is_dir {
            menu.item(item("New File…", IconName::FilePlus, Box::new(NewFile)))
                .item(item(
                    "New Folder…",
                    IconName::FolderPlus,
                    Box::new(NewFolder),
                ))
        } else {
            menu.item(item("Open", IconName::File, Box::new(Open)))
                .item(item(
                    "Open to the Side",
                    IconName::Columns2,
                    Box::new(OpenToSide),
                ))
                .item(item(
                    "Open in New Tab",
                    IconName::Plus,
                    Box::new(OpenInNewTab),
                ))
                .item(item(
                    "Open with Default App",
                    IconName::ArrowUpRight,
                    Box::new(OpenWithDefaultApp),
                ))
        };
        let menu = menu
            .separator()
            .item(item(
                "Insert Path",
                IconName::Terminal,
                Box::new(InsertPath),
            ))
            .item(item("Copy Path", IconName::Copy, Box::new(CopyPath)))
            .item(item(
                "Copy Relative Path",
                IconName::Copy,
                Box::new(CopyRelativePath),
            ))
            .separator()
            .item(item(
                REVEAL,
                IconName::FolderInput,
                Box::new(RevealInFinder),
            ))
            .separator();
        let menu = if entry {
            menu.item(item("Rename…", IconName::Pencil, Box::new(Rename)))
                .item(item("Duplicate", IconName::CopyPlus, Box::new(Duplicate)))
                .item(item("Cut", IconName::Scissors, Box::new(CutEntry)))
                .item(item("Copy", IconName::Copy, Box::new(CopyEntry)))
        } else {
            menu
        };
        let menu = menu.item(
            item("Paste", IconName::ClipboardPaste, Box::new(PasteEntry))
                .disabled(self.clipboard.is_none()),
        );
        if entry {
            menu.separator().item(item(
                "Move to Trash",
                IconName::Trash,
                Box::new(MoveToTrash),
            ))
        } else {
            menu
        }
    }

    /// The ⋯ menu: view options for the whole tree.
    fn more_menu(&self, menu: PopupMenu, tree: WeakEntity<Self>, rem_size: Pixels) -> PopupMenu {
        let act = |f: fn(&mut Self, &mut Context<Self>)| {
            let tree = tree.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = tree.update(cx, f);
            }
        };
        let root = self.root.clone();
        let menu = menu
            .min_w(rems(220. / 16.).to_pixels(rem_size))
            .item(
                PopupMenuItem::new("Refresh")
                    .icon(Icon::new(IconName::RefreshCw))
                    .on_click(act(Self::refresh)),
            )
            .item(
                PopupMenuItem::new("Collapse All")
                    .icon(Icon::new(IconName::ChevronsDownUp))
                    .on_click(act(Self::collapse_all)),
            )
            .item(
                PopupMenuItem::new(REVEAL)
                    .icon(Icon::new(IconName::FolderInput))
                    .on_click({
                        let root = root.clone();
                        move |_, _, cx| cx.reveal_path(&root)
                    }),
            )
            .separator();
        let menu = self.ides.iter().fold(menu, |menu, ide| {
            let (ide, root) = (ide.clone(), root.clone());
            menu.item(
                PopupMenuItem::new(format!("Open in {}", ide.name))
                    .icon(Icon::new(IconName::CodeXml))
                    .on_click(move |_, window, cx| {
                        if let Err(err) = ides::open(&ide, &root) {
                            window.push_notification(
                                Notification::error(format!("Couldn't open {}: {err}", ide.name)),
                                cx,
                            );
                        }
                    }),
            )
        });
        let menu = if self.ides.is_empty() {
            menu
        } else {
            menu.separator()
        };
        menu.item(
            PopupMenuItem::new("Show Ignored Files")
                .checked(self.show_ignored)
                .on_click(act(Self::toggle_ignored)),
        )
    }

    fn header_button(id: &'static str, icon: IconName, tooltip: &'static str) -> Button {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(Icon::new(icon))
            .tooltip(tooltip)
    }

    /// One of the Files / Changes / Branches icon tabs.
    fn tab_button(
        &self,
        tab: Tab,
        icon: IconName,
        label: &'static str,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.tab == tab;
        h_flex()
            .id(label)
            .h(rems(26. / 16.))
            .min_w(rems(32. / 16.))
            .px_2()
            .gap_1()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .when(active, |d| d.bg(ui::surface_hover()))
            .when(!active, |d| {
                d.hover(|s| s.bg(ui::surface_hover().opacity(0.6)))
            })
            .child(
                Icon::new(icon)
                    .small()
                    .text_color(if active { ui::fg() } else { ui::faint() }),
            )
            // The open tab says what it is; the others keep to their icons.
            .when(active, |d| {
                d.child(
                    div()
                        .text_size(rems(12.5 / 16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui::fg())
                        .child(label),
                )
            })
            .when(count > 0, |d| {
                d.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(10.5))
                        .text_color(ui::warning())
                        .child(count.to_string()),
                )
            })
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.set_tab(tab, window, cx)))
    }
}

impl Render for FileTreeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self.tab == Tab::Files;
        let tree = cx.entity().downgrade();
        let header = h_flex()
            .h(rems(40. / 16.))
            .pl_3()
            .pr_1p5()
            .gap_0p5()
            .flex_none()
            .child(
                h_flex()
                    .id("file-tree-change-folder")
                    .flex_1()
                    .min_w_0()
                    .h(rems(26. / 16.))
                    .ml_neg_1()
                    .px_1()
                    .gap_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(ui::surface_hover().opacity(0.6)))
                    .child(Icon::new(IconName::Folder).small().text_color(ui::muted()))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(rems(13.5 / 16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui::fg())
                            .child(self.name.clone()),
                    )
                    .tooltip(|window, cx| Tooltip::new("Change folder").build(window, cx))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FileTreeEvent::ChooseFolder))),
            )
            .when(files, |d| {
                d.child(
                    Self::header_button("file-tree-new-file", IconName::FilePlus, "New File")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.start_new(false, window, cx)),
                        ),
                )
                .child(
                    Self::header_button("file-tree-new-folder", IconName::FolderPlus, "New Folder")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.start_new(true, window, cx)),
                        ),
                )
                .child(
                    Self::header_button("file-tree-more", IconName::Ellipsis, "More")
                        .dropdown_menu(move |menu, window, cx| match tree.upgrade() {
                            Some(view) => {
                                view.read(cx)
                                    .more_menu(menu, tree.clone(), window.rem_size())
                            }
                            None => menu,
                        }),
                )
            });
        // Only the visible tab polls git, so the badge reads whichever one is
        // live: the Changes view on its tab, the tree's own snapshot elsewhere.
        let change_count = match (&self.tab, &self.git) {
            (Tab::Changes, _) | (_, None) => self.changes.read(cx).change_count(),
            (_, Some(state)) => state.as_ref().map_or(0, |s| s.snapshot.files.len()),
        };
        let tabs = h_flex()
            .flex_none()
            .px_2()
            .pb_1p5()
            .gap_1()
            .when(self.original_tabs, |d| d.child(self.tab_button(Tab::Files, IconName::Files, "Files", 0, cx)))
            .child(self.tab_button(Tab::Changes, IconName::Diff, "Changes", change_count, cx))
            .child(self.tab_button(Tab::Branches, IconName::GitBranch, "Branches", 0, cx))
            .when(!self.original_tabs, |d| d.child(
                Self::header_button("sidebar-git-history", IconName::GitGraph, "Open Git history")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let repo = this
                            .git
                            .as_ref()
                            .and_then(|git| git.as_ref())
                            .map_or_else(|| this.root.clone(), |state| state.toplevel.clone());
                        cx.emit(FileTreeEvent::OpenHistory(repo));
                    })),
            ));
        // Finding a file, under the tabs while the tree shows.
        let search = (self.tab == Tab::Files).then(|| {
            h_flex().flex_none().px_2().pb_1p5().child(
                ui::field()
                    .id("file-tree-search")
                    .flex_1()
                    .min_w_0()
                    .h(rems(30. / 16.))
                    .cursor_pointer()
                    .hover(|s| s.border_color(ui::petal().opacity(0.45)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FileTreeEvent::SearchFiles)))
                    .child(Icon::new(IconName::Search).small().text_color(ui::faint()))
                    .child(
                        div()
                            .text_size(rems(12.5 / 16.))
                            .text_color(ui::faint())
                            .child("Search files"),
                    ),
            )
        });
        // The repository at a glance, where the eye ends: the branch (a
        // click shows the branches) and what's changed (a click, the
        // changes).
        let footer =
            self.git.as_ref().and_then(|git| git.as_ref()).map(|state| {
                let branch = state
                    .snapshot
                    .branch
                    .clone()
                    .unwrap_or_else(|| "detached".into());
                let changed = state.snapshot.files.len();
                h_flex()
                    .flex_none()
                    .h(rems(34. / 16.))
                    .mx_2()
                    .mb_2()
                    .px_2()
                    .gap_2()
                    .rounded_lg()
                    .bg(ui::surface())
                    .child(
                        h_flex()
                            .id("file-tree-branch")
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .cursor_pointer()
                            .text_color(ui::muted())
                            .hover(|s| s.text_color(ui::fg()))
                            .tooltip(|window, cx| Tooltip::new("Branches").build(window, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_tab(Tab::Branches, window, cx)
                            }))
                            .child(
                                Icon::new(IconName::GitBranch)
                                    .xsmall()
                                    .text_color(ui::champagne()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(ui::mono())
                                    .text_size(ui::code_rems(12.))
                                    .child(branch),
                            ),
                    )
                    .child(
                        h_flex()
                            .id("file-tree-changed")
                            .flex_none()
                            .gap_1()
                            .cursor_pointer()
                            .font_family(ui::mono())
                            .text_size(ui::code_rems(11.5))
                            .text_color(if changed > 0 {
                                ui::warning()
                            } else {
                                ui::faint()
                            })
                            .hover(|s| s.text_color(ui::fg()))
                            .tooltip(|window, cx| Tooltip::new("Changes").build(window, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.set_tab(Tab::Changes, window, cx)
                            }))
                            .child(if changed > 0 {
                                format!("±{changed}")
                            } else {
                                "clean".to_string()
                            }),
                    )
            });
        // The workspace's right sidebar slot paints the rounded glass; a fill here
        // would show square corners past it (GPUI clips to rectangles).
        v_flex()
            .size_full()
            .min_h_0()
            .child(header)
            .when(self.original_tabs || !files, |d| d.child(tabs))
            .children(search)
            .child(match self.tab {
                Tab::Files => self.render_files(cx),
                Tab::Changes => div()
                    .flex_1()
                    .min_h_0()
                    .child(self.changes.clone())
                    .into_any_element(),
                Tab::Branches => div()
                    .flex_1()
                    .min_h_0()
                    .child(self.refs.clone())
                    .into_any_element(),
            })
            .children(footer)
    }
}

/// The frame every tree row shares: height, indent, hover and selection.
fn row_shell(id: SharedString, depth: usize, highlighted: bool) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(rems(ROW_HEIGHT / 16.))
        .w_full()
        .pl(rems((6. + depth as f32 * INDENT) / 16.))
        .pr_2()
        .gap_1p5()
        .rounded_md()
        .when(highlighted, |d| d.bg(ui::raised()))
        .when(!highlighted, |d| d.hover(|s| s.bg(ui::surface_hover())))
}

fn empty_state(icon: IconName, text: &'static str) -> Div {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .child(Icon::new(icon).text_color(ui::faint()))
        .child(
            div()
                .text_size(rems(12. / 16.))
                .text_color(ui::muted())
                .child(text),
        )
}

/// Untracked files read as `U`, like editors show them.
fn status_letter(status: char) -> char {
    if status == '?' { 'U' } else { status }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn parent_of(path: &Path) -> PathBuf {
    path.parent().unwrap_or(path).to_path_buf()
}

fn paths(state: &GitState) -> Vec<&str> {
    state
        .snapshot
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect()
}

fn show_toplevel(root: &Path) -> Option<PathBuf> {
    crate::features::git::toplevel(root)
}

/// What a click on a tree row does, by click count.
enum ClickOutcome {
    /// Folders toggle open and closed.
    Toggle,
    /// A file opens in the tab's preview pane.
    Open,
    /// The second press of a double click opens nothing more.
    Nothing,
}

fn click_outcome(is_dir: bool, clicks: usize) -> ClickOutcome {
    if is_dir {
        ClickOutcome::Toggle
    } else if clicks == 1 {
        ClickOutcome::Open
    } else {
        ClickOutcome::Nothing
    }
}

fn read_show_ignored(path: &Path) -> bool {
    themes::read_key(path, "showIgnoredFiles")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn write_show_ignored(path: &Path, show: bool) -> std::io::Result<()> {
    themes::write_key(path, "showIgnoredFiles", show.into())
}

#[cfg(test)]
mod show_ignored_tests {
    use super::{read_show_ignored, write_show_ignored};
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "gentle-show-ignored-{}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[gpui::test]
    fn open_trees_follow_ignored_choice(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext as _;

        let dir = Scratch::new();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(super::ShowIgnored {
                value: false,
                path: Some(dir.0.join("settings.json")),
            });
        });
        let root = dir.0.clone();
        let (first, cx) =
            cx.add_window_view(|window, cx| super::FileTreeView::new(root.clone(), window, cx));
        let second =
            cx.update(|window, cx| cx.new(|cx| super::FileTreeView::new(root.clone(), window, cx)));
        cx.update(|_, cx| first.update(cx, |tree, cx| tree.toggle_ignored(cx)));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(first.read(cx).show_ignored);
            assert!(second.read(cx).show_ignored, "other open tree follows");
        });
        let settings = dir.0.join("settings.json");
        assert!(read_show_ignored(&settings));
        let late =
            cx.update(|window, cx| cx.new(|cx| super::FileTreeView::new(root.clone(), window, cx)));
        cx.update(|_, cx| assert!(late.read(cx).show_ignored));
        cx.update(|_, cx| second.update(cx, |tree, cx| tree.toggle_ignored(cx)));
        cx.run_until_parked();
        cx.update(|_, cx| {
            assert!(!first.read(cx).show_ignored);
            assert!(!second.read(cx).show_ignored);
            assert!(!late.read(cx).show_ignored);
        });
        assert!(!read_show_ignored(&settings));
    }

    #[test]
    fn show_ignored_defaults_to_false_for_missing_or_invalid_settings() {
        let dir = Scratch::new();
        let path = dir.0.join("settings.json");
        assert!(!read_show_ignored(&path));
        std::fs::write(&path, r#"{"showIgnoredFiles":"true"}"#).unwrap();
        assert!(!read_show_ignored(&path));
    }

    #[test]
    fn show_ignored_persists_both_values_and_preserves_other_settings() {
        let dir = Scratch::new();
        let path = dir.0.join("settings.json");
        std::fs::write(&path, r#"{"theme":"Studio","other":{"value":3}}"#).unwrap();
        write_show_ignored(&path, true).unwrap();
        assert!(read_show_ignored(&path));
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["theme"], "Studio");
        assert_eq!(settings["other"]["value"], 3);
        write_show_ignored(&path, false).unwrap();
        assert!(!read_show_ignored(&path));
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["showIgnoredFiles"], false);
        assert_eq!(settings["theme"], "Studio");
        assert_eq!(settings["other"]["value"], 3);
    }
}

#[cfg(test)]
mod click_tests {
    use super::{ClickOutcome, click_outcome};

    /// A single click opens a file; a folder toggles instead.
    #[test]
    fn a_single_click_opens_a_file_and_a_folder_toggles() {
        assert!(matches!(click_outcome(false, 1), ClickOutcome::Open));
        assert!(matches!(click_outcome(true, 1), ClickOutcome::Toggle));
    }

    /// The second press of a double click opens nothing more.
    #[test]
    fn the_second_press_of_a_double_click_opens_nothing_more() {
        assert!(matches!(click_outcome(false, 2), ClickOutcome::Nothing));
        assert!(matches!(click_outcome(true, 2), ClickOutcome::Toggle));
    }
}
