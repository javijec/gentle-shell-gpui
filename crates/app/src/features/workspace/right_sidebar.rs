//! Workspace tools share the existing sidebar slot and its resize/slide state.

use super::*;

pub(super) const ACTIVITY_BAR_WIDTH: f32 = 42.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Files,
    Git,
    Engram,
}

impl Workspace {
    fn active_right_tool(&self, cx: &App) -> Tool {
        if self.sidebar_memory_active {
            Tool::Engram
        } else if self
            .sidebar
            .as_ref()
            .is_some_and(|tree| tree.read(cx).is_git_tab())
        {
            Tool::Git
        } else {
            Tool::Files
        }
    }

    pub(super) fn right_sidebar_view(&self) -> Option<AnyView> {
        if self.sidebar_memory_active {
            self.sidebar_memory.as_ref().map(|(view, _)| view.clone().into())
        } else {
            self.sidebar.clone().map(Into::into)
        }
    }

    pub(super) fn ensure_sidebar_memory(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_memory.is_some() {
            return;
        }
        let root = self.root().to_path_buf();
        let view = cx.new(|cx| MemoryView::new_docked(root, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, _: &MemoryEvent, _, cx| {
                if this.sidebar_memory_active && this.sidebar_visible {
                    this.toggle_sidebar(cx);
                }
            },
        );
        self.sidebar_memory = Some((view, subscription));
    }

    fn select_right_tool(
        &mut self,
        tool: Tool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_visible && self.active_right_tool(cx) == tool {
            self.toggle_sidebar(cx);
            return;
        }
        self.sidebar_memory_active = tool == Tool::Engram;
        if self.sidebar_memory_active {
            self.ensure_sidebar_memory(window, cx);
        } else if let Some(tree) = self.sidebar.clone() {
            tree.update(cx, |tree, cx| {
                tree.select_sidebar_tool(tool == Tool::Git, window, cx);
            });
        }
        if self.sidebar_visible {
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(self.files_sidebar_visible()));
            cx.notify();
        } else {
            self.toggle_sidebar(cx);
        }
    }

    pub(super) fn render_right_activity_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.active_right_tool(cx);
        let mut rail = v_flex()
            .w(rems(ACTIVITY_BAR_WIDTH / 16.))
            .h_full()
            .flex_none()
            .items_center()
            .gap_1()
            .py_2()
            .rounded_xl()
            .bg(ui::panel());
        for (index, (tool, icon, label)) in [
            (Tool::Files, IconName::Files, "Files"),
            (Tool::Git, IconName::GitBranch, "Git"),
            (Tool::Engram, IconName::Brain, "Engram"),
        ]
        .into_iter()
        .enumerate()
        {
            let selected = self.sidebar_visible && tool == active;
            rail = rail.child(
                h_flex()
                    .id(("right-tool", index))
                    .size(rems(34. / 16.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(ui::raised()).text_color(ui::fg()))
                    .when(!selected, |d| {
                        d.text_color(ui::muted()).hover(|d| d.bg(ui::surface_hover()))
                    })
                    .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_right_tool(tool, window, cx);
                    }))
                    .child(Icon::new(icon)),
            );
        }
        rail.child(div().flex_1())
            .child(self.render_screens_menu(cx))
    }
}
