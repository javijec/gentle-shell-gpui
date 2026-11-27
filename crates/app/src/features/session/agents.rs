//! The Agents panel: Gentle Agents subagents for this session, Tern-style.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use pi_rpc::desktop_rpc::{DesktopAgent, DesktopJob, JobExitCode};

use super::SessionView;
use super::state::desktop;
use super::state::desktop::output::{Failure, JobWatch, ThreadWatch};
use crate::shared::ui;
use gentle_core::activity::{
    AgentTask, Progress, ProjectedProgress, Thread, ThreadItem, helper_progress,
};
use gentle_core::gentle_state::{Job, JobOutcome};
use gentle_core::transcript::format_tokens;

/// How often the job output on screen is read again while the job runs.
const JOB_OUTPUT_POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentsTab {
    #[default]
    Agents,
    Activity,
    Jobs,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentsFilter {
    #[default]
    All,
    Active,
    Finished,
}

impl AgentsFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Active,
            Self::Active => Self::Finished,
            Self::Finished => Self::All,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::All => "Filter: all",
            Self::Active => "Filter: active",
            Self::Finished => "Filter: finished",
        }
    }
    fn keeps(self, helper: &Helper) -> bool {
        match self {
            Self::All => true,
            Self::Active => helper.is_active(),
            Self::Finished => !helper.is_active(),
        }
    }
}

/// One helper shown in the panel: the structured channel projection when the
/// peer offers it, or the legacy `gentle-agents` widget entry otherwise. The
/// structured entry is the truth for identity, status, model, usage and
/// cancel, and from Gentle Shell 5.0.0 for in-run progress too (turns, tool
/// calls, the tool running now). The widget's entry for the same task, when
/// it has one, adds time, label and thread, and the live step and counts
/// only for an older Shell whose projection carries none.
#[derive(Clone, Copy)]
enum Helper<'a> {
    Structured(&'a DesktopAgent, Option<&'a AgentTask>),
    Legacy(&'a AgentTask),
}

impl<'a> Helper<'a> {
    fn id(&self) -> &str {
        match self {
            Self::Structured(agent, _) => &agent.id,
            Self::Legacy(task) => &task.summary.id,
        }
    }

    fn agent(&self) -> &str {
        match self {
            Self::Structured(agent, _) => &agent.agent,
            Self::Legacy(task) => &task.summary.agent,
        }
    }

    fn status(&self) -> &str {
        match self {
            Self::Structured(agent, _) => &agent.status,
            Self::Legacy(task) => &task.summary.status,
        }
    }

    fn is_active(&self) -> bool {
        match self {
            Self::Structured(agent, _) => {
                matches!(agent.status.as_str(), "running" | "queued" | "waiting")
            }
            Self::Legacy(task) => task.summary.is_active(),
        }
    }

    /// The widget's entry for this helper, whichever source leads.
    fn live(&self) -> Option<&'a AgentTask> {
        match *self {
            Self::Structured(_, live) => live,
            Self::Legacy(task) => Some(task),
        }
    }

    fn label(&self) -> Option<&str> {
        self.live()?.summary.label.as_deref()
    }

    /// The model Gentle Shell routed this helper to; the legacy widget does
    /// not carry one.
    fn model(&self) -> Option<&str> {
        match self {
            Self::Structured(agent, _) => agent.model.as_deref(),
            Self::Legacy(_) => None,
        }
    }

    /// Turns, tool calls and the step, from one source
    /// ([`gentle_core::activity::helper_progress`]).
    fn progress(&self) -> Progress {
        match self {
            Self::Structured(agent, live) => helper_progress(
                ProjectedProgress {
                    active: self.is_active(),
                    step: agent.last_step.as_str(),
                    turns: agent.turns,
                    tool_calls: agent.tool_calls,
                    active_tool: agent.active_tool.as_deref(),
                },
                live.map(|task| &task.summary),
            ),
            Self::Legacy(task) => Progress {
                turns: Some(task.summary.turns),
                tool_calls: Some(task.summary.tool_calls),
                step: task.summary.last_step.clone(),
            },
        }
    }

    fn last_step(&self) -> Option<String> {
        self.progress().step
    }

    fn elapsed_ms(&self, now: u64) -> Option<u64> {
        self.live()?.summary.elapsed_ms(now)
    }

    fn turns(&self) -> Option<u64> {
        self.progress().turns
    }

    fn tool_calls(&self) -> Option<u64> {
        self.progress().tool_calls
    }
}

/// Token breakdown lines for one typed subagent, with an honest "unavailable"
/// marker where a breakdown field is not recorded (an in-flight task, or a
/// restored run that never recorded one). Pure so it can be tested without GPUI.
/// Model and cost are shown separately by the detail pane.
pub fn agent_usage_lines(agent: &DesktopAgent) -> Vec<(&'static str, String)> {
    let usage = &agent.usage;
    let token = |value: Option<u64>| value.map_or_else(|| "unavailable".to_string(), format_tokens);
    vec![
        ("Tokens", format_tokens(usage.total)),
        ("Input", token(usage.input)),
        ("Output", token(usage.output)),
        ("Cache read", token(usage.cache)),
        ("Reasoning", token(usage.reasoning)),
    ]
}

/// A billed cost as money; the protocol carries a fractional, non-negative
/// number.
fn format_cost(cost: f64) -> String {
    format!("${cost:.4}")
}

/// A count (turns, tools) neither source carries for this helper.
fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "–".into(), |value| value.to_string())
}

/// Panel state kept on the session.
#[derive(Default)]
pub struct AgentsPanel {
    pub open: bool,
    pub tab: AgentsTab,
    pub filter: AgentsFilter,
    pub selected: Option<String>,
    pub expanded_tools: std::collections::HashSet<(String, usize)>,
}

