//! Workspace tools share the existing sidebar slot and its resize/slide state.

use super::*;

pub(super) const ACTIVITY_BAR_WIDTH: f32 = 42.;

#[derive(Clone, Copy)]
struct ShortcutDrag(LayoutTool);

impl Workspace {
    pub(super) fn active_right_tool(&self, cx: &App) -> LayoutTool {
        if self.sidebar_history_active {
            LayoutTool::History
        } else if self.sidebar_memory_active {
            LayoutTool::Engram
        } else if self
            .sidebar
            .as_ref()
            .is_some_and(|tree| tree.read(cx).is_git_tab())
        {
            LayoutTool::Git
        } else {
            self.sidebar.as_ref().map_or(self.layout_profiles.current().dock, |_| LayoutTool::Files)
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
        self.layout_profiles.current_mut().dock = LayoutTool::History;
        self.schedule_save(cx);
        if self.sidebar_visible {
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(false));
            cx.notify();
        } else {
            self.toggle_sidebar(cx);
        }
    }

    pub(super) fn select_right_tool(
        &mut self,
        tool: LayoutTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sidebar_visible && self.active_right_tool(cx) == tool {
            self.toggle_sidebar(cx);
            return;
        }
        if tool == LayoutTool::History {
            let cwd = self.root().to_path_buf();
            self.show_sidebar_history(cwd, window, cx);
            return;
        }
        self.sidebar_history_active = false;
        self.sidebar_memory_active = tool == LayoutTool::Engram;
        self.layout_profiles.current_mut().dock = tool;
        self.schedule_save(cx);
        if self.sidebar_memory_active {
            self.ensure_sidebar_memory(window, cx);
        } else if let Some(tree) = self.sidebar.clone() {
            tree.update(cx, |tree, cx| {
                tree.select_sidebar_tool(tool == LayoutTool::Git, window, cx);
            });
        }
        if self.sidebar_visible {
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(self.files_sidebar_visible()));
            cx.notify();
        } else {
            self.toggle_sidebar(cx);
        }
    }

    /// Tower has its own tab, unlike the docked tools and overlay screens.
    fn shown_tower(&self) -> Option<PaneId> {
        self.tab()?.leaves().into_iter().find(|id| {
            self.panes.get(id).is_some_and(|pane| pane.item.kind == PaneKind::Tower)
        })
    }

    fn agents_window_open(&self, cx: &App) -> bool {
        self.active_session().is_some_and(|session| session.read(cx).agents_panel_open())
    }

    fn toggle_rail_command(
        &mut self,
        action: PaletteAction,
        selected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
        match action {
            PaletteAction::Devices => self.devices.update(cx, |panel, cx| panel.toggle(cx)),
            PaletteAction::Agents if self.agents_window_open(cx) => {
                if let Some(session) = self.active_session() {
                    session.update(cx, |session, cx| session.close_agents_panel(cx));
                }
            }
            PaletteAction::Screen(Screen::Tower) if self.shown_tower().is_some() => {
                if let Some(id) = self.shown_tower() {
                    self.close_pane(id, window, cx);
                }
            }
            _ if !selected => self.run_palette_action(action, window, cx),
            _ => {}
        }
        cx.notify();
    }

    fn rail_has_open_panel(&self, cx: &App) -> bool {
        self.sidebar_visible
            || self.overlay.is_some()
            || self.devices.read(cx).visible()
            || self.agents_window_open(cx)
    }

    fn toggle_rail_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.rail_has_open_panel(cx) {
            self.toggle_sidebar(cx);
            return;
        }
        if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
        if self.devices.read(cx).visible() {
            self.devices.update(cx, |panel, cx| panel.hide(cx));
        }
        if let Some(session) = self.active_session() {
            session.update(cx, |session, cx| session.close_agents_panel(cx));
        }
        if self.sidebar_visible {
            self.toggle_sidebar(cx);
        }
        cx.notify();
    }

    pub(super) fn navigation_visible(&self) -> bool {
        let profile = self.layout_profiles.current();
        profile.workspace_visible
            || (profile.tabs_visible && self.tab_layout == TabLayout::Vertical)
    }

    /// Capture live resize/visibility changes in just the active preset.
    pub(super) fn presentation_snapshot(&self, cx: &App) -> LayoutProfiles {
        let mut profiles = self.layout_profiles.clone();
        let p = profiles.current_mut();
        p.tab_layout = self.tab_layout;
        p.sidebar_visible = self.sidebar_visible;
        p.dock = self.active_right_tool(cx);
        p.tabs_width = self.tabs_sidebar_width;
        p.files_width = self.files_sidebar_width;
        profiles
    }

    pub(super) fn apply_presentation(
        &mut self,
        mut profiles: LayoutProfiles,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        profiles.normalize();
        let p = profiles.current().clone();
        let switched = profiles.active != self.layout_profiles.active;
        let old_nav = self.navigation_visible();
        let old_dock = self.active_right_tool(cx);
        self.cancel_workspace_rename_state(cx);
        self.layout_profiles = profiles;
        if let Some(tree) = self.sidebar.clone() {
            tree.update(cx, |tree, cx| tree.set_original_tabs(p.original_controls, cx));
        }
        self.tab_layout = p.tab_layout;
        self.sidebar_compact = !p.workspace_visible;
        self.tabs_sidebar_width = p.tabs_width;
        self.files_sidebar_width = p.files_width;
        if old_nav != self.navigation_visible() {
            self.tabs_sidebar_motion.retarget(old_nav, cx.reduce_motion());
        }
        if old_dock != p.dock {
            self.sidebar_memory_active = p.dock == LayoutTool::Engram;
            self.sidebar_history_active = p.dock == LayoutTool::History;
            if self.sidebar_memory_active {
                self.ensure_sidebar_memory(window, cx);
            } else if self.sidebar_history_active {
                self.ensure_sidebar_history(self.root().to_path_buf(), window, cx);
            } else if let Some(tree) = self.sidebar.clone() {
                tree.update(cx, |tree, cx| {
                    tree.select_sidebar_tool(p.dock == LayoutTool::Git, window, cx);
                });
            }
        }
        if switched && !p.sidebar_visible {
            // A focus layout hides auxiliary surfaces without closing any
            // agent session, device, or central work tab.
            if self.devices.read(cx).visible() {
                self.devices.update(cx, |panel, cx| panel.hide(cx));
            }
            if let Some(session) = self.active_session() {
                session.update(cx, |session, cx| session.close_agents_panel(cx));
            }
        }
        if self.sidebar_visible != p.sidebar_visible {
            self.toggle_sidebar(cx);
        } else {
            cx.emit(WorkspaceEvent::FilesVisibilityChanged(self.files_sidebar_visible()));
        }
        self.schedule_save(cx);
        cx.notify();
    }

    pub(super) fn show_navigation(&mut self, cx: &mut Context<Self>) {
        let was_open = self.navigation_visible();
        let p = self.layout_profiles.current_mut();
        p.tabs_visible = !(p.tabs_visible || p.workspace_visible);
        p.workspace_visible = p.tabs_visible && self.tab_layout == TabLayout::Vertical;
        self.sidebar_compact = !p.workspace_visible;
        if was_open != self.navigation_visible() {
            self.tabs_sidebar_motion.retarget(was_open, cx.reduce_motion());
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn shortcut_selected(&self, tool: LayoutTool, cx: &App) -> bool {
        if tool.docked() {
            return self.sidebar_visible && self.active_right_tool(cx) == tool;
        }
        match tool {
            LayoutTool::Agents => self.agents_window_open(cx),
            LayoutTool::Devices => self.devices.read(cx).visible(),
            LayoutTool::Tower => self.shown_tower().is_some(),
            LayoutTool::Docs => matches!(self.overlay, Some((Overlay::Docs(_), _))),
            LayoutTool::Usage => matches!(self.overlay, Some((Overlay::Usage(_), _))),
            LayoutTool::Profiles => matches!(self.overlay, Some((Overlay::Profiles(_), _))),
            LayoutTool::Sessions => matches!(self.overlay, Some((Overlay::Sessions(_), _))),
            LayoutTool::Providers => matches!(self.overlay, Some((Overlay::Providers(_), _))),
            LayoutTool::Configuration => matches!(self.overlay, Some((Overlay::Settings(_), _))),
            _ => false,
        }
    }

    fn activate_shortcut(
        &mut self,
        tool: LayoutTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tool.docked() {
            if self.overlay.is_some() {
                self.close_overlay(window, cx);
            }
            self.select_right_tool(tool, window, cx);
            return;
        }
        let selected = self.shortcut_selected(tool, cx);
        let action = match tool {
            LayoutTool::Agents => PaletteAction::Agents,
            LayoutTool::Docs => PaletteAction::Docs,
            LayoutTool::Devices => PaletteAction::Devices,
            LayoutTool::Usage => PaletteAction::Usage,
            LayoutTool::Tower => PaletteAction::Screen(Screen::Tower),
            LayoutTool::Profiles => PaletteAction::Profiles,
            LayoutTool::Sessions => PaletteAction::Sessions,
            LayoutTool::Providers => PaletteAction::Providers,
            LayoutTool::Configuration => PaletteAction::Settings,
            _ => return,
        };
        self.toggle_rail_command(action, selected, window, cx);
    }

    fn move_shortcut(
        &mut self,
        tool: LayoutTool,
        placement: ToolPlacement,
        before: Option<LayoutTool>,
        cx: &mut Context<Self>,
    ) {
        let p = self.layout_profiles.current_mut();
        p.place(tool, placement);
        if let Some(target) = before {
            p.move_before(tool, target);
        }
        // A context-menu edit while Configuration is open must not be
        // overwritten by the settings view's next edit.
        if let Some((Overlay::Settings(view), _)) = &self.overlay {
            let view = view.clone();
            let profiles = self.presentation_snapshot(cx);
            view.update(cx, |view, cx| view.sync_presentation(profiles, cx));
        }
        self.schedule_save(cx);
        cx.notify();
    }

    fn shortcut_menu(
        &mut self,
        tool: LayoutTool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.entity().downgrade();
        let current = self.layout_profiles.current().shortcuts.iter()
            .find(|s| s.tool == tool).map(|s| s.placement);
        self.open_owned_menu("layout-tool-menu", position, move |mut menu, _, _| {
            for placement in ToolPlacement::ALL {
                let weak = weak.clone();
                let label = if current == Some(placement) {
                    format!("✓ {}", placement.label())
                } else {
                    placement.label().to_string()
                };
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.close_menu(window, cx);
                        this.move_shortcut(tool, placement, None, cx);
                    });
                }));
            }
            menu
        }, window, cx);
    }

    fn shortcut_button(
        &self,
        tool: LayoutTool,
        placement: ToolPlacement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.shortcut_selected(tool, cx);
        let size = if placement == ToolPlacement::Title { 28. } else { 34. };
        h_flex()
            .id(("layout-shortcut", tool as usize))
            .size(rems(size / 16.))
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(ui::raised()).text_color(ui::fg()))
            .when(!selected, |d| d.text_color(ui::muted()).hover(|d| d.bg(ui::surface_hover())))
            .tooltip(move |window, cx| Tooltip::new(tool.label()).build(window, cx))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.shortcut_menu(tool, event.position, window, cx);
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.activate_shortcut(tool, window, cx)))
            .on_drag(ShortcutDrag(tool), |drag, _, _, cx| {
                cx.new(|_| DragChip(drag.0.label().into()))
            })
            .drag_over::<ShortcutDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &ShortcutDrag, _, cx| {
                cx.stop_propagation();
                this.move_shortcut(drag.0, placement, Some(tool), cx);
            }))
            .child(Icon::new(tool.icon()))
            .into_any_element()
    }

    pub(super) fn render_title_shortcuts(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let placement = ToolPlacement::Title;
        h_flex()
            .id("layout-title-tools")
            .gap_1()
            .min_w(rems(20. / 16.))
            .drag_over::<ShortcutDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &ShortcutDrag, _, cx| {
                this.move_shortcut(drag.0, placement, None, cx);
            }))
            .children(self.layout_profiles.current().shortcuts.iter()
                .filter(|s| s.placement == placement)
                .map(|s| self.shortcut_button(s.tool, placement, cx)))
    }

    pub(super) fn render_activity_bar(
        &self,
        placement: ToolPlacement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel_open = self.rail_has_open_panel(cx);
        let id = if placement == ToolPlacement::Left { "layout-left-rail" } else { "layout-right-rail" };
        let mut rail = v_flex()
            .id(id)
            .w(rems(ACTIVITY_BAR_WIDTH / 16.))
            .h_full()
            .flex_none()
            .items_center()
            .gap_1()
            .py_2()
            .rounded_xl()
            .bg(ui::panel())
            .drag_over::<ShortcutDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
            .on_drop(cx.listener(move |this, drag: &ShortcutDrag, _, cx| {
                this.move_shortcut(drag.0, placement, None, cx);
            }));
        if placement == ToolPlacement::Right {
            rail = rail.child(
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
                    .tooltip(move |window, cx| Tooltip::new(if panel_open { "Hide panels" } else { "Show panel" }).build(window, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_rail_panels(window, cx)))
                    .child(Icon::new(if panel_open { IconName::PanelRightClose } else { IconName::PanelRightOpen })),
            );
        }
        let scroll_id = if placement == ToolPlacement::Left { "layout-left-scroll" } else { "layout-right-scroll" };
        rail.child(
            v_flex()
                .id(scroll_id)
                .flex_1()
                .min_h_0()
                .w_full()
                .items_center()
                .gap_1()
                .overflow_y_scroll()
                .children(self.layout_profiles.current().shortcuts.iter()
                    .filter(|s| s.placement == placement)
                    .map(|s| self.shortcut_button(s.tool, placement, cx))),
        )
    }
}
