//! Screens drawn over the workspace: palette, settings, providers, sessions,
//! usage, memory, profiles, onboarding; and the surge. Floating web and
//! device cards live in `stages.rs`.

use super::*;

impl Workspace {
    // ── Screens ─────────────────────────────────────────────────────────────

    /// Open the switcher on `category`; the same shortcut again closes it, the
    /// other one changes category.
    pub(super) fn open_palette(
        &mut self,
        category: Category,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((Overlay::Palette(view), _)) = &self.overlay {
            if view.read(cx).category() == category {
                self.close_overlay(window, cx);
            } else {
                view.update(cx, |view, cx| view.set_category(category, window, cx));
            }
            return;
        }
        self.start_active_session(window, cx);
        let commands = self
            .active_session()
            .map(|t| t.read(cx).commands())
            .unwrap_or_default();
        let workspaces = self.workspace_rows();
        let input = PaletteInput {
            commands,
            workspaces,
            root: self.space().root.clone(),
            category,
            tab_layout: self.tab_layout,
        };
        let view = cx.new(|cx| PaletteView::new(input, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Close => this.close_overlay(window, cx),
                PaletteEvent::Run(PaletteAction::RenameWorkspace { index, name }) => {
                    // Renaming happens in place; the switcher stays open.
                    this.rename_workspace(*index, name.clone(), cx);
                }
                PaletteEvent::Run(action) => {
                    this.close_overlay(window, cx);
                    this.run_palette_action(action.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Palette(view), subscription));
        cx.notify();
    }

    /// Every workspace as the switcher lists it, with what its agents do.
    pub(super) fn workspace_rows(&self) -> Vec<WorkspaceRow> {
        self.spaces
            .iter()
            .enumerate()
            .map(|(index, space)| WorkspaceRow {
                index,
                name: space.name.clone(),
                tabs: space.tabs.len(),
                current: index == self.current,
                activity: self.space_activity(space),
            })
            .collect()
    }

    pub(super) fn run_palette_action(
        &mut self,
        action: PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_agent = matches!(
            action,
            PaletteAction::Agents
                | PaletteAction::NewSession
                | PaletteAction::Command(_)
                | PaletteAction::Screen(_)
        );
        if needs_agent {
            self.ensure_agent_built(window, cx);
        }
        let tab = self.active_session();
        match action {
            PaletteAction::Providers => self.open_providers(window, cx),
            PaletteAction::Sessions => self.open_sessions(window, cx),
            PaletteAction::Profiles => self.open_profiles(window, cx),
            PaletteAction::Usage => self.open_usage(window, cx),
            PaletteAction::Memory => {
                let cwd = self.active_cwd(cx);
                self.open_memory(cwd, Default::default(), window, cx)
            }
            PaletteAction::Docs => self.open_docs(window, cx),
            PaletteAction::History => {
                let cwd = self.active_cwd(cx);
                self.open_history(cwd, window, cx)
            }
            PaletteAction::Devices => self.devices.update(cx, |panel, cx| panel.show(cx)),
            PaletteAction::Settings => self.open_settings(window, cx),
            PaletteAction::Setup => self.open_onboarding(None, window, cx),
            PaletteAction::Welcome => self.open_welcome(None, window, cx),
            PaletteAction::InstallGsd => self.install_gsd(window, cx),
            PaletteAction::OpenExplorer => self.open_explorer(window, cx),
            PaletteAction::NewTab => self.on_new_tab(&NewTab, window, cx),
            PaletteAction::NewTerminalTab => self.on_new_terminal_tab(&NewTerminalTab, window, cx),
            PaletteAction::NewCanvasTab => self.on_new_canvas_tab(&NewCanvasTab, window, cx),
            PaletteAction::TilePane => self.tile(None, window, cx),
            PaletteAction::SplitRight => self.split(SplitAxis::Horizontal, None, window, cx),
            PaletteAction::SplitDown => self.split(SplitAxis::Vertical, None, window, cx),
            PaletteAction::NewAgentPane => self.split_with_kind(PaneKind::Agent, window, cx),
            PaletteAction::NewTerminalPane => self.split_with_kind(PaneKind::Terminal, window, cx),
            PaletteAction::ClosePane => self.on_close_pane(&ClosePane, window, cx),
            PaletteAction::ReopenClosed => self.reopen_closed(window, cx),
            PaletteAction::ToggleSidebar => self.toggle_sidebar(cx),
            PaletteAction::SetTabLayout(layout) => self.set_tab_layout(layout, cx),
            PaletteAction::Agents => {
                if let Some(session) = tab {
                    // Agents is rendered inside its owning session, not as a
                    // workspace overlay. Reveal that pane before opening it:
                    // active_session can fall back to an agent in another tab.
                    let pane = self
                        .panes
                        .iter()
                        .find(|(_, pane)| pane.session.as_ref() == Some(&session))
                        .map(|(id, _)| *id);
                    if let Some(pane) = pane {
                        self.reveal_pane(pane, window, cx);
                    }
                    session.update(cx, |s, cx| {
                        s.ensure_started(window, cx);
                        s.open_agents_panel(AgentsTab::Agents, cx);
                    });
                    cx.notify();
                } else {
                    window.push_notification(
                        Notification::info("Open an agent tab first to view its agents and jobs."),
                        cx,
                    );
                }
            }
            PaletteAction::NewSession => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.new_session(window, cx));
                }
            }
            PaletteAction::Command(name) => {
                if let Some(tab) = tab {
                    tab.update(cx, |s, cx| s.run_prompt(format!("/{name}"), cx));
                }
            }
            PaletteAction::Screen(screen) => self.open_screen(screen, tab, window, cx),
            PaletteAction::SwitchWorkspace(index) => self.switch_workspace(index, window, cx),
            PaletteAction::RenameWorkspace { index, name } => {
                self.rename_workspace(index, name, cx)
            }
            PaletteAction::DeleteWorkspace(index) => {
                self.confirm_delete_workspace(index, window, cx)
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: false,
            } => {
                let root = self.active_cwd(cx);
                self.create_workspace(Some(name), root, window, cx);
            }
            PaletteAction::CreateWorkspace {
                name,
                pick_folder: true,
            } => self.pick_workspace_folder(Some(name), window, cx),
            PaletteAction::OpenFile(path) => self.open_file(path, window, cx),
            PaletteAction::SwitchBranch(branch) => self.confirm_switch_branch(branch, window, cx),
        }
    }

    /// The folder explorer (⌘O / Ctrl+O), from the focused pane's folder.
    pub(super) fn open_explorer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((Overlay::Explorer(_), _)) = &self.overlay {
            self.close_overlay(window, cx);
            return;
        }
        let dir = self.active_cwd(cx);
        let view = cx.new(|cx| ExplorerView::new(dir, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ExplorerEvent, window, cx| {
                this.close_overlay(window, cx);
                match event {
                    ExplorerEvent::OpenAgent(folder) => {
                        this.show_folder(folder.clone(), window, cx)
                    }
                    ExplorerEvent::OpenTerminal(folder) => {
                        this.show_folder(folder.clone(), window, cx);
                        this.open_tab(PaneKind::Terminal, folder.clone(), None, None, window, cx);
                    }
                    ExplorerEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
                    ExplorerEvent::Close => {}
                }
            },
        );
        self.overlay = Some((Overlay::Explorer(view), subscription));
        cx.notify();
    }

    /// Show the workspace rooted at `folder`, creating it (a new workspace
    /// opens with an agent tab). `gsd <folder>` and the explorer both land here.
    pub(super) fn show_folder(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = workspaces::root_key(folder);
        let index = match workspaces::find_root(self.spaces.iter().map(|s| &s.root), &folder) {
            Some(index) => index,
            None => self.push_space(None, folder),
        };
        self.switch_workspace(index, window, cx);
    }

    /// Palette → Install `gsd` command; the result arrives as a notification.
    fn install_gsd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::platform::gsd::install() })
                .await;
            let _ = cx.update(|window, cx| {
                let note = match result {
                    Ok(message) => Notification::success(message),
                    Err(message) => Notification::error(message),
                };
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    /// Play an effort level's surge over the window; another change
    /// restarts it at the new level.
    pub(super) fn play_surge(
        &mut self,
        level: crate::shared::surge::Level,
        cx: &mut Context<Self>,
    ) {
        self.surge_runs += 1;
        let run = self.surge_runs;
        self.surge = Some((run, std::time::Instant::now(), level));
        self._surge_end = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(level.duration()).await;
            let _ = this.update(cx, |this, cx| {
                if this.surge.is_some_and(|(playing, _, _)| playing == run) {
                    this.surge = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// The Agents window of `session` (else the active one), on `tab`.
    fn open_agents_window(
        &mut self,
        tab: AgentsTab,
        session: Option<Entity<SessionView>>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = session.or_else(|| self.active_session()) {
            session.update(cx, |s, cx| s.open_agents_panel(tab, cx));
        }
    }

    /// The native screen of a terminal-only Gentle Shell command; `session`
    /// is the agent it concerns (the one that ran it, else the active one).
    pub(super) fn open_screen(
        &mut self,
        screen: Screen,
        session: Option<Entity<SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match screen {
            Screen::Usage => self.open_usage(window, cx),
            Screen::Stats => {
                self.open_usage(window, cx);
                if let Some((Overlay::Usage(view), _)) = &self.overlay {
                    view.update(cx, |view, cx| view.show_stats(cx));
                }
            }
            Screen::Agents => self.open_agents_window(AgentsTab::Agents, session, cx),
            // Background jobs are the Jobs tab of the Agents window.
            Screen::Jobs => self.open_agents_window(AgentsTab::Jobs, session, cx),
            Screen::Profiles => self.open_profiles(window, cx),
            Screen::Palette => {
                self.close_overlay(window, cx);
                self.open_palette(Category::Commands, window, cx);
            }
            Screen::Changes => {
                let cwd = match &session {
                    Some(session) => session.read(cx).cwd().clone(),
                    None => self.active_cwd(cx),
                };
                self.close_overlay(window, cx);
                self.open_review(cwd, window, cx);
            }
            Screen::Tower => {
                self.close_overlay(window, cx);
                self.open_tower(window, cx);
            }
        }
    }

    /// Gentle Tower in a tab of its own, so it spans the whole window: the
    /// one already open, wherever it is, else a new tab.
    pub(super) fn open_tower(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .panes
            .iter()
            .find(|(_, pane)| pane.item.kind == PaneKind::Tower)
            .map(|(id, _)| *id);
        match open {
            Some(id) => self.reveal_pane(id, window, cx),
            None => {
                let cwd = self.active_cwd(cx);
                self.open_tab(PaneKind::Tower, cwd, None, None, window, cx);
            }
        }
    }

    /// The review screen of the repository `cwd` is in, as a modal over the
    /// window (Esc closes it).
    pub(super) fn open_review(
        &mut self,
        cwd: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| ReviewView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ReviewEvent, window, cx| match event {
                ReviewEvent::Close => this.close_overlay(window, cx),
                // Opened like the Files sidebar opens it, once the review
                // is out of the way.
                ReviewEvent::OpenFile(path) => {
                    this.close_overlay(window, cx);
                    this.open_file(path.clone(), window, cx);
                }
            },
        );
        // Focused, so Esc reaches the workspace's CloseOverlay through it.
        view.focus_handle(cx).focus(window, cx);
        self.overlay = Some((Overlay::Review(view), subscription));
        cx.notify();
    }

    /// The window's active agent tab (`active_session`) is the one the
    /// sessions screen asks to list and switch, when its Gentle Shell offers
    /// it; its backend is started (probe included) first.
    pub(super) fn open_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let session = self.active_session();
        let current = session.as_ref().and_then(|t| t.read(cx).session_file());
        let view = cx.new(|cx| SessionsView::new(cwd, current, session, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &SessionsEvent, window, cx| match event {
                SessionsEvent::Close => this.close_overlay(window, cx),
                SessionsEvent::NewSession => {
                    this.close_overlay(window, cx);
                    if let Some(tab) = this.active_session() {
                        tab.update(cx, |s, cx| s.new_session(window, cx));
                    }
                }
                SessionsEvent::Open { path, cwd } => {
                    this.close_overlay(window, cx);
                    let path = path.clone();
                    if *cwd == this.active_cwd(cx) {
                        if let Some(tab) = this.active_session() {
                            tab.update(cx, |s, cx| s.open_session(path, cx));
                        }
                    } else {
                        this.open_tab(PaneKind::Agent, cwd.clone(), None, Some(path), window, cx);
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Sessions(view), subscription));
        cx.notify();
    }

    pub(super) fn open_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let models = self
            .active_session()
            .map(|t| t.read(cx).model_choices())
            .unwrap_or_default()
            .into_iter()
            .map(|(provider, id, name, reasoning)| ModelChoice {
                provider,
                id,
                name,
                reasoning,
            })
            .collect();
        let session = self.active_session();
        let view = cx.new(|cx| ProfilesView::new(cwd, models, session, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &ProfilesEvent, window, cx| {
            let tab = this.active_session();
            match event {
                ProfilesEvent::Close => this.close_overlay(window, cx),
                ProfilesEvent::Changed => {}
                ProfilesEvent::RunCommand(text) => {
                    if let Some(tab) = tab {
                        let text = text.clone();
                        tab.update(cx, |s, cx| s.run_prompt(text, cx));
                    }
                }
                ProfilesEvent::SwitchOrchestrator { model, thinking } => {
                    if let Some(tab) = tab {
                        let ((provider, id), thinking) = (model.clone(), thinking.clone());
                        tab.update(cx, |s, cx| {
                            s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
                ProfilesEvent::UseInSession { profile, model, thinking } => {
                    if let Some(tab) = tab {
                        let (profile, model, thinking) = (profile.clone(), model.clone(), thinking.clone());
                        let note = format!("Profile \u{201c}{profile}\u{201d} applied to this session's orchestrator; helpers keep the routing this session started with.");
                        tab.update(cx, |s, cx| {
                            s.note(note, cx);
                            s.set_session_profile(Some(profile), cx);
                            if let Some((provider, id)) = model {
                                s.send_command(serde_json::json!({"type": "set_model", "provider": provider, "modelId": id}), cx);
                            }
                            if let Some(level) = thinking {
                                s.send_command(serde_json::json!({"type": "set_thinking_level", "level": level}), cx);
                            }
                        });
                    }
                }
            }
        });
        self.overlay = Some((Overlay::Profiles(view), subscription));
        cx.notify();
    }

    /// Configuration on its Updates page, for the title-bar update pill.
    pub(super) fn open_app_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(window, cx);
        if let Some((Overlay::Settings(view), _)) = &self.overlay {
            view.update(cx, |view, cx| view.show_updates(cx));
        }
    }

    /// Configuration's update status is asked of the window's active agent
    /// tab when its Gentle Shell reports it.
    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_active_session(window, cx);
        let cwd = self.active_cwd(cx);
        let session = self.active_session();
        let tab_layout = self.tab_layout;
        let view = cx.new(|cx| SettingsView::new(cwd, tab_layout, session, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Close => this.close_overlay(window, cx),
                SettingsEvent::TabLayoutChanged(layout) => this.set_tab_layout(*layout, cx),
                SettingsEvent::OpenProfiles => this.open_profiles(window, cx),
                SettingsEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_models(window, cx));
                    }
                }
                SettingsEvent::DevicesChanged(prefs) => {
                    let prefs = *prefs;
                    crate::features::devices::hub(cx)
                        .update(cx, |hub, cx| hub.set_prefs(prefs, cx));
                    pi_rpc::set_system_prompt(crate::features::devices::agent_instructions(prefs));
                    if !prefs.enabled {
                        this.devices.update(cx, |panel, cx| panel.hide(cx));
                    }
                    cx.notify();
                }
                SettingsEvent::MusicChanged => {
                    this.music.update(cx, |music, cx| music.refresh(cx));
                }
                SettingsEvent::BackendUpdated => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.restart_for_update(window, cx));
                    }
                }
                SettingsEvent::Setup(event) => {
                    let usable = view.read(cx).setup_usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
                SettingsEvent::ShellCommand(text) => {
                    let view = view.downgrade();
                    let report = move |result: Result<(), String>, cx: &mut App| {
                        let _ = view.update(cx, |view, cx| view.shell_done(result, cx));
                    };
                    match this.active_session() {
                        Some(session) => session.update(cx, |session, cx| {
                            session.run_extension_command(text.clone(), cx, report)
                        }),
                        None => report(
                            Err(
                                "Open an agent tab first: Gentle Shell applies this setting."
                                    .into(),
                            ),
                            cx,
                        ),
                    }
                }
            },
        );
        self.overlay = Some((Overlay::Settings(view), subscription));
        cx.notify();
    }

    /// Where an open agent tab found the docs skill, if any did.
    pub(super) fn docs_skill_dir(&self, cx: &App) -> Option<PathBuf> {
        self.active_session()
            .into_iter()
            .chain(self.sessions())
            .find_map(|s| s.read(cx).skill_dir(crate::features::docs::SKILL))
    }

    pub(super) fn open_docs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.space().root.clone();
        let skill = self.docs_skill_dir(cx);
        let view = cx.new(|cx| DocsView::new(root.clone(), skill, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &DocsEvent, window, cx| match event {
                DocsEvent::Close => this.close_overlay(window, cx),
                DocsEvent::Run(prompt) => {
                    this.close_overlay(window, cx);
                    this.open_tab(
                        PaneKind::Agent,
                        root.clone(),
                        Some(prompt.clone()),
                        None,
                        window,
                        cx,
                    );
                }
                DocsEvent::Open(path) => {
                    this.close_overlay(window, cx);
                    this.open_file(path.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Docs(view), subscription));
        cx.notify();
    }

    /// The Memory screen for `cwd`'s project, or on `target`'s project and
    /// memory (a session's Engram status).
    pub(super) fn open_memory(
        &mut self,
        cwd: PathBuf,
        target: gentle_core::engram::MemoryTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| MemoryView::new(cwd, target, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &MemoryEvent, window, cx| match event {
                MemoryEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Memory(view), subscription));
        cx.notify();
    }

    pub(super) fn open_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The Subscription tab reads through the active agent's RPC slice, so
        // its backend must be started (probe included) before it is opened.
        self.start_active_session(window, cx);
        let session = self.active_session();
        let view = cx.new(|cx| UsageView::new(window, cx, session));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &UsageEvent, window, cx| match event {
                UsageEvent::Close => this.close_overlay(window, cx),
            },
        );
        self.overlay = Some((Overlay::Usage(view), subscription));
        cx.notify();
    }

    pub(super) fn open_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.active_cwd(cx);
        let view = cx.new(|cx| ProvidersView::new(cwd, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &ProvidersEvent, window, cx| match event {
                ProvidersEvent::Close => {
                    this.close_overlay(window, cx);
                    // Signed in from the welcome screen: back to its sequence.
                    if let Some(view) = this.resume_welcome.take() {
                        this.attach_welcome(view, window, cx);
                    }
                }
                ProvidersEvent::Changed => {
                    for tab in this.sessions() {
                        tab.update(cx, |session, cx| session.reload_credentials(window, cx));
                    }
                }
                ProvidersEvent::Terminal(cwd, command) => {
                    this.close_overlay(window, cx);
                    this.open_setup_terminal(cwd.clone(), command.clone(), window, cx);
                }
            },
        );
        self.overlay = Some((Overlay::Providers(view), subscription));
        cx.notify();
    }

    /// The onboarding screen; `probe` is a look at the machine already taken.
    pub(super) fn open_onboarding(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| OnboardingView::new(probe, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &OnboardingEvent, window, cx| {
                let usable = view.read(cx).usable();
                this.on_setup_event(event, usable, window, cx);
            },
        );
        self.overlay = Some((Overlay::Onboarding(view), subscription));
        cx.notify();
    }

    /// The first-run welcome screen; `probe` is a look at the machine already
    /// taken. ⌘K → "Welcome screen" shows it again.
    pub(super) fn open_welcome(
        &mut self,
        probe: Option<Probe>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resume_welcome = None;
        let view = cx.new(|cx| WelcomeView::new(probe, cx));
        self.attach_welcome(view, window, cx);
    }

    /// Show `view` (new, or back from Providers) and route its events.
    pub(super) fn attach_welcome(
        &mut self,
        view: Entity<WelcomeView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, view, event: &WelcomeEvent, window, cx| match event {
                WelcomeEvent::Close => this.close_overlay(window, cx),
                // Providers takes the window; closing it returns to the welcome.
                WelcomeEvent::Setup(OnboardingEvent::SignIn) => {
                    this.resume_welcome = Some(view.clone());
                    this.close_overlay(window, cx);
                    this.open_providers(window, cx);
                }
                WelcomeEvent::Setup(event) => {
                    let usable = view.read(cx).usable(cx);
                    this.on_setup_event(event, usable, window, cx);
                }
            },
        );
        view.read(cx).focus_handle().clone().focus(window, cx);
        self.overlay = Some((Overlay::Welcome(view), subscription));
        cx.notify();
    }

    /// An action from the setup checklist, on the onboarding screen or in
    /// Configuration (which is closed first, like the screen, when the action
    /// moves elsewhere). `usable`: gentle-shell can start as of its last look.
    pub(super) fn on_setup_event(
        &mut self,
        event: &OnboardingEvent,
        usable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OnboardingEvent::Close => self.close_overlay(window, cx),
            OnboardingEvent::Installed => {
                if usable {
                    self.retry_failed_sessions(window, cx);
                }
            }
            OnboardingEvent::SignIn => {
                self.close_overlay(window, cx);
                self.open_providers(window, cx);
            }
            OnboardingEvent::Terminal(command) => {
                self.close_overlay(window, cx);
                let home = dirs::home_dir().unwrap_or_else(|| self.active_cwd(cx));
                self.open_setup_terminal(home, command.clone(), window, cx);
            }
        }
    }

    /// A terminal tab in `cwd` (where its agents run) with `command` typed at
    /// the prompt for the user to run (it may ask for a password); the
    /// clipboard holds it when the terminal can't take it.
    pub(super) fn open_setup_terminal(
        &mut self,
        cwd: PathBuf,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pane) = self.new_pane(PaneKind::Terminal, cwd, None, None, window, cx) else {
            cx.write_to_clipboard(ClipboardItem::new_string(command));
            window.push_notification(
                Notification::info("The setup command is on your clipboard: run it in a terminal."),
                cx,
            );
            return;
        };
        self.push_pane_tab(pane, window, cx);
        let terminal = self.panes.get(&pane).and_then(|p| {
            p.item
                .view
                .clone()
                .downcast::<crate::features::terminal::TerminalView>()
                .ok()
        });
        let note = match terminal {
            Some(terminal) => {
                terminal.update(cx, |terminal, cx| terminal.type_when_ready(command, cx));
                "The command is typed in this terminal: press Enter to run it. Reopen Setup & requirements from the command palette when it's done."
            }
            None => {
                cx.write_to_clipboard(ClipboardItem::new_string(command));
                "The setup command is on your clipboard: paste it here and press Enter. Reopen Setup & requirements from the command palette when it's done."
            }
        };
        window.push_notification(Notification::info(note), cx);
    }

    /// Start again every agent tab whose backend could not start (onboarding
    /// just made gentle-shell available).
    pub(super) fn retry_failed_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in self.sessions() {
            tab.update(cx, |session, cx| session.retry_start(window, cx));
        }
    }
}