pub fn status_color(status: &str) -> Hsla {
    match status {
        "running" => ui::warning(),
        "waiting" | "queued" => ui::accent(),
        "done" | "finished" | "completed" | "succeeded" => ui::success(),
        "failed" | "error" | "aborted" | "cancelled" | "stopped" => ui::danger(),
        _ => ui::muted(),
    }
}

fn duration(ms: Option<u64>) -> String {
    ms.map(ui::format_duration).unwrap_or_else(|| "–".into())
}

/// The job output on screen and the poll that keeps it current.
pub struct JobOutputView {
    /// When a job's output well last drew: the poll stops once it has not
    /// drawn for a few ticks (the card closed, the tab is hidden).
    seen: Rc<Cell<Option<Instant>>>,
    _poll: Task<()>,
}

impl Default for JobOutputView {
    fn default() -> Self {
        Self {
            seen: Rc::default(),
            _poll: Task::ready(()),
        }
    }
}

/// What a job row shows of the job's output.
#[derive(Clone)]
pub(super) enum JobTail {
    /// This Shell cannot read job output (4.8.0): the row stays as it was.
    Unavailable,
    /// It can; the row folds open on click.
    Folded,
    /// The output well is open.
    Open {
        /// The last tail and whether earlier output was cut.
        output: Option<(String, bool)>,
        failure: Option<Failure>,
        /// When the well last drew, for the poll.
        seen: Rc<Cell<Option<Instant>>>,
    },
}

/// One background job as the Jobs chip's card and the Jobs tab list it, from
/// the structured channel (`list_jobs` / `jobs_update`) or, on a Shell
/// without it, the legacy `details.gentleJobs` records.
pub(super) struct JobRow {
    id: String,
    label: String,
    state: String,
    outcome: JobOutcome,
}

impl JobRow {
    pub(super) fn is_running(&self) -> bool {
        self.outcome == JobOutcome::Running
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    fn from_legacy(job: &Job) -> Self {
        Self {
            id: job.id.clone(),
            label: job.display_label(),
            state: job.state_text(),
            outcome: job.outcome(),
        }
    }

    fn from_desktop(job: &DesktopJob) -> Self {
        let outcome = if desktop::output::is_running(job) {
            JobOutcome::Running
        } else if matches!(job.exit_code, Some(JobExitCode::Code(0))) && job.status != "failed" {
            JobOutcome::Succeeded
        } else {
            JobOutcome::Failed
        };
        Self {
            id: job.id.clone(),
            label: if job.label.is_empty() {
                format!("job {}", job.id)
            } else {
                job.label.clone()
            },
            state: match &job.exit_code {
                Some(JobExitCode::Code(code)) => format!("exit {code}"),
                Some(JobExitCode::Null) => "no exit code".to_string(),
                None => job.status.clone(),
            },
            outcome,
        }
    }
}

impl SessionView {
    /// This tab's numbers for the resource diagnostics.
    pub fn resources(&self) -> crate::features::diagnostics::TabResources {
        let helpers = self.helpers();
        let jobs = self.job_rows();
        crate::features::diagnostics::TabResources {
            backend: self.process().map(|p| {
                let (records, bytes) = p.traffic();
                (p.pid(), records, bytes)
            }),
            agents: helpers.len(),
            agents_running: helpers.iter().filter(|h| h.is_active()).count(),
            jobs: jobs.len(),
            jobs_running: jobs.iter().filter(|j| j.is_running()).count(),
            items: self.transcript.items.len(),
        }
    }

    /// Every background job, newest data source first: the structured list
    /// once the channel seeded it, the legacy records otherwise.
    pub(super) fn job_rows(&self) -> Vec<JobRow> {
        if self.desktop.jobs_ready() {
            self.desktop
                .jobs()
                .iter()
                .map(JobRow::from_desktop)
                .collect()
        } else {
            self.activity.jobs.iter().map(JobRow::from_legacy).collect()
        }
    }

    /// What job `id`'s row shows of its output.
    pub(super) fn job_tail(&self, id: &str) -> JobTail {
        let output = self.desktop.output();
        if !output.job_supported() || !self.desktop.jobs_ready() {
            return JobTail::Unavailable;
        }
        match output.job().filter(|job| job.id == id) {
            Some(JobWatch {
                output, failure, ..
            }) => JobTail::Open {
                output: output
                    .as_ref()
                    .map(|read| (read.output.clone(), read.truncated)),
                failure: *failure,
                seen: self.job_output.seen.clone(),
            },
            None => JobTail::Folded,
        }
    }

