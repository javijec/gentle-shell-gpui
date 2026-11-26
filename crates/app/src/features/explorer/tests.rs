//! The explorer in a headless window over a temporary folder: what it lists,
//! when, and which rows it draws.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

// Not a glob: GPUI exports a `test` attribute that would shadow `#[test]`.
use gpui_kit::{Entity, TestAppContext, VisualTestContext, point, px, size};

use super::{ExplorerEvent, ExplorerView};

/// A temporary folder, removed when dropped.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(name: &str) -> Temp {
    let dir = std::env::temp_dir().join(format!("gs-explorer-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Temp(dir)
}

/// A file, a folder, a repository and (on Unix) links to both folders.
fn project(root: &Path) {
    std::fs::write(root.join("notes.txt"), "").unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "").unwrap();
    std::fs::create_dir_all(root.join("repo/.git")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("src"), root.join("linked")).unwrap();
        std::os::unix::fs::symlink(root.join("repo"), root.join("linked-repo")).unwrap();
    }
}

fn init(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
}

fn open(cx: &mut TestAppContext, dir: PathBuf) -> (Entity<ExplorerView>, &mut VisualTestContext) {
    init(cx);
    cx.add_window_view(|window, cx| ExplorerView::new(dir, window, cx))
}

fn load(view: &Entity<ExplorerView>, dir: PathBuf, cx: &mut VisualTestContext) {
    view.update_in(cx, |this, window, cx| this.load(dir, None, window, cx));
}

/// The rows as shown: folders with a trailing slash.
fn shown(view: &Entity<ExplorerView>, cx: &VisualTestContext) -> Vec<String> {
    view.read_with(cx, |this, _| {
        this.shown
            .iter()
            .map(|e| format!("{}{}", e.name, if e.is_dir { "/" } else { "" }))
            .collect()
    })
}

fn dir(view: &Entity<ExplorerView>, cx: &VisualTestContext) -> PathBuf {
    view.read_with(cx, |this, _| this.dir.clone())
}

fn row(i: usize) -> &'static str {
    Box::leak(format!("explorer-row-{i}").into_boxed_str())
}

#[gpui_kit::test]
fn lists_folders_first_marking_repositories_and_following_links(cx: &mut TestAppContext) {
    let root = temp("list");
    project(&root.0);
    let (view, cx) = open(cx, root.0.clone());

    assert_eq!(dir(&view, cx), root.0);
    let repos = view.read_with(cx, |this, _| this.repos.clone());
    if cfg!(unix) {
        assert_eq!(
            shown(&view, cx),
            ["linked/", "linked-repo/", "repo/", "src/", "notes.txt"]
        );
        assert_eq!(repos, HashSet::from(["repo".into(), "linked-repo".into()]));
    } else {
        assert_eq!(shown(&view, cx), ["repo/", "src/", "notes.txt"]);
        assert_eq!(repos, HashSet::from(["repo".into()]));
    }
}

#[gpui_kit::test]
fn the_folder_is_read_off_the_ui_thread_keeping_the_previous_one_shown(cx: &mut TestAppContext) {
    let root = temp("off-thread");
    project(&root.0);
    let (view, cx) = open(cx, root.0.clone());
    let before = shown(&view, cx);

    load(&view, root.0.join("src"), cx);
    // Navigating returns before the folder is read: the previous listing
    // stays until the new one arrives.
    assert_eq!(dir(&view, cx), root.0);
    assert_eq!(shown(&view, cx), before);

    cx.run_until_parked();
    assert_eq!(dir(&view, cx), root.0.join("src"));
    assert_eq!(shown(&view, cx), ["main.rs"]);
}

#[gpui_kit::test]
fn nothing_opens_before_the_first_folder_is_read(cx: &mut TestAppContext) {
    let root = temp("first");
    init(cx);
    let window = cx.add_window(|window, cx| ExplorerView::new(root.0.clone(), window, cx));
    let view = window.root(cx).unwrap();
    let opened = Rc::new(RefCell::new(Vec::new()));
    let seen = opened.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &ExplorerEvent, _| {
            if let ExplorerEvent::OpenAgent(folder) | ExplorerEvent::OpenTerminal(folder) = event {
                seen.borrow_mut().push(folder.clone());
            }
        })
    });

    view.update(cx, |this, cx| {
        this.open_agent(cx);
        this.open_terminal(cx);
    });
    assert!(opened.borrow().is_empty());

    cx.run_until_parked();
    view.update(cx, |this, cx| this.open_agent(cx));
    assert_eq!(*opened.borrow(), std::slice::from_ref(&root.0));
}

#[gpui_kit::test]
fn the_latest_navigation_wins(cx: &mut TestAppContext) {
    let root = temp("latest");
    project(&root.0);
    let (view, cx) = open(cx, root.0.clone());

    load(&view, root.0.join("src"), cx);
    load(&view, root.0.join("repo"), cx);
    cx.run_until_parked();
    assert_eq!(dir(&view, cx), root.0.join("repo"));
    assert!(shown(&view, cx).is_empty());

    load(&view, root.0.join("repo"), cx);
    load(&view, root.0.join("src"), cx);
    cx.run_until_parked();
    assert_eq!(dir(&view, cx), root.0.join("src"));
    assert_eq!(shown(&view, cx), ["main.rs"]);
}

#[gpui_kit::test]
fn going_up_selects_the_folder_left_and_clears_the_filter(cx: &mut TestAppContext) {
    let root = temp("up");
    project(&root.0);
    let (view, cx) = open(cx, root.0.join("src"));
    view.update_in(cx, |this, window, cx| {
        this.filter
            .update(cx, |state, cx| state.set_value("ma", window, cx))
    });
    cx.run_until_parked();

    view.update_in(cx, |this, window, cx| this.go_up(window, cx));
    cx.run_until_parked();
    assert_eq!(dir(&view, cx), root.0);
    view.read_with(cx, |this, cx| {
        assert_eq!(this.filter.read(cx).value(), "");
        assert_eq!(this.selected_entry().map(|e| e.name.as_str()), Some("src"));
    });
}

#[gpui_kit::test]
fn an_unreadable_folder_says_why(cx: &mut TestAppContext) {
    let root = temp("missing");
    let missing = root.0.join("gone");
    let why = std::fs::read_dir(&missing).unwrap_err().to_string();
    let (view, cx) = open(cx, missing.clone());

    assert_eq!(dir(&view, cx), missing);
    assert!(shown(&view, cx).is_empty());
    assert_eq!(view.read_with(cx, |this, _| this.error.clone()), Some(why));
}

#[gpui_kit::test]
fn a_long_folder_draws_the_rows_in_view_and_follows_the_selection(cx: &mut TestAppContext) {
    let root = temp("long");
    for i in 0..300 {
        std::fs::write(root.0.join(format!("file-{i:03}")), "").unwrap();
    }
    let (view, cx) = open(cx, root.0.clone());

    // Rows keep their place and size (as drawn before the list was virtual).
    assert_eq!(
        cx.debug_bounds(row(0)),
        Some(gpui_kit::Bounds::new(
            point(px(629.), px(361.)),
            size(px(662.), px(32.))
        ))
    );
    assert!(cx.debug_bounds(row(299)).is_none());

    // ↑ from the first row wraps to the last, scrolled into view.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |this, _| this.selected), 299);
    assert!(cx.debug_bounds(row(299)).is_some());
    assert!(cx.debug_bounds(row(0)).is_none());
}
