//! Configuration: everything the user's Gentle Shell is already set up with,
//! read from the same homes the launcher uses, plus package management
//! (`gentle-shell install|remove`) and the Claude bridge's plan.

use std::path::PathBuf;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use crate::{features::app_update, features::workspaces::LayoutProfiles};

mod agent_ui;
mod app;
mod background;
mod browser;
mod claude;
mod devices;
mod diagnostics;
mod events;
mod fonts;
mod hydration;
mod input_style;
mod interface;
mod layout;
mod maestro;
mod models;
mod music;
mod navigation;
mod notifications;
mod packages;
mod pages;
mod reading_desk;
mod runs_on;
mod services;
mod shell_config;
mod shell_status;
mod sign_ins;
mod snapshot;
mod theme;
mod tower;
mod upgrade;
mod view;

pub use claude::forget_claude_auth;
pub use events::SettingsEvent;
pub use snapshot::npm_name;

use navigation::SettingsPage;
use snapshot::Snapshot;

pub struct SettingsView {
    cwd: PathBuf,
    /// The two independently editable presentation presets.
    layout_profiles: LayoutProfiles,
    page: SettingsPage,
    body_scroll: ScrollHandle,
    settings_search: Entity<InputState>,
    _settings_search: Subscription,
    compact_categories_open: bool,
    snapshot: Option<Snapshot>,
    /// Package input, progress, output and event subscription.
    package: packages::PackageUiState,
    _load: Task<()>,
    /// Shared task for package, Gentle Shell and Pi commands.
    _job: Task<()>,
    /// The Appearance sliders: transparency, blur, opacity.
    glass: theme::GlassSliders,
    /// Pending background-helper policy command and its feedback.
    background: background::BackgroundUiState,
    /// Gentle Shell and Pi update progress and feedback.
    shell_update: upgrade::ShellUpdateUiState,
    /// Re-render as this app's own update moves along.
    _app_update: Subscription,
    /// Notification configuration write feedback.
    notifications: notifications::NotificationUiState,
    /// Setup & requirements: the onboarding checklist, embedded.
    setup: Entity<crate::features::onboarding::OnboardingView>,
    _setup: Subscription,
    /// UI testing (Maestro): what was found, the job running and its log.
    maestro: maestro::Maestro,
    /// Music (gentle-stream): what was found, the job running and its log.
    music: music::Music,
    /// Gentle Tower: whether its web app is there, building it.
    tower: tower::Tower,
    /// Devices: the switches, what this machine has, an install running.
    devices: devices::Devices,
    /// Browser: the browsers found on this machine.
    browsers: browser::Browsers,
    /// Engram's own update.
    engram: upgrade::EngramUpdate,
    /// Theme: importing and sharing themes.
    theme_share: theme::ThemeShare,
    /// Fonts: the open picker, its search and cursor.
    fonts: fonts::FontsUi,
    /// Active agent tab, if available, for the new Desktop RPC update status.
    session: Option<Entity<crate::features::session::SessionView>>,
    _session_sub: Option<Subscription>,
    /// RPC status and its in-flight ticket; separate from upgrade UI state.
    shell_updates: upgrade::ShellUpdates,
    /// Gentle Shell's settings stores (persona, banner, notifications) as
    /// the active agent tab's Shell reports them, when it serves them.
    shell_config: shell_config::ShellConfig,
    /// Gentle Shell's own status (`get_status`), when its Shell offers it.
    shell_status: shell_status::ShellStatusRead,
    /// Which providers have credentials, as the active agent tab's Shell's
    /// model registry reports them, when it serves `get_auth_status`.
    sign_ins: sign_ins::SignIns,
    /// Where agents run: this computer, a WSL distro or a command.
    runs_on: runs_on::RunsOn,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(
        cwd: PathBuf,
        layout_profiles: LayoutProfiles,
        session: Option<Entity<crate::features::session::SessionView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let package = packages::PackageUiState::new(window, cx);
        let settings_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting…"));
        let settings_search_subscription = cx.subscribe(
            &settings_search,
            |this, input, event: &InputEvent, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => {
                    let query = input.read(cx).value().to_string();
                    if !query.trim().is_empty()
                        && let Some(page) = SettingsPage::ALL
                            .into_iter()
                            .find(|page| page.matches_query(&query))
                    {
                        this.navigate_to(page, cx);
                    }
                }
                _ => {}
            },
        );
        let setup = cx.new(crate::features::onboarding::OnboardingView::embedded);
        let setup_sub = cx.subscribe(&setup, |_, _, event, cx| {
            cx.emit(SettingsEvent::Setup(event.clone()));
        });
        let mut this = Self {
            cwd,
            layout_profiles,
            page: SettingsPage::default(),
            body_scroll: ScrollHandle::new(),
            settings_search,
            _settings_search: settings_search_subscription,
            compact_categories_open: false,
            snapshot: None,
            package,
            _load: Task::ready(()),
            _job: Task::ready(()),
            glass: theme::GlassSliders::new(cx),
            background: background::BackgroundUiState::default(),
            shell_update: upgrade::ShellUpdateUiState::default(),
            _app_update: cx.observe_global::<app_update::AppUpdate>(|_, cx| cx.notify()),
            notifications: notifications::NotificationUiState::default(),
            setup,
            _setup: setup_sub,
            maestro: maestro::Maestro::default(),
            music: music::Music::default(),
            tower: tower::Tower::default(),
            devices: devices::Devices::default(),
            browsers: browser::Browsers::default(),
            engram: upgrade::EngramUpdate::default(),
            theme_share: theme::ThemeShare::new(window, cx),
            fonts: fonts::FontsUi::new(window, cx),
            _session_sub: session
                .as_ref()
                .map(|session| cx.observe(session, |this, _, cx| this.session_changed(cx))),
            session,
            shell_updates: upgrade::ShellUpdates::default(),
            shell_config: shell_config::ShellConfig::default(),
            shell_status: shell_status::ShellStatusRead::default(),
            sign_ins: sign_ins::SignIns::default(),
            runs_on: runs_on::RunsOn::new(window, cx),
        };
        this.reload(cx);
        this.refresh_maestro(cx);
        this.refresh_music(cx);
        this.refresh_tower(cx);
        this.refresh_devices(cx);
        this.refresh_browsers(cx);
        this.refresh_runs_on(cx);
        this
    }

    /// gentle-shell can start, as of the Setup & requirements checklist.
    pub fn setup_usable(&self, cx: &App) -> bool {
        self.setup.read(cx).usable()
    }

    /// Open on the Updates page, where this app's update is installed.
    pub fn show_updates(&mut self, cx: &mut Context<Self>) {
        self.navigate_to(SettingsPage::Updates, cx);
    }

    /// Reuse a single navigation path for the sidebar, compact picker and search.
    fn navigate_to(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.compact_categories_open = false;
        if self.page != page {
            self.page = page;
            self.body_scroll.scroll_to_item(0);
        }
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.reload_with(false, cx);
    }

    /// The active agent tab changed: its probe answered, or a Desktop answer
    /// or event arrived.
    fn session_changed(&mut self, cx: &mut Context<Self>) {
        self.update_status_changed(cx);
        self.shell_status_changed(cx);
        self.shell_config_changed(cx);
        self.background_changed(cx);
        self.sign_ins_changed(cx);
    }

    /// Show what the active tab's Gentle Shell reported over what the files
    /// said; whatever it does not serve keeps the files' read.
    fn adopt_shell(&mut self, cx: &mut Context<Self>) {
        if let Some(snapshot) = self.snapshot.as_mut() {
            if let Some(state) = self
                .shell_config
                .state(pi_rpc::desktop_rpc::ConfigStore::Persona)
            {
                snapshot.persona = shell_config::persona_of(state);
            }
            if let Some(state) = self
                .shell_config
                .state(pi_rpc::desktop_rpc::ConfigStore::Banner)
            {
                snapshot.banner = state.value.as_ref().map(snapshot::describe_banner);
            }
            if let Some(state) = &self.background.shell {
                let project_file = snapshot.background_subagents.project_file.clone();
                snapshot.background_subagents =
                    background::BackgroundPolicy::from_shell(state, project_file);
            }
            if let Some(status) = &self.sign_ins.status {
                snapshot.sign_ins = sign_ins::sign_ins_of(status);
            }
        }
        self.adopt_sign_ins(cx);
        cx.notify();
    }

    /// Prefer update information from the active agent's Desktop RPC.
    /// Fall back to the existing launcher/registry path when unavailable.
    /// The settings stores, the background-subagents policy and the
    /// sign-ins are asked of it too; the files fill in whatever it does not
    /// serve.
    fn reload_with(&mut self, refresh_updates: bool, cx: &mut Context<Self>) {
        self.ask_shell_status(cx);
        let shell_updates = self.ask_update_status(refresh_updates, cx);
        if !shell_updates {
            self.shell_updates.status = None;
        }
        self.ask_shell_config(cx);
        self.ask_sign_ins(cx);
        self.ask_background(cx);
        let cwd = self.cwd.clone();
        self._load = cx.spawn(async move |this, cx| {
            let snapshot = cx
                .background_executor()
                .spawn(async move { services::snapshot::load(&cwd, shell_updates) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.snapshot = Some(snapshot);
                this.adopt_shell(cx);
            });
        });
    }
}