    /// Open job `id`'s output (closing another's), or close it when open.
    /// While it shows and the job runs, it is read again every
    /// [`JOB_OUTPUT_POLL`].
    pub(super) fn toggle_job_output(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.desktop.output().job().is_some_and(|job| job.id == id) {
            self.stop_job_output(cx);
            return;
        }
        self.desktop.output_mut().watch_job(id);
        if self.desktop.output().job().is_none() {
            return;
        }
        self.job_output.seen.set(Some(Instant::now()));
        self.sync_output(cx);
        self.job_output._poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(JOB_OUTPUT_POLL).await;
                crate::shared::diagnostics::tick();
                let keep = this
                    .update(cx, |this, cx| {
                        let shown = this
                            .job_output
                            .seen
                            .get()
                            .is_some_and(|at| at.elapsed() < JOB_OUTPUT_POLL * 3);
                        if !shown {
                            this.desktop.output_mut().unwatch_job();
                            return false;
                        }
                        let running = this.desktop.output_mut().tick_job();
                        this.sync_output(cx);
                        // Draw again, so a well still on screen says so.
                        cx.notify();
                        running
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        cx.notify();
    }

    /// Close the job output on screen and stop its poll.
    pub(super) fn stop_job_output(&mut self, cx: &mut Context<Self>) {
        if self.desktop.output().job().is_some() {
            self.desktop.output_mut().unwatch_job();
            cx.notify();
        }
        self.job_output._poll = Task::ready(());
    }

    /// Read the thread of the helper the Activity tab shows when the
    /// `gentle-agents` widget has none for it (the widget wins when it has
    /// one), and stop reading it otherwise.
    pub(super) fn sync_subagent_thread(&mut self, cx: &mut Context<Self>) {
        let wanted = (self.agents_panel.open && self.agents_panel.tab == AgentsTab::Activity)
            .then(|| self.selected_helper())
            .flatten()
            .and_then(|helper| match helper {
                Helper::Structured(agent, None) => Some(agent.id.clone()),
                _ => None,
            });
        match wanted {
            Some(id) => self.desktop.output_mut().watch_thread(&id),
            None => self.desktop.output_mut().unwatch_thread(),
        }
        self.sync_output(cx);
    }

    /// Hand the generative UI's live panels the data they show: jobs,
    /// subagents (also kanban cards with an `agentId`) and the todo list,
    /// built only while a panel shows them (the panel ignores repeats).
    pub(super) fn sync_genui_live(&mut self, cx: &mut Context<Self>) {
        use crate::features::genui::todo_records;
        use gentle_core::genui::live::Source;
        use serde_json::json;

        if self.genui.wants(Source::Jobs, cx) {
            // The job whose output is on screen also carries its tail.
            let watched = self.desktop.output().job().and_then(|job| {
                job.output
                    .as_ref()
                    .map(|read| (job.id.clone(), read.output.clone()))
            });
            let jobs = self
                .job_rows()
                .iter()
                .map(|row| {
                    let status = match row.outcome {
                        JobOutcome::Running => "running",
                        JobOutcome::Succeeded => "done",
                        JobOutcome::Failed => "failed",
                    };
                    let mut job = json!({"id": row.id, "label": row.label, "status": status, "state": row.state});
                    if let Some((_, output)) = watched.as_ref().filter(|(id, _)| *id == row.id) {
                        job["output"] = output.clone().into();
                    }
                    job
                })
                .collect();
            self.genui
                .feed(Source::Jobs, serde_json::Value::Array(jobs), cx);
        }
        if self.genui.wants(Source::Subagents, cx) {
            let agents = self
                .helpers()
                .iter()
                .map(|helper| {
                    let (model, cost, tokens) = match helper {
                        Helper::Structured(agent, _) => (
                            agent.model.clone(),
                            Some(agent.usage.cost),
                            Some(agent.usage.total),
                        ),
                        Helper::Legacy(_) => (None, None, None),
                    };
                    json!({
                        "id": helper.id(),
                        "agent": helper.agent(),
                        "label": helper.label(),
                        "status": helper.status(),
                        "step": helper.last_step(),
                        "active": helper.is_active(),
                        "model": model,
                        "cost": cost,
                        "tokens": tokens,
                    })
                })
                .collect();
            self.genui
                .feed(Source::Subagents, serde_json::Value::Array(agents), cx);
        }
        if self.genui.wants(Source::Todo, cx) {
            let todo = todo_records(&self.todo.tasks);
            self.genui.feed(Source::Todo, todo, cx);
        }
    }
}

/// One background job: status marker, label, exit code and, while it runs
/// and the peer offers `cancel_job`, a cancel button. When the peer offers
/// `get_job_output`, a click folds its output open below.
pub(super) fn job_row(
    row: &JobRow,
    cancel: bool,
    tail: JobTail,
    view: &WeakEntity<SessionView>,
) -> AnyElement {
    // The running marker is an icon, not a text glyph: `⧗` is missing from
    // the fonts of minimal Linux installs (WSLg) and drew as an empty box.
    let marker = match row.outcome {
        JobOutcome::Running => ui::spinner(Size::XSmall, ui::accent()).into_any_element(),
        JobOutcome::Succeeded => div()
            .text_color(ui::success())
            .child("✓")
            .into_any_element(),
        JobOutcome::Failed => div().text_color(ui::danger()).child("✗").into_any_element(),
    };
    let row_id = row.id.clone();
    let summary = h_flex()
        .id(SharedString::from(format!("job-{}", row.id)))
        .debug_selector(move || format!("job-row-{row_id}"))
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(div().flex_none().child(marker))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(ui::mono())
                .text_size(ui::code_rems(16.))
                .text_color(ui::fg())
                .child(row.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_color(ui::faint())
                .child(row.state.clone()),
        );
    let summary = match &tail {
        JobTail::Unavailable => summary,
        JobTail::Folded | JobTail::Open { .. } => {
            let open = matches!(tail, JobTail::Open { .. });
            let (id, view) = (row.id.clone(), view.clone());
            summary
                .cursor_pointer()
                .child(
                    Icon::new(if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .xsmall()
                    .text_color(ui::faint()),
                )
                .on_click(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| this.toggle_job_output(&id, cx));
                })
        }
    };
    let line = h_flex()
        .gap_2()
        .text_size(rems(12. / 16.))
        .child(summary)
        .when(cancel && row.is_running(), |d| {
            let (id, view) = (row.id.clone(), view.clone());
            d.child(
                Button::new(SharedString::from(format!("job-cancel-{id}")))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::OctagonX))
                    .tooltip("Cancel this job")
                    .on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| this.cancel_running_job(id.clone(), cx));
                    }),
            )
        });
    match tail {
        JobTail::Open {
            output,
            failure,
            seen,
        } => {
            seen.set(Some(Instant::now()));
            v_flex()
                .gap_1()
                .child(line)
                .child(job_output_well(output, failure))
                .into_any_element()
        }
        JobTail::Unavailable | JobTail::Folded => line.into_any_element(),
    }
}

