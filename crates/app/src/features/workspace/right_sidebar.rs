//! Workspace tools share the existing sidebar slot and its resize/slide state.

use super::*;

pub(super) const ACTIVITY_BAR_WIDTH: f32 = 42.;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Files,
    Git,
    Engram,
    History,
}

impl Workspace {
    fn active_right_tool(&self, cx: &App) -> Tool {
        if self.sidebar_history_active {
            Tool::History
        } else if self.sidebar_memory_active {
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
        if self.sidebar_history_active {
            self.sidebar_history.as_ref().map(|(_, view)| view.clone().into())
        } else if self.sidebar_memory_active {
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

    pub(super) fn ensure_sidebar_history(
        &mut self,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_history.as_ref().is_some_and(|(path, _)| *path == cwd) {
            return;
        }
        let view = cx.new(|cx| crate::features::history::HistoryView::new(cwd.clone(), window, cx));
        self.sidebar_history = Some((cwd, view));
    }

    pub(super) fn show_sidebar_history(
        &mut self,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_sidebar_history(cwd, window, cx);
        self.sidebar_history_active = true;
        self.sidebar_memory_active = false;
        if self.sidebar_visible {
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(false));
            cx.notify();
        } else {
            self.toggle_sidebar(cx);
        }
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
        if tool == Tool::History {
            let cwd = self.root().to_path_buf();
            self.show_sidebar_history(cwd, window, cx);
            return;
        }
        self.sidebar_history_active = false;
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
            .bg(ui::panel())
            .child(
                h_flex()
                    .id("right-sidebar-toggle")
                    .size(rems(34. / 16.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_color(ui::muted())
                    .hover(|d| d.bg(ui::surface_hover()))
                    .tooltip({
                        let label = format!(
                            "{} tools sidebar ({})",
                            if self.sidebar_visible { "Hide" } else { "Show" },
                            ui::shortcut("B")
                        );
                        move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
                    .child(Icon::new(if self.sidebar_visible {
                        IconName::PanelRightClose
                    } else {
                        IconName::PanelRightOpen
                    })),
            );
        for (index, (tool, icon, label)) in [
            (Tool::Files, IconName::Files, "Files"),
            (Tool::Git, IconName::GitBranch, "Git"),
            (Tool::Engram, IconName::Brain, "Engram"),
            (Tool::History, IconName::GitGraph, "Git history"),
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
        // Native screens share the command palette's actions. Keep them
        // above the bottom administration shortcuts, scrolling if needed.
        let mut commands = v_flex()
            .id("right-native-commands")
            .flex_1()
            .min_h_0()
            .w_full()
            .items_center()
            .gap_1()
            .overflow_y_scroll()
            .child(div().flex_1().min_h_0());
        for (index, (icon, label, detail, action)) in
            crate::features::palette::sidebar_commands(self.tab_layout)
                .into_iter()
                .enumerate()
        {
            // History is now a docked tool in the top group.
            if action == PaletteAction::History {
                continue;
            }
            let selected = match &action {
                PaletteAction::Devices => self.devices.read(cx).visible(),
                PaletteAction::Docs => matches!(self.overlay, Some((Overlay::Docs(_), _))),
                PaletteAction::Usage => matches!(self.overlay, Some((Overlay::Usage(_), _))),
                _ => false,
            };
            let tooltip = format!("{label} — {detail}");
            commands = commands.child(
                h_flex()
                    .id(("right-native-command", index))
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
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.overlay.is_some() {
                            this.close_overlay(window, cx);
                        }
                        if action == PaletteAction::Devices {
                            this.devices.update(cx, |panel, cx| panel.toggle(cx));
                        } else if !selected {
                            this.run_palette_action(action.clone(), window, cx);
                        }
                    }))
                    .child(Icon::new(icon)),
            );
        }
        rail = rail.child(commands);
        // These screens keep their existing overlay behavior. Engram is already
        // available as a docked tool above, so it does not need a duplicate.
        type Open = fn(&mut Workspace, &mut Window, &mut Context<Workspace>);
        let screens: [(&str, IconName, &str, bool, Open); 4] = [
            (
                "right-profiles",
                IconName::Route,
                "Profiles",
                matches!(self.overlay, Some((Overlay::Profiles(_), _))),
                Self::open_profiles,
            ),
            (
                "right-sessions",
                IconName::RotateCcwClock,
                "Sessions",
                matches!(self.overlay, Some((Overlay::Sessions(_), _))),
                Self::open_sessions,
            ),
            (
                "right-providers",
                IconName::KeyRound,
                "Providers",
                matches!(self.overlay, Some((Overlay::Providers(_), _))),
                Self::open_providers,
            ),
            (
                "right-configuration",
                IconName::Settings,
                "Configuration",
                matches!(self.overlay, Some((Overlay::Settings(_), _))),
                Self::open_settings,
            ),
        ];
        for (id, icon, label, selected, open) in screens {
            let tooltip = match id {
                "right-configuration" => format!(
                    "{label} ({})",
                    ui::platform_shortcut("⌘,", "Ctrl+,")
                ),
                "right-sessions" => format!(
                    "{label} ({})",
                    ui::platform_shortcut("⌘⇧O", "Ctrl+Shift+O")
                ),
                _ => label.to_string(),
            };
            rail = rail.child(
                h_flex()
                    .id(id)
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
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if selected {
                            this.close_overlay(window, cx);
                        } else {
                            open(this, window, cx);
                        }
                    }))
                    .child(Icon::new(icon)),
            );
        }
        rail
    }
}
