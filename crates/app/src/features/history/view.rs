//! The Git pane: the header, the toolbar with the scope picker, the filter
//! and the actions, the inline prompt, the banner of a command git stopped
//! in, the graph list with its context menus, the selected commit's details,
//! the hover card of a commit, and the footer.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use gentle_core::git::log::{Commit, RefLabel, initials};
use gentle_core::git::ops::{CommitFile, PullMode, ResetMode};
use gentle_core::sessions::relative_time;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::graph::{self, RowGraph};
use super::ops::Ask;
use super::{HistoryView, Line, Outcome};
use crate::features::git::{Scope, ops::Lfs};
use crate::platform::ides;
use crate::shared::ui;

const ROW_HEIGHT: f32 = 30.;
const AUTHOR_WIDTH: f32 = 150.;
const TIME_WIDTH: f32 = 40.;
const SHA_WIDTH: f32 = 60.;
/// The share of the list's height the selected commit's panel may take;
/// its body scrolls past that.
const DETAIL_SHARE: f32 = 0.45;
/// Below these pane widths the row drops the author's name and the SHA,
/// then the author and the ref chips; the toolbar drops its labels.
const WIDE: f32 = 640.;
const MEDIUM: f32 = 460.;

/// The activity strip's day cells (px): the gap between them, their size
/// range (they grow with the pane up to the max, and a narrower pane shows
/// fewer weeks below the min), and the weekday labels' column.
const ACTIVITY_GAP: f32 = 3.;
const ACTIVITY_MIN: f32 = 7.;
const ACTIVITY_MAX: f32 = 13.;
const ACTIVITY_LABELS: f32 = 26.;

/// What a context menu row does to the view.
type MenuAction = Rc<dyn Fn(&mut HistoryView, &mut Window, &mut Context<HistoryView>)>;

/// Which columns fit the pane.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    Narrow,
    Medium,
    Wide,
}

/// At most `max` characters, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// `2d ago` → `2d`, `just now` → `now`: the row's time column.
fn short_time(now_ms: i64, secs: i64) -> String {
    let text = relative_time(now_ms, secs * 1000);
    text.strip_suffix(" ago")
        .map(str::to_string)
        .unwrap_or_else(|| {
            if text == "just now" {
                "now".into()
            } else {
                text
            }
        })
}