/// A job's output tail: plain text (Gentle Shell strips terminal control
/// sequences), oldest line first, with a marker when earlier output was cut.
fn job_output_well(output: Option<(String, bool)>, failure: Option<Failure>) -> AnyElement {
    let well = v_flex()
        .debug_selector(|| "job-output".into())
        .ml(rems(18. / 16.))
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(ui::well())
        .border_1()
        .border_color(ui::border())
        .overflow_hidden()
        .font_family(ui::mono())
        .text_size(ui::code_rems(12.))
        .text_color(ui::muted());
    let note = |text: &'static str| div().text_color(ui::faint()).child(text);
    match (output, failure) {
        (_, Some(failure)) => well.child(note(match failure {
            Failure::Gone => "This job's output is no longer available.",
            Failure::Unavailable => "Gentle Shell can't read job output right now.",
            Failure::Failed => "The output could not be read.",
        })),
        (None, None) => well.child(note("Reading the output…")),
        (Some((text, _)), None) if text.trim().is_empty() => well.child(note("No output yet.")),
        (Some((text, truncated)), None) => well
            .when(truncated, |d| d.child(note("… earlier output not shown")))
            .children(
                text.lines()
                    .map(|line| div().whitespace_nowrap().child(line.to_string())),
            ),
    }
    .into_any_element()
}

/// What a tab shows while it has nothing to list.
fn empty_state(icon: IconName, text: &'static str) -> AnyElement {
    v_flex()
        .pt_10()
        .items_center()
        .gap_2()
        .child(Icon::new(icon).text_color(ui::faint()))
        .child(
            div()
                .max_w(rems(360. / 16.))
                .text_sm()
                .text_center()
                .text_color(ui::muted())
                .child(text),
        )
        .into_any_element()
}

