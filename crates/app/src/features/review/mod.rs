//! The review screen, after Tern: a modal over the window with the selected
//! file's diff on the left (Split or Unified) and the repository's changes on
//! the right (the same [`ChangesView`] as the Files sidebar: Unstaged and
//! Staged, stage, stash, commit). The composer's `±` button and
//! `/gentle:changes` open it; Esc or its ✕ closes it.

use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::DiffMode;
use gpui_kit::component::{Icon, Sizable as _, v_flex};
use gpui_kit::*;

use crate::features::files::{ChangesEvent, ChangesView, FileView};
use crate::shared::pane::CloseViewer;
use crate::shared::ui;

/// What the workspace does for the review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewEvent {
    /// The header's ✕.
    Close,
    /// "Open file" from a change's menu (absolute path).
    OpenFile(PathBuf),
}

pub struct ReviewView {
    /// The repository's top level (or the folder, outside one).
    root: PathBuf,
    changes: Entity<ChangesView>,
    diff: Option<Entity<FileView>>,
    /// The file `diff` shows; cleared to reload it when the changes move.
    shown: Option<PathBuf>,
    focus: FocusHandle,
    _changes: Subscription,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl Focusable for ReviewView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ReviewView {
    /// The review of the repository `cwd` is in, showing the first changed
    /// file's diff once its status is read.
    pub fn new(cwd: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let root = crate::features::git::toplevel(&cwd).unwrap_or(cwd);
        let changes = cx.new(|cx| ChangesView::new(root.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &changes,
            window,
            |this, changes, event: &ChangesEvent, window, cx| match event {
                ChangesEvent::OpenDiff(path) => this.show_diff(path.clone(), window, cx),
                ChangesEvent::OpenFile(path) => cx.emit(ReviewEvent::OpenFile(path.clone())),
                // Files came or went (a commit, a discard, an edit): show
                // the selected file's diff afresh, else the first one's.
                ChangesEvent::Changed => {
                    this.shown = None;
                    if changes.read(cx).change_count() == 0 {
                        this.diff = None;
                    } else {
                        changes.update(cx, |changes, cx| changes.open_diff(cx));
                    }
                    // The header counts the files.
                    cx.notify();
                }
            },
        );
        changes.update(cx, |changes, cx| changes.open_diff(cx));
        Self {
            root,
            changes,
            diff: None,
            shown: None,
            focus: cx.focus_handle(),
            _changes: subscription,
        }
    }

    /// Swap in `path`'s diff, in the mode the last one had (Unified at
    /// first: the review's half of the window is too narrow for two sides
    /// of real code; Split wraps each side to fit).
    fn show_diff(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown.as_ref() == Some(&path) {
            return;
        }
        let mode = self
            .diff
            .as_ref()
            .map_or(DiffMode::Unified, |diff| diff.read(cx).diff_mode());
        let root = self.root.clone();
        let diff = cx.new(|cx| FileView::diff(root, path.clone(), window, cx));
        diff.update(cx, |diff, cx| diff.set_diff_mode(mode, cx));
        self.diff = Some(diff);
        self.shown = Some(path);
        cx.notify();
    }

    /// The diff header's ✕ closes the diff, leaving the list.
    fn on_close_viewer(&mut self, action: &CloseViewer, _: &mut Window, cx: &mut Context<Self>) {
        if self
            .diff
            .as_ref()
            .is_none_or(|diff| diff.entity_id() != action.pane)
        {
            cx.propagate();
            return;
        }
        self.diff = None;
        self.shown = None;
        cx.notify();
    }

    fn render_empty(&self, cx: &App) -> Div {
        let clean = self.changes.read(cx).change_count() == 0;
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Icon::new(IconName::Diff).text_color(ui::faint()))
            .child(
                div()
                    .text_size(rems(13. / 16.))
                    .text_color(ui::muted())
                    .child(if clean {
                        "Working tree clean"
                    } else {
                        "Select a file"
                    }),
            )
    }
}

impl Render for ReviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::shared::diagnostics::render("ReviewView");
        let diff = match &self.diff {
            Some(diff) => diff.clone().into_any_element(),
            None => self.render_empty(cx).into_any_element(),
        };
        let files = self.changes.read(cx).change_count();
        let repo = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        let detail = match files {
            0 => repo,
            1 => format!("1 changed file · {repo}"),
            n => format!("{n} changed files · {repo}"),
        };
        let header = ui::screen_header()
            .child(Icon::new(IconName::Diff).small().text_color(ui::muted()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child("Review"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(ui::muted())
                    .child(detail),
            )
            .child(
                Button::new("review-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::X))
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ReviewEvent::Close))),
            );
        // No line between the columns: the list sits on its own surface,
        // kept off the panel's edges.
        let body = div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(diff),
            )
            .child(
                div()
                    .flex_none()
                    .w(rems(380. / 16.))
                    // A narrow window keeps most of its width for the diff.
                    .max_w(relative(0.45))
                    .h_full()
                    .py_2()
                    .pr_2()
                    .child(
                        div()
                            .size_full()
                            .rounded_xl()
                            .bg(ui::surface())
                            .overflow_hidden()
                            .child(self.changes.clone()),
                    ),
            );
        let (backdrop, panel) = ui::workspace_screen(None);
        // Wider than the other screens: a diff needs the room more than a
        // form does, side by side above all.
        let panel = panel.max_w(rems(1800. / 16.));
        backdrop.occlude().child(ui::pop_in(
            "review-screen",
            panel
                .track_focus(&self.focus)
                .on_action(cx.listener(Self::on_close_viewer))
                .child(header)
                .child(body),
        ))
    }
}