/// The author's picture in a circle, else their initials; a petal dot marks
/// a commit written with a co-author.
fn avatar(name: &str, picture: Option<Arc<Path>>, coauthored: bool, size: f32) -> Div {
    let circle = div()
        .size(rems(size / 16.))
        .flex_none()
        .rounded_full()
        .overflow_hidden()
        .flex()
        .items_center()
        .justify_center()
        .bg(ui::raised())
        .border_1()
        .border_color(ui::border())
        .text_size(rems(size * 0.4 / 16.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(ui::muted())
        .map(|d| match picture {
            Some(path) => d.child(img(path).size_full().rounded_full()),
            None => d.child(initials(name)),
        });
    div()
        .relative()
        .flex_none()
        .child(circle)
        .when(coauthored, |d| {
            d.child(
                div()
                    .absolute()
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .size(rems(size * 0.32 / 16.))
                    .rounded_full()
                    .bg(ui::petal())
                    .border_1()
                    .border_color(ui::screen_bg()),
            )
        })
}

/// A ref beside a commit's subject: `chip`'s look with optional icons
/// (✓ for HEAD's branch, a globe when the remote has it there too).
fn ref_chip(text: String, color: Hsla, lead: Option<IconName>, trail: Option<IconName>) -> Div {
    h_flex()
        .flex_none()
        .gap_1()
        .px_1p5()
        .py(px(1.))
        .rounded_md()
        .text_size(rems(10.5 / 16.))
        .text_color(color)
        .bg(color.opacity(0.12))
        .border_1()
        .border_color(color.opacity(0.22))
        .when_some(lead, |d, icon| d.child(Icon::new(icon).xsmall()))
        .child(text)
        .when_some(trail, |d, icon| {
            d.child(Icon::new(icon).xsmall().text_color(color.opacity(0.75)))
        })
}

/// A changed file's status letter, in its tone.
fn status_color(status: char) -> Hsla {
    match status {
        'A' => ui::success(),
        'D' => ui::danger(),
        'R' | 'C' => ui::info(),
        _ => ui::warning(),
    }
}

impl HistoryView {
    fn fit(&self, window: &Window) -> Fit {
        let rem = |n: f32| f32::from(rems(n / 16.).to_pixels(window.rem_size()));
        match self.width.get() {
            // Not measured yet: the first frame lays out wide.
            w if w == 0. || w >= rem(WIDE) => Fit::Wide,
            w if w >= rem(MEDIUM) => Fit::Medium,
            _ => Fit::Narrow,
        }
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let fit = self.fit(window);
        let history = self.read();
        let folder = self
            .toplevel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The pane header, as the file viewer's: no line under it.
        h_flex()
            .flex_none()
            .h(rems(38. / 16.))
            .px_4()
            .gap_2()
            .child(
                Icon::new(IconName::GitGraph)
                    .small()
                    .text_color(ui::muted()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(rems(13. / 16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child("Git"),
            )
            .when(fit > Fit::Narrow, |d| {
                d.when_some(history.map(|h| h.total), |d, total| {
                    d.child(ui::chip(total.to_string(), ui::muted()).flex_shrink_0())
                })
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(rems(12.5 / 16.))
                    .text_color(ui::faint())
                    .when(fit > Fit::Narrow, |d| d.child(folder)),
            )
            .when(self.loading && history.is_some(), |d| {
                d.child(Spinner::new().xsmall().color(ui::muted()))
            })
            .child(
                Button::new("git-refresh")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::RefreshCw))
                    .loading(self.loading)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            )
    }

    /// The scope picker, the filter and the actions. Only in a repository.
    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let history = self.read()?;
        let fit = self.fit(window);
        let wide = fit == Fit::Wide;
        let busy = self.busy.is_some();
        let weak = cx.entity().downgrade();
        let current = history.current.clone();
        let scope = history.scope.clone();
        let label = match &scope {
            Scope::All => "All branches".to_string(),
            _ => history
                .branch()
                .map(str::to_string)
                .unwrap_or_else(|| "Detached HEAD".into()),
        };
        let branches = history.branches.clone();
        let picker = {
            let weak = weak.clone();
            let (current, scope) = (current.clone(), scope.clone());
            Button::new("git-scope")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::GitBranch))
                .when(fit == Fit::Narrow, |b| b.tooltip(label.clone()))
                .when(fit > Fit::Narrow, |b| {
                    b.label(clip(&label, if wide { 28 } else { 16 }))
                })
                .dropdown_caret(true)
                .dropdown_menu(move |menu, window, _| {
                    let show = |label: String, target: Scope, checked: bool| {
                        let weak = weak.clone();
                        PopupMenuItem::new(label)
                            .checked(checked)
                            .on_click(move |_, _, cx| {
                                let target = target.clone();
                                let _ = weak.update(cx, |this, cx| this.show(target, cx));
                            })
                    };
                    let head = match &current {
                        Some(branch) => format!("HEAD · {branch}"),
                        None => "HEAD (detached)".into(),
                    };
                    let mut menu = menu
                        .max_h(rems(380. / 16.).to_pixels(window.rem_size()))
                        .scrollable(true)
                        .item(show(head, Scope::Head, scope == Scope::Head))
                        .item(show("All branches".into(), Scope::All, scope == Scope::All));
                    let others: Vec<&String> = branches
                        .iter()
                        .filter(|b| Some(*b) != current.as_ref())
                        .collect();
                    if !others.is_empty() {
                        menu = menu.separator().label("Show branch");
                        for branch in others {
                            menu = menu.item(show(
                                branch.clone(),
                                Scope::Branch(branch.clone()),
                                scope == Scope::Branch(branch.clone()),
                            ));
                        }
                    }
                    menu
                })
        };
        let filter = h_flex()
            .flex_1()
            .min_w(rems(90. / 16.))
            .max_w(rems(240. / 16.))
            .h(rems(26. / 16.))
            .px_2()
            .gap_1p5()
            .rounded_md()
            .bg(ui::well())
            .child(Icon::new(IconName::Search).xsmall().text_color(ui::faint()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.filter).appearance(false).xsmall()),
            );

        let (ahead, behind) = history
            .upstream
            .as_ref()
            .map_or((0, 0), |(_, ahead, behind)| (*ahead, *behind));
        let has_remote = !history.remotes.is_empty();
        let tool = |id: &'static str, icon: IconName, label: String, tip: String| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(Icon::new(icon))
                .disabled(busy)
                .when(wide && !label.is_empty(), |b| b.label(label))
                .tooltip(tip)
        };
        let count = |n: u32, arrow: &str| {
            if n > 0 {
                format!(" {arrow}{n}")
            } else {
                String::new()
            }
        };
        let fetch = tool(
            "git-fetch",
            IconName::CloudDownload,
            "Fetch".into(),
            "Fetch all remotes".into(),
        )
        .disabled(busy || !has_remote)
        .on_click(cx.listener(|this, _, _, cx| this.fetch(cx)));
        let pull = tool(
            "git-pull",
            IconName::ArrowDownToLine,
            format!("Pull{}", count(behind, "↓")),
            format!("Pull · {}", self.pull_mode.label()),
        )
        .disabled(busy || history.upstream.is_none())
        .on_click(cx.listener(|this, _, _, cx| this.pull(cx)));
        let pull_modes = {
            let weak = weak.clone();
            let chosen = self.pull_mode;
            Button::new("git-pull-mode")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::ChevronDown))
                .tooltip("Pull options")
                .disabled(busy)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.label("Pull integration");
                    for mode in PullMode::ALL {
                        let weak = weak.clone();
                        menu = menu.item(
                            PopupMenuItem::new(mode.label())
                                .checked(mode == chosen)
                                .on_click(move |_, _, cx| {
                                    let _ =
                                        weak.update(cx, |this, cx| this.set_pull_mode(mode, cx));
                                }),
                        );
                    }
                    let weak = weak.clone();
                    menu.separator().item(
                        PopupMenuItem::new("Fetch all remotes")
                            .icon(Icon::new(IconName::CloudDownload))
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| this.fetch(cx));
                            }),
                    )
                })
        };
        let push = tool(
            "git-push",
            IconName::ArrowUpFromLine,
            format!("Push{}", count(ahead, "↑")),
            match &history.upstream {
                Some((upstream, ..)) => format!("Push to {upstream}"),
                None => "Push (publishes the branch)".into(),
            },
        )
        .disabled(busy || !has_remote)
        .on_click(cx.listener(|this, _, _, cx| this.push(cx)));
        let head_label = current.clone().unwrap_or_else(|| "HEAD".into());
        let new_branch = {
            let label = head_label.clone();
            tool(
                "git-new-branch",
                IconName::GitBranchPlus,
                String::new(),
                "New branch at HEAD".into(),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                let ask = Ask::Branch {
                    at: "HEAD".into(),
                    label: label.clone(),
                };
                this.ask(ask, String::new(), window, cx)
            }))
        };
        let stash = tool(
            "git-stash",
            IconName::Archive,
            String::new(),
            "Stash changes".into(),
        )
        .disabled(busy || !history.dirty)
        .on_click(
            cx.listener(|this, _, window, cx| this.ask(Ask::Stash, String::new(), window, cx)),
        );
        let jump = tool(
            "git-jump-head",
            IconName::LocateFixed,
            String::new(),
            "Jump to HEAD".into(),
        )
        .disabled(false)
        .on_click(cx.listener(|this, _, _, cx| this.jump_to_head(cx)));
        let more = {
            let weak = weak.clone();
            let lfs = self.lfs;
            let ides = self.ides.clone();
            let root = self.toplevel.clone();
            Button::new("git-more")
                .ghost()
                .xsmall()
                .icon(Icon::new(IconName::Ellipsis))
                .tooltip("More actions")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                    let item = |label: &'static str,
                                icon: IconName,
                                enabled: bool,
                                run: fn(
                        &mut HistoryView,
                        &mut Window,
                        &mut Context<HistoryView>,
                    )| {
                        let weak = weak.clone();
                        PopupMenuItem::new(label)
                            .icon(Icon::new(icon))
                            .disabled(!enabled)
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| run(this, window, cx));
                            })
                    };
                    let mut menu = menu
                        .item(item(
                            "Fetch all remotes",
                            IconName::CloudDownload,
                            !busy && has_remote,
                            |this, _, cx| this.fetch(cx),
                        ))
                        .item(item(
                            "Add remote…",
                            IconName::Globe,
                            !busy,
                            |this, window, cx| this.ask(Ask::Remote, String::new(), window, cx),
                        ));
                    if lfs {
                        menu = menu.separator();
                        for op in Lfs::ALL {
                            let weak = weak.clone();
                            menu = menu.item(
                                PopupMenuItem::new(op.label())
                                    .icon(Icon::new(IconName::Database))
                                    .disabled(busy || (op == Lfs::Push && !has_remote))
                                    .on_click(move |_, _, cx| {
                                        let _ = weak.update(cx, |this, cx| this.lfs(op, cx));
                                    }),
                            );
                        }
                    }
                    menu = menu.separator().item(item(
                        "Open terminal here",
                        IconName::SquareTerminal,
                        true,
                        |this, window, cx| this.open_terminal(window, cx),
                    ));
                    for ide in ides.iter() {
                        let (ide, root) = (ide.clone(), root.clone());
                        menu = menu.item(
                            PopupMenuItem::new(format!("Open in {}", ide.name))
                                .icon(Icon::new(IconName::CodeXml))
                                .on_click(move |_, window, cx| {
                                    if let Err(err) = ides::open(&ide, &root) {
                                        window.push_notification(
                                            Notification::error(format!(
                                                "Couldn't open {}: {err}",
                                                ide.name
                                            )),
                                            cx,
                                        );
                                    }
                                }),
                        );
                    }
                    menu
                })
        };

        // The sidebar is narrower than a typical tiled pane. Keep the scope
        // and filter on their own row, and wrap actions rather than clip them.
        let narrow = fit == Fit::Narrow;
        let actions = h_flex()
            .gap_1()
            .when(narrow, |d| d.w_full().flex_wrap())
            .child(fetch)
            .child(h_flex().child(pull).child(pull_modes))
            .child(push)
            .when(!narrow, |d| d.child(div().w(rems(6. / 16.))))
            .child(new_branch)
            .child(stash)
            .child(jump)
            .child(more);
        Some(
            h_flex()
                .flex_none()
                .px_3()
                .gap_1()
                .when(narrow, |d| d.flex_col().items_start().pb_1())
                .when(!narrow, |d| d.h(rems(34. / 16.)))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .when(narrow, |d| d.w_full())
                        .child(picker)
                        .child(filter),
                )
                .child(actions),
        )
    }

    /// The inline prompt for a name or a message, under the toolbar.
    fn render_prompt(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let prompt = self.prompt.as_ref()?;
        let narrow = self.fit(window) == Fit::Narrow;
        let field = |input| {
            ui::field()
                .flex_1()
                .min_w(rems(120. / 16.))
                .when(narrow, |d| d.w_full())
                .h(rems(28. / 16.))
                .child(
                    div()
                        .flex_1()
                        .child(Input::new(input).appearance(false).small()),
                )
        };
        Some(
            h_flex()
                .flex_none()
                .mx_3()
                .mb_2()
                .px_3()
                .py_2()
                .gap_2()
                .rounded_lg()
                .bg(ui::surface())
                .when(narrow, |d| d.flex_col().items_start())
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_prompt(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(prompt.ask.title()),
                )
                .child(field(&prompt.first))
                .when_some(prompt.second.as_ref(), |d, second| d.child(field(second)))
                .child(
                    Button::new("git-prompt-ok")
                        .outline()
                        .xsmall()
                        .label(prompt.ask.confirm())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_prompt(window, cx)),
                        ),
                )
                .child(
                    Button::new("git-prompt-cancel")
                        .ghost()
                        .xsmall()
                        .label("Cancel")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel_prompt(window, cx)),
                        ),
                ),
        )
    }

    /// A merge, rebase, cherry-pick or revert git stopped in: what to do next.
    fn render_banner(&self, cx: &mut Context<Self>) -> Option<Div> {
        let op = self.read()?.in_progress?;
        let busy = self.busy.is_some();
        Some(
            ui::card()
                .flex_none()
                .mx_3()
                .mb_2()
                .px_3()
                .py_2()
                .border_color(ui::warning().opacity(0.35))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .xsmall()
                                .text_color(ui::warning()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(ui::fg())
                                .child(format!(
                                    "{} in progress. Resolve the conflicts and stage them in \
                                     Changes, then continue.",
                                    op.label()
                                )),
                        )
                        .child(
                            Button::new("git-continue")
                                .outline()
                                .xsmall()
                                .label("Continue")
                                .disabled(busy)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.finish(op, false, cx)),
                                ),
                        )
                        .child(
                            Button::new("git-abort")
                                .ghost()
                                .xsmall()
                                .label("Abort…")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_abort(op, window, cx)
                                })),
                        ),
                ),
        )
    }

    fn graph_cell(&self, graph: RowGraph) -> Div {
        div()
            .relative()
            .flex_none()
            .w(px(graph::width(self.graph.width.max(1))))
            .h_full()
            .child(graph.element())
    }

    fn render_line(
        &self,
        ix: usize,
        line: Line,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .px_2()
            .child(self.render_row(ix, line, window, cx))
            .into_any_element()
    }

    fn render_row(
        &self,
        ix: usize,
        line: Line,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = h_flex()
            .id(("history-row", ix))
            .w_full()
            .h(rems(ROW_HEIGHT / 16.))
            .pr_3()
            .gap_2()
            .rounded_lg();
        match line {
            Line::Uncommitted => {
                let weak = cx.entity().downgrade();
                row.child(self.graph_cell(RowGraph::uncommitted()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(ui::faint())
                            .child("Uncommitted changes"),
                    )
                    .context_menu(move |menu, _, _| {
                        let weak = weak.clone();
                        menu.item(
                            PopupMenuItem::new("Stash changes…")
                                .icon(Icon::new(IconName::Archive))
                                .on_click(move |_, window, cx| {
                                    let _ = weak.update(cx, |this, cx| {
                                        this.ask(Ask::Stash, String::new(), window, cx)
                                    });
                                }),
                        )
                    })
                    .into_any_element()
            }
            Line::Earlier(count) => row
                .cursor_pointer()
                .hover(|s| s.bg(ui::surface_hover()))
                .on_click(cx.listener(|this, _, _, cx| this.load_earlier(cx)))
                .child(
                    self.graph_cell(RowGraph::earlier()).child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(graph::PAD * 2.))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .h(rems(18. / 16.))
                                    .min_w(rems(18. / 16.))
                                    .px_1()
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(ui::info().opacity(0.16))
                                    .font_family(ui::mono())
                                    .text_size(ui::code_rems(10.))
                                    .text_color(ui::info())
                                    .child(count.to_string()),
                            ),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(ui::faint())
                        .child("Earlier history"),
                )
                .when(self.loading, |d| {
                    d.child(Spinner::new().xsmall().color(ui::muted()))
                })
                .into_any_element(),
            Line::Commit(i) => self.render_commit(row, i, window, cx),
        }
    }

    /// The refs pointing at `commit` as chips: local branches (✓ on HEAD's,
    /// a globe when a remote branch of the same name is there too), remote
    /// branches no local one mirrors, then tags. `lane` names the branch the
    /// graph says this commit starts (from a merge's subject) when no ref
    /// does any more.
    fn ref_chips(
        &self,
        commit: &Commit,
        lane: Option<&str>,
        lane_color: Hsla,
        max: usize,
    ) -> Vec<AnyElement> {
        let current = self.read().and_then(|h| h.current.clone());
        let mut chips = Vec::new();
        let local: Vec<&String> = commit
            .refs
            .iter()
            .filter_map(|r| match r {
                RefLabel::Branch(name) => Some(name),
                _ => None,
            })
            .collect();
        for name in &local {
            let mirrored = commit.refs.iter().any(|r| {
                matches!(r, RefLabel::Remote(remote)
                    if remote.split_once('/').is_some_and(|(_, b)| b == name.as_str()))
            });
            let is_head = commit.head && current.as_deref() == Some(name.as_str());
            let color = if is_head { ui::accent() } else { lane_color };
            chips.push(
                ref_chip(
                    clip(name, max),
                    color,
                    is_head.then_some(IconName::Check),
                    mirrored.then_some(IconName::Globe),
                )
                .into_any_element(),
            );
        }
        for label in &commit.refs {
            match label {
                RefLabel::Remote(remote)
                    if !remote
                        .split_once('/')
                        .is_some_and(|(_, b)| local.iter().any(|l| l.as_str() == b)) =>
                {
                    chips.push(
                        ref_chip(clip(remote, max), ui::muted(), Some(IconName::Globe), None)
                            .into_any_element(),
                    )
                }
                RefLabel::Tag(tag) => chips.push(
                    ref_chip(clip(tag, max), ui::info(), Some(IconName::Tag), None)
                        .into_any_element(),
                ),
                _ => {}
            }
        }
        if let Some(name) = lane
            && !commit.refs.iter().any(|r| match r {
                RefLabel::Branch(b) => b == name,
                RefLabel::Remote(r) => r.split_once('/').is_some_and(|(_, b)| b == name),
                RefLabel::Tag(_) => false,
            })
        {
            chips.push(ref_chip(clip(name, max), lane_color, None, None).into_any_element());
        }
        chips
    }

    fn render_commit(
        &self,
        row: Stateful<Div>,
        i: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let commit = &self.commits[i];
        let Some(graph_row) = self.graph.rows.get(i) else {
            return row.into_any_element();
        };
        let filtering = self.filtering();
        let dirty_above =
            !filtering && i == 0 && matches!(self.lines.first(), Some(Line::Uncommitted));
        let selected = self.selected.as_ref() == Some(&commit.hash);
        let lane_color = graph::lane_color(graph_row.color);
        let fit = self.fit(window);

        // Ref chips need room beside the subject: a narrow pane drops them
        // (the hover card still names the branch), others cut long names.
        let chips = if fit > Fit::Narrow {
            let lane = graph_row
                .lane_tip
                .then_some(graph_row.branch.as_deref())
                .flatten();
            self.ref_chips(
                commit,
                lane,
                lane_color,
                if fit == Fit::Wide { 28 } else { 18 },
            )
        } else {
            Vec::new()
        };
        // Filtered rows aren't neighbors in the graph: only their node.
        let graph = if filtering {
            RowGraph::node_only(graph_row, commit.head)
        } else {
            RowGraph::commit(graph_row, commit.head, dirty_above)
        };

        let card = CommitCard {
            commits: self.commits.clone(),
            index: i,
            branch: graph_row
                .branch
                .clone()
                .or_else(|| self.read().and_then(|h| h.branch().map(str::to_string))),
            now_ms: self.now_ms,
        };
        let hash = commit.hash.clone();
        let menu = self.commit_menu(i, cx);
        row.cursor_pointer()
            .when(selected, |d| d.bg(ui::raised()))
            .when(!selected, |d| d.hover(|s| s.bg(ui::surface_hover())))
            .on_click(cx.listener(move |this, _, _, cx| this.select(hash.clone(), cx)))
            .tooltip(move |_, cx| cx.new(|_| card.clone()).into())
            .context_menu(menu)
            .child(self.graph_cell(graph))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    // Chips that don't fit are cut, leaving the subject at
                    // least its minimum width.
                    .when(!chips.is_empty(), |d| {
                        d.child(
                            h_flex()
                                .flex_shrink(1.)
                                .min_w_0()
                                .overflow_hidden()
                                .gap_1p5()
                                .children(chips.into_iter().map(|c| div().flex_none().child(c))),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems(96. / 16.))
                            .truncate()
                            .text_sm()
                            .text_color(if graph_row.mainline || filtering {
                                ui::fg()
                            } else {
                                ui::muted()
                            })
                            .child(commit.subject.clone()),
                    ),
            )
            .when(fit > Fit::Narrow, |d| {
                d.child(
                    h_flex()
                        .flex_none()
                        .when(fit == Fit::Wide, |d| d.w(rems(AUTHOR_WIDTH / 16.)))
                        .gap_2()
                        .child(avatar(
                            &commit.author,
                            super::avatars::picture(&commit.email, cx),
                            !commit.coauthors.is_empty(),
                            18.,
                        ))
                        .when(fit == Fit::Wide, |d| {
                            d.child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(ui::muted())
                                    .child(commit.author.clone()),
                            )
                        }),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(rems(TIME_WIDTH / 16.))
                    .whitespace_nowrap()
                    .text_right()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(12.))
                    .text_color(ui::faint())
                    .child(short_time(self.now_ms, commit.time)),
            )
            .when(fit == Fit::Wide, |d| {
                d.child(
                    div()
                        .flex_none()
                        .w(rems(SHA_WIDTH / 16.))
                        .whitespace_nowrap()
                        .text_right()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(11.5))
                        .text_color(ui::faint())
                        .child(commit.short().to_string()),
                )
            })
            .into_any_element()
    }

    /// The right-click menu of a commit row. Rendering a row only captures
    /// the shared commit list and its index; the menu reads the rest when it
    /// opens.
    fn commit_menu(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> impl Fn(
        gpui_kit::component::menu::PopupMenu,
        &mut Window,
        &mut Context<gpui_kit::component::menu::PopupMenu>,
    ) -> gpui_kit::component::menu::PopupMenu
    + 'static {
        let weak = cx.entity().downgrade();
        let busy = self.busy.is_some();
        let history = self.read();
        let current = history.and_then(|h| h.current.clone());
        let has_remote = history.is_some_and(|h| !h.remotes.is_empty());
        let commits = self.commits.clone();
        move |menu, _, _| {
            let commit = &commits[index];
            let hash = commit.hash.clone();
            let short = commit.short().to_string();
            let subject = commit.subject.clone();
            let head = commit.head;
            let merge = commit.is_merge();
            let branches: Vec<String> = commit
                .refs
                .iter()
                .filter_map(|r| match r {
                    RefLabel::Branch(name) if Some(name) != current.as_ref() => Some(name.clone()),
                    _ => None,
                })
                .collect();
            let tags: Vec<String> = commit
                .refs
                .iter()
                .filter_map(|r| match r {
                    RefLabel::Tag(tag) => Some(tag.clone()),
                    _ => None,
                })
                .collect();
            let target = current.clone().unwrap_or_else(|| "HEAD".into());
            // One menu row that runs `run` on the view.
            let row = |label: String, icon: IconName, enabled: bool, run: MenuAction| {
                let weak = weak.clone();
                PopupMenuItem::new(label)
                    .icon(Icon::new(icon))
                    .disabled(!enabled)
                    .on_click(move |_, window, cx| {
                        let run = run.clone();
                        let _ = weak.update(cx, |this, cx| run(this, window, cx));
                    })
            };
            let h = hash.clone();
            let mut menu = menu.item(row(
                "Copy commit SHA".into(),
                IconName::Copy,
                true,
                Rc::new(move |this, _, cx| this.copy_hash(&h, cx)),
            ));
            let h = hash.clone();
            menu = menu.item(row(
                "Copy commit message".into(),
                IconName::Copy,
                true,
                Rc::new(move |this, _, cx| this.copy_message(h.clone(), cx)),
            ));
            menu = menu.separator();
            for branch in &branches {
                let b = branch.clone();
                menu = menu.item(row(
                    format!("Switch to {branch}"),
                    IconName::GitBranch,
                    !busy,
                    Rc::new(move |this, _, cx| this.switch(b.clone(), cx)),
                ));
            }
            let h = hash.clone();
            menu = menu.item(row(
                "Check out this commit".into(),
                IconName::GitCommitHorizontal,
                !busy,
                Rc::new(move |this, _, cx| this.checkout_detached(h.clone(), cx)),
            ));
            let (h, s) = (hash.clone(), short.clone());
            menu = menu.item(row(
                "New branch here…".into(),
                IconName::GitBranchPlus,
                !busy,
                Rc::new(move |this, window, cx| {
                    let ask = Ask::Branch {
                        at: h.clone(),
                        label: s.clone(),
                    };
                    this.ask(ask, String::new(), window, cx)
                }),
            ));
            let (h, s) = (hash.clone(), short.clone());
            menu = menu.item(row(
                "New tag here…".into(),
                IconName::Tag,
                !busy,
                Rc::new(move |this, window, cx| {
                    let ask = Ask::Tag {
                        at: h.clone(),
                        label: s.clone(),
                    };
                    this.ask(ask, String::new(), window, cx)
                }),
            ));
            menu = menu.separator();
            let h = hash.clone();
            menu = menu.item(row(
                "Cherry-pick".into(),
                IconName::Cherry,
                !busy && !head,
                Rc::new(move |this, _, cx| this.cherry_pick(h.clone(), merge, cx)),
            ));
            let h = hash.clone();
            menu = menu.item(row(
                "Revert".into(),
                IconName::Undo2,
                !busy,
                Rc::new(move |this, _, cx| this.revert(h.clone(), merge, cx)),
            ));
            let h = hash.clone();
            menu = menu.item(row(
                format!("Reset {target} here, keep changes"),
                IconName::RefreshCw,
                !busy && !head,
                Rc::new(move |this, _, cx| this.reset(h.clone(), ResetMode::KeepChanges, cx)),
            ));
            let h = hash.clone();
            menu = menu.item(row(
                format!("Reset {target} here, discard changes…"),
                IconName::TriangleAlert,
                !busy,
                Rc::new(move |this, window, cx| this.confirm_reset_discard(h.clone(), window, cx)),
            ));
            if head {
                menu = menu.item(row(
                    "Edit message…".into(),
                    IconName::Pencil,
                    !busy,
                    Rc::new(|this, window, cx| this.reword(window, cx)),
                ));
            }
            if !tags.is_empty() {
                menu = menu.separator();
                for tag in &tags {
                    let t = tag.clone();
                    menu = menu.item(row(
                        format!("Push tag {tag}"),
                        IconName::ArrowUpFromLine,
                        !busy && has_remote,
                        Rc::new(move |this, _, cx| this.push_tag(t.clone(), cx)),
                    ));
                    let t = tag.clone();
                    menu = menu.item(row(
                        format!("Delete tag {tag}…"),
                        IconName::Trash,
                        !busy,
                        Rc::new(move |this, window, cx| this.delete_tag(t.clone(), window, cx)),
                    ));
                }
            }
            let (s, subj) = (short.clone(), subject.clone());
            menu.separator().item(row(
                "Explain with the agent".into(),
                IconName::Sparkles,
                true,
                Rc::new(move |this, window, cx| this.explain(&s, &subj, window, cx)),
            ))
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.filtering() && self.lines.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .py_8()
                .gap_2()
                .child(Icon::new(IconName::Search).text_color(ui::faint()))
                .child(
                    div()
                        .text_sm()
                        .text_color(ui::muted())
                        .child("No loaded commit matches the filter."),
                )
                .into_any_element();
        }
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                uniform_list(
                    "history-list",
                    self.lines.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let lines = this.lines.clone();
                        range
                            .map(|ix| this.render_line(ix, lines[ix], window, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .py_2()
                .track_scroll(&self.scroll),
            )
            .vertical_scrollbar(&self.scroll)
            .into_any_element()
    }

    fn render_body(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let state = |icon: IconName, text: &'static str| {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .py_8()
                .gap_2()
                .child(Icon::new(icon).text_color(ui::faint()))
                .child(div().text_sm().text_color(ui::muted()).child(text))
                .into_any_element()
        };
        match &self.history {
            None => h_flex()
                .flex_1()
                .justify_center()
                .py_8()
                .gap_2()
                .text_sm()
                .text_color(ui::muted())
                .child(Spinner::new().small().color(ui::muted()))
                .child("Reading history…")
                .into_any_element(),
            Some(None) => state(
                IconName::FolderGit,
                "This folder isn't in a Git repository.",
            ),
            Some(Some(_)) if self.commits.is_empty() => {
                state(IconName::GitCommitHorizontal, "No commits here yet.")
            }
            Some(Some(_)) => v_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_list(cx))
                .children(self.render_detail(window, cx))
                .children(self.render_activity(cx))
                .into_any_element(),
        }
    }

    /// The selected commit: who and when, its whole message and the files it
    /// changed; a file opens what the commit did to it.
    fn render_detail(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let commit = self.selected_commit()?;
        let detail = self.detail.as_ref().filter(|d| d.hash == commit.hash)?;
        let fit = self.fit(window);
        let body = detail.message.as_deref().and_then(|m| {
            m.split_once('\n')
                .map(|(_, rest)| rest.trim().to_string())
                .filter(|rest| !rest.is_empty())
        });
        let hash = commit.hash.clone();
        let header = h_flex()
            .gap_2()
            .child(avatar(
                &commit.author,
                super::avatars::picture(&commit.email, cx),
                !commit.coauthors.is_empty(),
                18.,
            ))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(ui::fg())
                    .child(commit.author.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(ui::faint())
                    .child(relative_time(self.now_ms, commit.time * 1000)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(12.))
                    .text_color(ui::muted())
                    .child(commit.short().to_string()),
            )
            .child(
                Button::new("git-detail-copy")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Copy))
                    .tooltip("Copy commit SHA")
                    .on_click(cx.listener(move |this, _, _, cx| this.copy_hash(&hash, cx))),
            )
            .child(
                Button::new("git-detail-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::Close))
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected = None;
                        this.detail = None;
                        cx.notify();
                    })),
            );
        let files: AnyElement = match &detail.files {
            None => h_flex()
                .gap_2()
                .text_xs()
                .text_color(ui::muted())
                .child(Spinner::new().xsmall().color(ui::muted()))
                .child("Reading the changes…")
                .into_any_element(),
            Some(files) if files.is_empty() => div()
                .text_xs()
                .text_color(ui::faint())
                .child(if commit.is_merge() {
                    "No changes against the first parent."
                } else {
                    "No file changes."
                })
                .into_any_element(),
            Some(files) => v_flex()
                .gap_0p5()
                .child(ui::section_label(format!(
                    "{} {} changed",
                    files.len(),
                    if files.len() == 1 { "file" } else { "files" }
                )))
                .children(
                    files
                        .iter()
                        .enumerate()
                        .map(|(ix, file)| self.file_row(ix, file, fit, cx)),
                )
                .into_any_element(),
        };
        Some(
            v_flex()
                .id("git-detail")
                .flex_none()
                .max_h(relative(DETAIL_SHARE))
                .overflow_y_scroll()
                .mx_2()
                .mb_1()
                .px_3()
                .py_2()
                .gap_2()
                .rounded_lg()
                .bg(ui::surface())
                .child(header)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ui::fg())
                        .child(commit.subject.clone()),
                )
                .when_some(body, |d, body| {
                    d.child(
                        div()
                            .text_xs()
                            .whitespace_normal()
                            .text_color(ui::muted())
                            .child(body),
                    )
                })
                .child(files)
                .into_any_element(),
        )
    }

    fn file_row(
        &self,
        ix: usize,
        file: &CommitFile,
        fit: Fit,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (dir, name) = match file.path.rsplit_once('/') {
            Some((dir, name)) => (format!("{dir}/"), name.to_string()),
            None => (String::new(), file.path.clone()),
        };
        let path = file.path.clone();
        let color = status_color(file.status);
        h_flex()
            .id(("git-file", ix))
            .h(rems(24. / 16.))
            .px_1p5()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(ui::surface_hover()))
            .on_click(
                cx.listener(move |this, _, window, cx| {
                    this.open_file_diff(path.clone(), window, cx)
                }),
            )
            .when_some(file.old_path.clone(), |d, old| {
                d.tooltip(move |window, cx| {
                    ui::tooltip(format!("Renamed from {old}")).build(window, cx)
                })
            })
            .child(
                div()
                    .flex_none()
                    .w(rems(14. / 16.))
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(11.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(color)
                    .child(file.status.to_string()),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_xs()
                    .when(fit > Fit::Narrow, |d| {
                        d.child(
                            div()
                                .flex_shrink(100.)
                                .min_w_0()
                                .truncate()
                                .text_color(ui::faint())
                                .child(dir),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .truncate()
                            .text_color(ui::fg())
                            .child(name),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_1p5()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(11.))
                    .map(|d| match (file.added, file.removed) {
                        (Some(added), Some(removed)) => d
                            .child(div().text_color(ui::success()).child(format!("+{added}")))
                            .child(
                                div()
                                    .text_color(ui::danger())
                                    .child(format!("\u{2212}{removed}")),
                            ),
                        _ => d.child(div().text_color(ui::faint()).child("binary")),
                    }),
            )
            .into_any_element()
    }

    /// A year of activity as a grid of days under the list, like GitHub's
    /// contribution graph: this repository's commits, or the user's GitHub
    /// contributions. A narrow pane shows the most recent weeks that fit.
    fn render_activity(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use super::Source;
        use gentle_core::git::activity::{self, Unit, WEEKS};
        let source = self.activity_source;
        // Nothing until this repository's commits are read.
        self.repo_days.as_ref()?;
        let unit = match source {
            Source::Repository => Unit::Commits,
            Source::GitHub => Unit::Contributions,
        };
        // The card's margins and padding, and the weekday labels beside it.
        let available = (self.width.get() - 16. - 24. - ACTIVITY_LABELS - 6.).max(0.);
        let weeks =
            (((available + ACTIVITY_GAP) / (ACTIVITY_MIN + ACTIVITY_GAP)) as usize).clamp(1, WEEKS);
        let cell = ((available - (weeks - 1) as f32 * ACTIVITY_GAP) / weeks as f32)
            .clamp(ACTIVITY_MIN, ACTIVITY_MAX);
        let step = cell + ACTIVITY_GAP;
        let shown = self.activity.as_ref().map(|grid| grid.last(weeks));
        let shade = |level: u8| match level {
            0 => ui::well(),
            1 => ui::accent().opacity(0.3),
            2 => ui::accent().opacity(0.5),
            3 => ui::accent().opacity(0.75),
            _ => ui::accent(),
        };
        let swatch = |level: u8| {
            div()
                .flex_none()
                .size(px(cell))
                .rounded(px(2.))
                .bg(shade(level))
        };
        // While the GitHub calendar loads, or when it can't, the grid's room
        // holds a line saying so: the card keeps its height.
        let grid: AnyElement = match shown {
            Some(shown) => {
                let months = div().relative().h(px(14.)).children(
                    activity::month_labels(shown)
                        .into_iter()
                        .map(|(column, name)| {
                            div()
                                .absolute()
                                .left(px(column as f32 * step))
                                .text_size(px(10.))
                                .text_color(ui::faint())
                                .child(name)
                        }),
                );
                let columns = h_flex().items_start().gap(px(ACTIVITY_GAP)).children(
                    shown.iter().enumerate().map(|(w, week)| {
                        v_flex()
                            .gap(px(ACTIVITY_GAP))
                            .children(week.iter().enumerate().map(|(d, day)| {
                                if day.future {
                                    return div().size(px(cell)).into_any_element();
                                }
                                let summary = activity::day_summary(day, unit);
                                swatch(day.level)
                                    .id(("git-activity-day", w * 7 + d))
                                    .hover(|s| s.border_1().border_color(ui::fg().opacity(0.6)))
                                    .tooltip(move |window, cx| {
                                        ui::tooltip(summary.clone()).build(window, cx)
                                    })
                                    .into_any_element()
                            }))
                    }),
                );
                v_flex().child(months).child(columns).into_any_element()
            }
            None => {
                let (busy, text) = match &self.github {
                    Some(Err(why)) if !self.github_loading => (false, why.clone()),
                    _ => (true, "Reading your GitHub contributions…".to_string()),
                };
                h_flex()
                    .h(px(14. + 7. * cell + 6. * ACTIVITY_GAP))
                    .gap_2()
                    .text_xs()
                    .text_color(ui::muted())
                    .when(busy, |d| {
                        d.child(Spinner::new().xsmall().color(ui::muted()))
                    })
                    .when(!busy, |d| {
                        d.child(
                            Icon::new(IconName::CircleAlert)
                                .xsmall()
                                .text_color(ui::warning()),
                        )
                    })
                    .child(div().min_w_0().whitespace_normal().child(text))
                    .into_any_element()
            }
        };
        // Mon, Wed and Fri beside their rows, as GitHub labels them.
        let weekdays = v_flex()
            .flex_none()
            .w(px(ACTIVITY_LABELS))
            .pt(px(14.))
            .gap(px(ACTIVITY_GAP))
            .children(
                ["", "Mon", "", "Wed", "", "Fri", ""]
                    .into_iter()
                    .map(|label| {
                        div()
                            .h(px(cell))
                            .flex()
                            .items_center()
                            .text_size(px(10.))
                            .text_color(ui::faint())
                            .child(label)
                    }),
            );
        let view = cx.entity().downgrade();
        let login = match (&self.github, source) {
            (Some(Ok(calendar)), Source::GitHub) => Some(format!("@{} on GitHub", calendar.login)),
            _ => None,
        };
        let heading = match shown {
            Some(shown) => activity::heading(activity::total(shown), shown.len(), unit),
            None => "Your GitHub contributions".to_string(),
        };
        let header = h_flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(ui::fg())
                    .child(heading),
            )
            .child(ui::segmented(
                "git-activity-source",
                &["This repo", "GitHub"],
                Some(usize::from(source == Source::GitHub)),
                move |choice, _, cx| {
                    let source = if *choice == 1 {
                        Source::GitHub
                    } else {
                        Source::Repository
                    };
                    let _ = view.update(cx, |this, cx| this.set_activity_source(source, cx));
                },
            ));
        let legend = h_flex()
            .gap(px(ACTIVITY_GAP))
            .text_size(px(10.))
            .text_color(ui::faint())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .when_some(login, |d, login| d.child(login)),
            )
            .child(div().mr_1().child("Less"))
            .children((0..=4).map(swatch))
            .child(div().ml_1().child("More"));
        Some(
            v_flex()
                .flex_none()
                .mx_2()
                .mb_1()
                .px_3()
                .py_2()
                .gap_2()
                .rounded_lg()
                .bg(ui::surface())
                .child(header)
                .child(
                    h_flex()
                        .items_start()
                        .gap(px(6.))
                        .child(weekdays)
                        .child(grid),
                )
                .child(legend)
                .into_any_element(),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let _ = cx;
        let line = |icon: Option<(IconName, Hsla)>, text: String, color: Hsla| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1p5()
                .text_xs()
                .when_some(icon, |d, (icon, tone)| {
                    d.child(Icon::new(icon).xsmall().text_color(tone))
                })
                .child(div().min_w_0().truncate().text_color(color).child(text))
        };
        let status = match (&self.busy, &self.outcome, self.read()) {
            (Some(doing), _, _) => h_flex()
                .flex_1()
                .gap_1p5()
                .text_xs()
                .text_color(ui::muted())
                .child(Spinner::new().xsmall().color(ui::muted()))
                .child(*doing),
            (None, Some(Outcome::Done(text)), _) => line(
                Some((IconName::CircleCheck, ui::success())),
                text.clone(),
                ui::muted(),
            ),
            (None, Some(Outcome::Failed(text)), _) => line(
                Some((IconName::CircleAlert, ui::danger())),
                text.lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or(text)
                    .to_string(),
                ui::danger(),
            ),
            (None, None, Some(history)) if !self.commits.is_empty() => {
                let shown = self.matched;
                let text = if self.filtering() {
                    format!("{shown} of {} loaded commits match", self.commits.len())
                } else {
                    format!(
                        "Showing {} of {} commits",
                        self.commits.len(),
                        history.total
                    )
                };
                line(None, text, ui::muted())
            }
            _ => div().flex_1(),
        };
        // Inside a pane: separated by space, no line (the chrome has none).
        h_flex()
            .flex_none()
            .h(rems(32. / 16.))
            .px_4()
            .gap_2()
            .child(status)
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = self.width.clone();
        let view = cx.entity_id();
        v_flex()
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .min_h_0()
            // Measures the pane, so the next frame lays the rows out for it.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let w = f32::from(bounds.size.width);
                        if (w - width.get()).abs() > 1. {
                            width.set(w);
                            cx.notify(view);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(self.render_header(window, cx))
            .children(self.render_toolbar(window, cx))
            .children(self.render_prompt(window, cx))
            .children(self.render_banner(cx))
            .child(self.render_body(window, cx))
            .child(self.render_footer(cx))
    }
}