/// Wall-clock milliseconds, the unit of the activity document's timestamps.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl SessionView {
    pub(super) fn open_agents(&mut self, select: Option<String>, cx: &mut Context<Self>) {
        self.agents_panel.open = true;
        if select.is_some() {
            self.agents_panel.selected = select;
        }
        // Tick running times once a second while the panel is open.
        self._agents_ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                crate::shared::diagnostics::tick();
                let keep = this
                    .update(cx, |this, cx| {
                        let open = this.agents_panel.open;
                        if open
                            && this
                                .activity
                                .agents
                                .as_ref()
                                .is_some_and(|a| a.tasks.iter().any(|t| t.summary.is_active()))
                        {
                            cx.notify();
                        }
                        // The selection or the widget may have moved: read
                        // the thread the Activity tab now needs, if any.
                        if open {
                            this.sync_subagent_thread(cx);
                        }
                        open
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        self.sync_subagent_thread(cx);
        cx.emit(super::SessionEvent::AgentsPanelChanged);
        cx.notify();
    }

    pub(super) fn close_agents(&mut self, cx: &mut Context<Self>) {
        self.agents_panel.open = false;
        self._agents_ticker = Task::ready(());
        self.sync_subagent_thread(cx);
        self.stop_job_output(cx);
        cx.emit(super::SessionEvent::AgentsPanelChanged);
        cx.notify();
    }

    /// The helpers to show: structured first once the channel seeded them, the
    /// legacy `gentle-agents` widget otherwise.
    fn helpers(&self) -> Vec<Helper<'_>> {
        if self.desktop.subagents_ready() {
            let live = |id: &str| {
                self.activity
                    .agents
                    .iter()
                    .flat_map(|activity| activity.tasks.iter())
                    .find(|task| task.summary.id == id)
            };
            self.desktop
                .subagents()
                .iter()
                .map(|agent| Helper::Structured(agent, live(&agent.id)))
                .collect()
        } else {
            self.activity
                .agents
                .iter()
                .flat_map(|activity| activity.tasks.iter())
                .map(Helper::Legacy)
                .collect()
        }
    }

    /// The helper a `subagent_run` call for `agent` started, by its prompt
    /// in the activity widget; else the last helper for that agent.
    pub(super) fn helper_for_run(&self, agent: &str, prompt: Option<&str>) -> Option<String> {
        let helpers = self.helpers();
        self.activity
            .agents
            .as_ref()
            .and_then(|a| gentle_core::activity::task_for_run(&a.tasks, agent, prompt))
            .filter(|id| helpers.iter().any(|helper| helper.id() == *id))
            .or_else(|| {
                helpers
                    .iter()
                    .rev()
                    .find(|helper| helper.agent() == agent)
                    .map(|helper| helper.id())
            })
            .map(str::to_string)
    }

    fn selected_helper(&self) -> Option<Helper<'_>> {
        let helpers = self.helpers();
        let id = self.agents_panel.selected.as_deref();
        helpers
            .iter()
            .copied()
            .find(|helper| Some(helper.id()) == id)
            .or_else(|| helpers.first().copied())
    }

    /// Stop one helper over `cancel_subagent`; the legacy prompt stays only for
    /// a peer that genuinely does not offer the method.
    pub(super) fn stop_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let applied = self.desktop.cancel_subagent(id.to_string());
        match applied.report.as_ref() {
            Some(desktop::supervision::Report::Unsupported) => self.ask_to_stop_agent(id, cx),
            _ => self.apply_supervision(applied, cx),
        }
    }

    /// Gentle Shell has no host-side stop for helpers; ask the agent to cancel it.
    fn ask_to_stop_agent(&mut self, id: &str, cx: &mut Context<Self>) {
        let agent = self
            .desktop
            .subagents()
            .iter()
            .find(|agent| agent.id == id)
            .map(|agent| agent.agent.clone())
            .or_else(|| {
                self.activity.agents.as_ref().and_then(|activity| {
                    activity
                        .tasks
                        .iter()
                        .find(|task| task.summary.id == id)
                        .map(|task| task.summary.agent.clone())
                })
            })
            .unwrap_or_default();
        let text = format!(
            "Stop the subagent task {id} ({agent}) now with subagent_cancel, then tell me what it had done."
        );
        self.send_prompt(text, cx);
    }

    pub(crate) fn render_agents_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.agents_panel.open {
            return None;
        }
        let structured = self.desktop.subagents_ready();
        let helpers = self.helpers();
        let now = now_ms();
        let filter = self.agents_panel.filter;
        let visible: Vec<Helper> = helpers
            .iter()
            .copied()
            .filter(|h| filter.keeps(h))
            .collect();
        let selected = self.selected_helper();
        let selected_id = selected.map(|h| h.id().to_string());
        let tab = self.agents_panel.tab;
        let on_jobs = tab == AgentsTab::Jobs;
        let jobs = self.job_rows();
        let (summary_line, running_now, count) = if on_jobs {
            let running = jobs.iter().filter(|j| j.is_running()).count();
            (
                format!("{running} running · {} finished", jobs.len() - running),
                running > 0,
                jobs.len(),
            )
        } else {
            let running = helpers.iter().filter(|h| h.status() == "running").count();
            let waiting = helpers.iter().filter(|h| h.status() == "waiting").count();
            let queued = helpers.iter().filter(|h| h.status() == "queued").count();
            let finished = helpers.iter().filter(|h| !h.is_active()).count();
            let mut bits = vec![
                format!("{running} running"),
                format!("{waiting} waiting"),
                format!("{queued} queued"),
                format!("{finished} finished"),
            ];
            // Counts from the widget, or from a projection that carries them
            // (Gentle Shell 5.0.0); an older projection alone has none.
            let counted = !structured
                || helpers
                    .iter()
                    .any(|h| matches!(h, Helper::Structured(agent, _) if agent.turns.is_some()));
            if counted {
                let turns: u64 = helpers.iter().filter_map(|h| h.turns()).sum();
                let tools: u64 = helpers.iter().filter_map(|h| h.tool_calls()).sum();
                bits.push(format!("{turns} turns"));
                bits.push(format!("{tools} tools"));
            }
            (bits.join(" · "), running > 0, helpers.len())
        };

        let header = ui::screen_header()
            .child(Icon::new(IconName::Users).small().text_color(ui::muted()))
            .child(
                div()
                    .flex_shrink_0()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child("Agents"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(ui::muted())
                    .child(summary_line),
            )
            .when(running_now, |d| {
                d.child(ui::pulse(
                    div()
                        .flex_shrink_0()
                        .size(rems(6. / 16.))
                        .rounded_full()
                        .bg(ui::warning()),
                ))
            })
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(ui::code_rems(12.))
                    .font_family(ui::mono())
                    .text_color(ui::faint())
                    .child(count.to_string()),
            )
            .child(
                Button::new("agents-close")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(IconName::X))
                    .on_click(cx.listener(|this, _, _, cx| this.close_agents(cx))),
            );

        let tab_button = |id: &'static str, label: &'static str, tab: AgentsTab| {
            let active = self.agents_panel.tab == tab;
            div()
                .id(id)
                .px_3()
                .py_1()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(active, |d| d.bg(ui::raised()).text_color(ui::fg()))
                .when(!active, |d| {
                    d.text_color(ui::muted()).hover(|d| d.text_color(ui::fg()))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.agents_panel.tab = tab;
                    this.sync_subagent_thread(cx);
                    cx.notify();
                }))
                .child(label)
        };
        let tabs = h_flex()
            .self_start()
            .p(px(2.))
            .gap(px(2.))
            .rounded_lg()
            .bg(ui::well())
            .border_1()
            .border_color(ui::border())
            .child(tab_button("agents-tab", "Agents", AgentsTab::Agents))
            .child(tab_button("activity-tab", "Activity", AgentsTab::Activity))
            .child(tab_button("jobs-tab", "Jobs", AgentsTab::Jobs));

        let no_subagents = || {
            empty_state(
                IconName::Bot,
                "No subagents yet. When el Gentleman delegates work, helpers appear here live.",
            )
        };
        let left_body = match tab {
            AgentsTab::Agents if helpers.is_empty() => no_subagents(),
            AgentsTab::Agents => {
                self.render_agents_table(&visible, selected_id.as_deref(), now, cx)
            }
            // The thread is the `gentle-agents` widget's entry for the same
            // task id when it has one; otherwise `get_subagent_thread` reads
            // it, where the peer offers that.
            AgentsTab::Activity => match selected {
                Some(Helper::Legacy(task) | Helper::Structured(_, Some(task))) => {
                    self.render_thread(&task.summary.id, &task.thread, false, cx)
                }
                Some(Helper::Structured(agent, None)) => {
                    match self.desktop.output().thread().filter(|t| t.id == agent.id) {
                        Some(ThreadWatch {
                            thread: Some(thread),
                            ..
                        }) if thread.items.is_empty() => {
                            empty_state(IconName::MessageSquare, "This helper has no activity yet.")
                        }
                        Some(ThreadWatch {
                            thread: Some(thread),
                            truncated,
                            ..
                        }) => self.render_thread(&agent.id, thread, *truncated, cx),
                        Some(ThreadWatch {
                            failure: Some(failure),
                            ..
                        }) => empty_state(
                            IconName::MessageSquare,
                            match failure {
                                Failure::Gone => {
                                    "Gentle Shell no longer keeps this helper's activity."
                                }
                                Failure::Unavailable => {
                                    "Gentle Shell can't read helper activity right now."
                                }
                                Failure::Failed => "This helper's activity could not be read.",
                            },
                        ),
                        Some(_) => {
                            empty_state(IconName::MessageSquare, "Reading this helper's activity…")
                        }
                        None => empty_state(
                            IconName::MessageSquare,
                            "Gentle Shell has not published an activity thread for this helper.",
                        ),
                    }
                }
                None => no_subagents(),
            },
            AgentsTab::Jobs if jobs.is_empty() => empty_state(
                IconName::Activity,
                "No background jobs yet. Commands the agent runs in the background show up here.",
            ),
            AgentsTab::Jobs => {
                let (cancel, view) = (self.desktop.cancel_job_supported(), cx.entity().downgrade());
                v_flex()
                    .gap_0p5()
                    .children(jobs.iter().map(|job| {
                        let tail = self.job_tail(job.id());
                        div()
                            .px_3()
                            .py_1p5()
                            .child(job_row(job, cancel, tail, &view))
                    }))
                    .into_any_element()
            }
        };

        let left = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            .gap_3()
            .child(tabs)
            .child(
                div()
                    .id("agents-left")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(left_body),
            );

        let right = v_flex()
            .id("agents-detail")
            .w(relative(0.36))
            .min_w(rems(240. / 16.))
            .max_w(rems(420. / 16.))
            .flex_shrink_0()
            .h_full()
            .p_5()
            .gap_3()
            .border_l_1()
            .border_color(ui::border())
            .overflow_y_scroll()
            .children(selected.map(|helper| self.render_agent_detail(&helper, now)));

        let footer = h_flex()
            .px_4()
            .py_3()
            .gap_2()
            .border_t_1()
            .border_color(ui::border())
            .when_some(
                selected
                    .filter(|h| !on_jobs && h.is_active())
                    .map(|h| h.id().to_string()),
                |d, id| {
                    let rpc = selected.is_some_and(|h| {
                        matches!(h, Helper::Structured(..))
                            && self.desktop.cancel_subagent_supported()
                    });
                    if rpc {
                        d.child(
                            Button::new("agents-stop")
                                .danger()
                                .outline()
                                .small()
                                .icon(Icon::new(IconName::OctagonX))
                                .label("Stop")
                                .tooltip("Ask Gentle Shell to cancel this helper over the Desktop channel")
                                .on_click(cx.listener(move |this, _, _, cx| this.stop_agent(&id, cx))),
                        )
                    } else {
                        d.child(
                            Button::new("agents-stop")
                                .danger()
                                .outline()
                                .small()
                                .icon(Icon::new(IconName::OctagonX))
                                .label("Ask to stop")
                                .tooltip("Gentle Shell stops helpers from its agent: this asks it to run subagent_cancel")
                                .on_click(cx.listener(move |this, _, _, cx| this.ask_to_stop_agent(&id, cx))),
                        )
                    }
                },
            )
            .when(!on_jobs, |d| {
                d.child(
                    Button::new("agents-filter")
                        .ghost()
                        .small()
                        .icon(Icon::new(IconName::Funnel))
                        .label(filter.label())
                        .selected(filter != AgentsFilter::All)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agents_panel.filter = this.agents_panel.filter.next();
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("agents-footer-close")
                    .ghost()
                    .small()
                    .label("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close_agents(cx))),
            )
            .when(!on_jobs, |d| {
                d.child(
                    Button::new("agents-transcript")
                        .primary()
                        .small()
                        .icon(Icon::new(if tab == AgentsTab::Activity {
                            IconName::ArrowLeft
                        } else {
                            IconName::MessageSquare
                        }))
                        .label(if tab == AgentsTab::Activity { "Back to agents" } else { "Open transcript" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.agents_panel.tab = match this.agents_panel.tab {
                                AgentsTab::Agents => AgentsTab::Activity,
                                AgentsTab::Activity | AgentsTab::Jobs => AgentsTab::Agents,
                            };
                            this.sync_subagent_thread(cx);
                            cx.notify();
                        })),
                )
            });

        let (backdrop, panel) = ui::workspace_screen(None);
        Some(
            backdrop
                .id("agents-overlay")
                .occlude()
                .child(ui::pop_in(
                    "agents-screen",
                    panel
                        .child(header)
                        .child(
                            h_flex()
                                .flex_1()
                                .min_h_0()
                                .child(left)
                                // A job has no detail pane: the list takes the width.
                                .when(!on_jobs, |d| d.child(right)),
                        )
                        .child(footer),
                ))
                .into_any_element(),
        )
    }

    fn render_agents_table(
        &self,
        helpers: &[Helper],
        selected: Option<&str>,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let column = |w: f32, text: String| {
            div()
                .w(rems(w / 16.))
                .flex_shrink_0()
                .text_right()
                .child(text)
        };
        let head = h_flex()
            .px_3()
            .pb_1()
            .gap_3()
            .text_xs()
            .text_color(ui::faint())
            .child(div().w(rems(10. / 16.)).flex_shrink_0())
            .child(div().flex_1().min_w_0().truncate().child("Agent"))
            .child(column(72., "Status".into()))
            .child(column(56., "Time".into()))
            .child(column(40., "Turns".into()))
            .child(column(40., "Tools".into()));
        let rows = helpers.iter().enumerate().map(|(ix, helper)| {
            let id = helper.id().to_string();
            let is_selected = selected == Some(helper.id());
            let color = status_color(helper.status());
            let (name, kind) = match helper.label() {
                Some(label) if !label.is_empty() => {
                    (label.to_string(), Some(helper.agent().to_string()))
                }
                _ => (helper.agent().to_string(), None),
            };
            let running = helper.status() == "running";
            let row = h_flex()
                .id(SharedString::from(format!("agent-row-{}", helper.id())))
                .px_3()
                .py_2()
                .gap_3()
                .rounded_lg()
                .cursor_pointer()
                .when(is_selected, |d| d.bg(ui::raised()))
                .when(!is_selected, |d| d.hover(|d| d.bg(ui::surface_hover())))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.agents_panel.selected = Some(id.clone());
                    if event.click_count() >= 2 {
                        this.agents_panel.tab = AgentsTab::Activity;
                    }
                    this.sync_subagent_thread(cx);
                    cx.notify();
                }))
                .child(
                    div()
                        .w(rems(10. / 16.))
                        .flex_shrink_0()
                        .flex()
                        .justify_center()
                        .child(if running {
                            ui::pulse(div().size(rems(7. / 16.)).rounded_full().bg(color))
                                .into_any_element()
                        } else {
                            div()
                                .size(rems(7. / 16.))
                                .rounded_full()
                                .bg(color)
                                .into_any_element()
                        }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .gap_0p5()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(ui::code_rems(14.))
                                .font_family(ui::mono())
                                .text_color(ui::fg())
                                .child(name),
                        )
                        .child(
                            h_flex()
                                .min_w_0()
                                .overflow_hidden()
                                .gap_1()
                                .when_some(kind, |d, kind| d.child(kind_chip(kind)))
                                .when_some(helper.model().map(str::to_string), |d, model| {
                                    d.child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(ui::code_rems(12.))
                                            .font_family(ui::mono())
                                            .text_color(ui::info())
                                            .child(model),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child(helper.last_step().unwrap_or_default()),
                        ),
                )
                .child(
                    column(72., helper.status().to_string())
                        .truncate()
                        .text_xs()
                        .text_color(color),
                )
                .child(
                    column(56., duration(helper.elapsed_ms(now)))
                        .text_size(ui::code_rems(12.))
                        .font_family(ui::mono())
                        .text_color(ui::muted()),
                )
                .child(
                    column(40., count(helper.turns()))
                        .text_size(ui::code_rems(12.))
                        .font_family(ui::mono())
                        .text_color(ui::muted()),
                )
                .child(
                    column(40., count(helper.tool_calls()))
                        .text_size(ui::code_rems(12.))
                        .font_family(ui::mono())
                        .text_color(ui::muted()),
                );
            ui::appear(("agent-row-in", ix), div().child(row))
        });
        v_flex()
            .gap_0p5()
            .child(head)
            .children(rows)
            .into_any_element()
    }

    fn render_agent_detail(&self, helper: &Helper, now: u64) -> AnyElement {
        let field = |label: &'static str, value: String, mono: bool| {
            v_flex().gap_1().child(ui::section_label(label)).child(
                div()
                    .text_sm()
                    .text_color(ui::fg())
                    .when(mono, |d| {
                        d.font_family(ui::mono()).text_size(ui::code_rems(12.))
                    })
                    .child(value),
            )
        };
        let stat = |label: &'static str, value: String| {
            ui::card()
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .gap_0p5()
                .child(ui::section_label(label))
                .child(
                    div()
                        .truncate()
                        .text_size(ui::code_rems(14.))
                        .font_family(ui::mono())
                        .text_color(ui::fg())
                        .child(value),
                )
        };
        match helper {
            Helper::Structured(agent, _) => {
                let color = status_color(&agent.status);
                v_flex()
                    .gap_4()
                    .child(
                        v_flex().gap_1().child(
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(ui::fg())
                                        .child(agent.agent.clone()),
                                )
                                .child(ui::chip(agent.status.clone(), color).flex_shrink_0()),
                        ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(stat(
                                "Model",
                                agent.model.clone().unwrap_or_else(|| "unavailable".into()),
                            ))
                            .child(stat("Cost", format_cost(agent.usage.cost))),
                    )
                    .when(helper.live().is_some() || agent.turns.is_some(), |d| {
                        d.child(
                            h_flex()
                                .gap_2()
                                .child(stat("Time", duration(helper.elapsed_ms(now))))
                                .child(stat("Turns", count(helper.turns())))
                                .child(stat("Tools", count(helper.tool_calls()))),
                        )
                    })
                    .child(
                        v_flex().gap_1().child(ui::section_label("Usage")).children(
                            agent_usage_lines(agent)
                                .into_iter()
                                .map(|(label, value)| field(label, value, true)),
                        ),
                    )
                    .when_some(agent.thinking.clone(), |d, thinking| {
                        d.child(field("Thinking", thinking, false))
                    })
                    .child(field(
                        "Last step",
                        helper.last_step().unwrap_or_default(),
                        true,
                    ))
                    .child(field("Id", agent.id.clone(), true))
            }
            Helper::Legacy(task) => {
                let s = &task.summary;
                let color = status_color(&s.status);
                v_flex()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_lg()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(ui::fg())
                                            .child(s.agent.clone()),
                                    )
                                    .child(ui::chip(s.status.clone(), color).flex_shrink_0()),
                            )
                            .when_some(s.label.clone(), |d, label| {
                                d.child(div().text_xs().text_color(ui::muted()).child(label))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(stat("Time", duration(s.elapsed_ms(now))))
                            .child(stat("Turns", s.turns.to_string()))
                            .child(stat("Tools", s.tool_calls.to_string())),
                    )
                    .when_some(s.error.clone(), |d, error| {
                        d.child(
                            v_flex()
                                .gap_1()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .bg(ui::danger().opacity(0.08))
                                .border_1()
                                .border_color(ui::danger().opacity(0.22))
                                .child(
                                    div()
                                        .text_size(rems(10. / 16.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(ui::danger())
                                        .child("ERROR"),
                                )
                                .child(
                                    div()
                                        .text_size(ui::code_rems(12.))
                                        .font_family(ui::mono())
                                        .text_color(ui::danger())
                                        .child(error),
                                ),
                        )
                    })
                    .when_some(s.last_step.clone(), |d, step| {
                        d.child(field("Last step", step, true))
                    })
                    .when_some(s.prompt.clone(), |d, prompt| {
                        d.child(
                            v_flex().gap_1().child(ui::section_label("Task")).child(
                                ui::well_box()
                                    .px_3()
                                    .py_2()
                                    .text_color(ui::fg())
                                    .whitespace_normal()
                                    .child(prompt),
                            ),
                        )
                    })
                    .child(field("Id", s.id.clone(), true))
            }
        }
        .into_any_element()
    }

    /// A helper's thread, from the widget or `get_subagent_thread`.
    /// `truncated`: the store holds older items than the window carries.
    fn render_thread(
        &self,
        id: &str,
        thread: &Thread,
        truncated: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = id.to_string();
        let mut column = v_flex()
            .debug_selector(|| "agent-thread".into())
            .gap_2()
            .px_1();
        if thread.dropped > 0 || truncated {
            let text = if thread.dropped > 0 {
                format!("… {} earlier items dropped", thread.dropped)
            } else {
                "… earlier items not shown".to_string()
            };
            column = column.child(div().text_xs().text_color(ui::faint()).child(text));
        }
        for (ix, item) in thread.items.iter().enumerate() {
            column =
                column.child(match item {
                    ThreadItem::Text { text } => div()
                        .text_sm()
                        .text_color(ui::fg())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Thinking { text } => div()
                        .text_xs()
                        .italic()
                        .text_color(ui::faint())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Note { text } => div()
                        .text_xs()
                        .text_color(ui::muted())
                        .child(text.clone())
                        .into_any_element(),
                    ThreadItem::Tool {
                        name,
                        args,
                        running,
                        is_error,
                        output,
                    } => {
                        let key = (id.clone(), ix);
                        let open = self.agents_panel.expanded_tools.contains(&key);
                        let status: AnyElement = if *running {
                            ui::spinner(Size::XSmall, ui::accent()).into_any_element()
                        } else if *is_error {
                            Icon::new(IconName::X)
                                .xsmall()
                                .text_color(ui::danger())
                                .into_any_element()
                        } else {
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(ui::success())
                                .into_any_element()
                        };
                        let body = output.clone().unwrap_or_default();
                        let (shown, hidden) = ui::tail(&body, if open { 40 } else { 3 });
                        ui::card()
                            .overflow_hidden()
                            .child(
                                h_flex()
                                    .id(SharedString::from(format!("thread-{id}-{ix}")))
                                    .px_3()
                                    .py_1p5()
                                    .gap_2()
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(ui::surface_hover()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.agents_panel.expanded_tools.remove(&key) {
                                            this.agents_panel.expanded_tools.insert(key.clone());
                                        }
                                        cx.notify();
                                    }))
                                    .child(
                                        Icon::new(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .xsmall()
                                        .text_color(ui::faint()),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(ui::champagne())
                                            .child(name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .font_family(ui::mono())
                                            .text_size(ui::code_rems(12.))
                                            .text_color(ui::muted())
                                            .child(ThreadItem::tool_subject(args)),
                                    )
                                    .child(status),
                            )
                            .when(!shown.trim().is_empty(), |d| {
                                d.child(
                                    v_flex()
                                        .px_3()
                                        .py_1p5()
                                        .border_t_1()
                                        .border_color(ui::border())
                                        .bg(ui::well())
                                        .overflow_hidden()
                                        .font_family(ui::mono())
                                        .text_size(ui::code_rems(12.))
                                        .text_color(ui::muted())
                                        .when(hidden > 0, |d| {
                                            d.child(
                                                div()
                                                    .text_color(ui::faint())
                                                    .child(format!("… {hidden} more lines")),
                                            )
                                        })
                                        .children(shown.lines().map(|l| {
                                            div().whitespace_nowrap().child(l.to_string())
                                        })),
                                )
                            })
                            .into_any_element()
                    }
                    ThreadItem::Other => div().into_any_element(),
                });
        }
        column.into_any_element()
    }
}

/// Tern's model pill: outlined, rounded, with a small leading dot.
fn kind_chip(text: String) -> Div {
    h_flex()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .gap_1()
        .px_2()
        .h(rems(18. / 16.))
        .rounded_full()
        .border_1()
        .border_color(ui::border_strong())
        .bg(ui::well())
        .text_size(rems(10.5 / 16.))
        .text_color(ui::muted())
        .child(
            div()
                .flex_shrink_0()
                .size(rems(4. / 16.))
                .rounded_full()
                .bg(ui::faint()),
        )
        .child(text)
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{DesktopAgent, DesktopAgentStep, DesktopAgentUsage};

    use super::{agent_usage_lines, format_cost};

    fn agent(model: Option<&str>, usage: DesktopAgentUsage) -> DesktopAgent {
        DesktopAgent {
            id: "a1".into(),
            agent: "worker".into(),
            status: "running".into(),
            model: model.map(str::to_string),
            model_source: None,
            fallback_from: None,
            thinking: None,
            last_step: DesktopAgentStep::Writing,
            turns: None,
            tool_calls: None,
            active_tool: None,
            usage,
        }
    }

    #[test]
    fn usage_lines_report_tokens_and_honest_unavailable_breakdown() {
        let full = agent(
            Some("anthropic/claude-sonnet-4"),
            DesktopAgentUsage {
                total: 15234,
                cost: 0.0421,
                input: Some(12000),
                output: Some(3234),
                cache: Some(5000),
                reasoning: Some(4000),
            },
        );
        let lines = agent_usage_lines(&full);
        assert_eq!(lines[0], ("Tokens", "15.2k".to_string()));
        assert_eq!(lines[1], ("Input", "12k".to_string()));
        assert_eq!(lines[2], ("Output", "3.2k".to_string()));
        assert_eq!(lines[3], ("Cache read", "5k".to_string()));
        assert_eq!(lines[4], ("Reasoning", "4k".to_string()));

        // Breakdown absent: honest "unavailable", never invented zeroes.
        let partial = agent(
            None,
            DesktopAgentUsage {
                total: 0,
                cost: 0.0,
                input: None,
                output: None,
                cache: None,
                reasoning: None,
            },
        );
        let lines = agent_usage_lines(&partial);
        assert_eq!(lines[0], ("Tokens", "0".to_string()));
        assert_eq!(lines[1].1, "unavailable");
        assert_eq!(lines[2].1, "unavailable");
        assert_eq!(lines[3].1, "unavailable");
        assert_eq!(lines[4].1, "unavailable");
    }

    #[test]
    fn cost_is_money() {
        assert_eq!(format_cost(0.0421), "$0.0421");
        assert_eq!(format_cost(0.0), "$0.0000");
        assert_eq!(format_cost(1.5), "$1.5000");
    }
}