/// The card a commit row shows on hover: author and co-authors, the full
/// subject, then hash, changes, time and branch.
#[derive(Clone)]
struct CommitCard {
    commits: Rc<Vec<Commit>>,
    index: usize,
    branch: Option<String>,
    now_ms: i64,
}

impl Render for CommitCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let commit = &self.commits[self.index];
        let detail = |label: &'static str, value: AnyElement| {
            h_flex()
                .gap_4()
                .justify_between()
                .text_xs()
                .child(div().flex_none().text_color(ui::muted()).child(label))
                .child(value)
        };
        let text = |value: String| {
            div()
                .min_w_0()
                .truncate()
                .text_color(ui::fg())
                .child(value)
                .into_any_element()
        };
        let changed = match commit.stat {
            Some(stat) => h_flex()
                .gap_1p5()
                .child(div().text_color(ui::fg()).child(format!(
                    "{} {}",
                    stat.files,
                    if stat.files == 1 { "file" } else { "files" }
                )))
                .child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(ui::success())
                        .child(format!("+{}", stat.insertions)),
                )
                .child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(ui::danger())
                        .child(format!("\u{2212}{}", stat.deletions)),
                )
                .into_any_element(),
            None if commit.is_merge() => text(format!("Merge of {} parents", commit.parents.len())),
            None => text("Nothing".into()),
        };
        let coauthors = (!commit.coauthors.is_empty())
            .then(|| format!("Co-authored with {}", commit.coauthors.join(", ")));
        ui::island(cx).w(rems(320. / 16.)).child(
            v_flex()
                .p_3()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .child(avatar(
                            &commit.author,
                            super::avatars::picture(&commit.email, cx),
                            coauthors.is_some(),
                            30.,
                        ))
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(ui::fg())
                                        .child(commit.author.clone()),
                                )
                                .when_some(coauthors, |d, line| {
                                    d.child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(ui::muted())
                                            .child(line),
                                    )
                                }),
                        ),
                )
                .child(
                    div()
                        .text_size(rems(13. / 16.))
                        .text_color(ui::fg())
                        .child(commit.subject.clone()),
                )
                .child(
                    v_flex()
                        .gap_1p5()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(ui::well())
                        .child(detail(
                            "Commit",
                            div()
                                .font_family(ui::mono())
                                .text_size(ui::code_rems(12.))
                                .text_color(ui::fg())
                                .child(commit.short().to_string())
                                .into_any_element(),
                        ))
                        .child(detail("Changed", changed))
                        .child(detail(
                            "Committed",
                            text(relative_time(self.now_ms, commit.time * 1000)),
                        ))
                        .when_some(self.branch.clone(), |d, branch| {
                            d.child(detail("Branch", text(branch)))
                        }),
                ),
        )
    }
}
