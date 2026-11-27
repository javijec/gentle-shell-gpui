//! Rendering for [`SessionView`]: the composition of the pane and the
//! conversation's items (tool cards, thinking, messages, notices). The
//! composer, its pickers, dialogs and overlays live in the child modules.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::attachments;
use super::state::BackendStatus;
use super::{CONTEXT, SessionView};
use crate::shared::overlay::LiveSlots;
use crate::shared::ui;
use gentle_core::input_style::InputStyle;
use gentle_core::tools;
use gentle_core::transcript::steps::{self, StepKind};
use gentle_core::transcript::{Block, Item, NoticeLevel, ToolStatus, assistant_visible};

mod activity;
mod cards;
mod composer;
mod dialogs;
mod effort;
mod live;
mod model_picker;
mod paths;
mod permissions;
mod plan_limits;
mod rail;
mod review;
mod slash_menu;
mod todo_island;
mod transcript_list;

pub(super) use activity::parse_rate_limit;
pub(super) use cards::markdown_custom;
pub(super) use live::{Live, RateLimit};
use live::{Span, TICK, label_stamp, ticker_needed, track};
pub(super) use paths::PathLookup;
pub(super) use transcript_list::TranscriptList;
use transcript_list::{GroupRow, HeaderKey, Prepared, Row, Tail};

/// How long a finished answer's petal line takes to draw back and fade.
const ANSWER_FADE: Duration = Duration::from_millis(500);

// Keys into `SessionView::expanded`: one per foldable region.
const OUTPUT: usize = 0;
const INPUT: usize = 1;
const THINKING: usize = 2;
/// The whole step: a finished tool (or helper) row folds to its quiet line.
const STEP: usize = 3;

fn fold_key(ix: usize, sub: usize, slot: usize) -> usize {
    (ix << 16) | (sub << 2) | slot
}

/// The fold key of the run of finished steps that starts at item `start`
/// (sub 1 keeps it apart from the step's own fold).
fn group_key(start: usize) -> usize {
    fold_key(start, 1, STEP)
}

/// Lines of a collapsed tool input and output.
const INPUT_LINES: usize = 6;
const DIFF_LINES: usize = 12;
const OUTPUT_LINES: usize = 8;
/// Lines a long message you sent shows folded. It folds only when that
/// hides more than `USER_SLACK` lines: hiding fewer would save less room
/// than its toggle takes.
const USER_LINES: usize = 8;
const USER_SLACK: usize = 2;
/// Pane widths below which the composer sheds secondary chrome.
const NARROW: f32 = 520.;
const TINY: f32 = 420.;

/// Bring the Markdown rendered for `key` up to `shown` (the text as the
/// conversation shows it, with `drawn` Mermaid diagrams as images),
/// appending when it only grew (streaming). Blocks not drawn yet have no
/// state: it is made when they first show ([`ensure_markdown_block`]).
fn sync_markdown_block(
    markdown: &mut HashMap<(usize, usize), (Entity<TextViewState>, usize, usize)>,
    key: (usize, usize),
    (shown, drawn): (&str, usize),
    cx: &mut Context<SessionView>,
) {
    let Some((state, pushed, was_drawn)) = markdown.get_mut(&key) else {
        return;
    };
    if *pushed == shown.len() && *was_drawn == drawn {
        return;
    }
    // A diagram drawn since replaced its source: the start changed too.
    let appended = *was_drawn == drawn && shown.is_char_boundary(*pushed) && *pushed < shown.len();
    state.update(cx, |state, cx| {
        if appended {
            state.push_str(&shown[*pushed..], cx);
        } else {
            state.set_text(shown, cx);
        }
    });
    (*pushed, *was_drawn) = (shown.len(), drawn);
}

/// The Markdown for `key`, made from `shown` the first time it shows.
fn ensure_markdown_block(
    markdown: &mut HashMap<(usize, usize), (Entity<TextViewState>, usize, usize)>,
    key: (usize, usize),
    (shown, drawn): (&str, usize),
    cx: &mut Context<SessionView>,
) {
    markdown.entry(key).or_insert_with(|| {
        (
            cx.new(|cx| TextViewState::markdown(shown, cx)),
            shown.len(),
            drawn,
        )
    });
}

/// Whether `text` may hold a Mermaid block: most messages don't, and they
/// skip [`shown_markdown`]'s copy.
fn has_mermaid(text: &str) -> bool {
    text.contains("```mermaid") || text.contains("~~~mermaid")
}

/// What the renderer needs to know from the app, read once per sync.
struct DiagramEnv {
    dark: bool,
    cli: Option<std::path::PathBuf>,
}

impl DiagramEnv {
    fn read(cx: &App) -> Self {
        Self {
            dark: crate::features::files::mermaid_dark(cx),
            cli: crate::features::files::diagram_renderer_cli(cx),
        }
    }
}

/// `text` as the conversation shows it: Mermaid blocks drawn as images once
/// the renderer has drawn them (the others as their source), how many are
/// drawn, and the diagrams still to draw, added to `jobs`.
fn shown_markdown<'a>(
    diagrams: &crate::features::files::MermaidDiagrams,
    env: &DiagramEnv,
    text: &'a str,
    jobs: &mut Vec<crate::features::files::MermaidJob>,
) -> (std::borrow::Cow<'a, str>, usize) {
    if !has_mermaid(text) {
        return (text.into(), 0);
    }
    let prepared = diagrams.prepare(text, env.dark, env.cli.as_ref());
    jobs.extend(prepared.jobs);
    let drawn = prepared.text.matches("](mermaid:").count();
    (prepared.text.into(), drawn)
}

impl SessionView {
    /// A prompt is in flight: Pi is working or has not picked it up yet.
    pub(super) fn busy(&self) -> bool {
        self.transcript.running || self.live.pending.is_some()
    }

    /// Something the Stop button, ⌘. or Esc can stop: a turn or a `!command`.
    pub(super) fn stoppable(&self) -> bool {
        self.busy() || self.transcript.shells_running()
    }

    /// When the running turn, or the prompt not picked up yet, began.
    pub(super) fn turn_since(&self) -> Option<Instant> {
        self.live
            .turn
            .or_else(|| self.live.pending.as_ref().map(|p| p.since))
    }

    /// The empty state (hero and banner) shows.
    pub(super) fn hero_shown(&self) -> bool {
        self.transcript.items.is_empty() && self.live.pending.is_none()
    }

    /// Bring everything derived from the transcript up to date: Markdown,
    /// live spans, decoded images, card data, generative UI surfaces and
    /// the list's measurements. Only items from the first changed one on
    /// are revisited.
    pub(super) fn sync_view(&mut self, cx: &mut Context<Self>) {
        let dirty = self.transcript.take_dirty();
        if let Some(dirty) = dirty {
            if dirty.reset {
                self.markdown.clear();
                self.list.forget_items();
                // Keyed by item index: they would time or fade other items.
                self.live.tools.clear();
                self.live.thinking.clear();
                self.live.open_spans = 0;
                self.live.answer_done.clear();
                self.live.rail_hover = None;
            }
            let from = dirty.from.min(self.transcript.items.len());
            self.sync_markdown(from, cx);
            self.track_live(from);
            self.list.prepare(&self.transcript.items, from);
            // Steps fold to their quiet line by default; keep the list's
            // estimate weights in step (a running step that fails opens).
            let items = self.transcript.items.get(from..).unwrap_or_default();
            for (ix, item) in (from..).zip(items) {
                let collapsed = self.step_collapsed(ix, item);
                self.list.set_step_collapsed(ix, collapsed);
            }
            // A step that finishes next to others joins their run, and the
            // run's rows change height: restate every run from the one
            // around `from` (earlier items can sit in it) on.
            let items = &self.transcript.items;
            let (lo, len) = (
                items[..from]
                    .iter()
                    .rposition(|item| !steps::groupable(item) && assistant_visible(item))
                    .map_or(0, |ix| ix + 1),
                items.len(),
            );
            if let Some(changed) = self.sync_groups(lo..len) {
                self.list.remeasure_items(changed);
            }
            self.request_images(from, dirty.reset, cx);
            self.genui.set_root(&self.cwd, cx);
            self.genui
                .sync(&self.transcript.items, from, dirty.reset, cx);
            let session = self.session_file();
            self.genui.restore_pins(session.as_deref(), cx);
        }
        self.sync_live(cx);
        self.sync_genui_live(cx);
        let tail = self.tail_rows();
        let header = HeaderKey {
            hero: self.hero_shown(),
            backend: match self.backend.status {
                BackendStatus::Starting => 0,
                BackendStatus::Ready(_) => 1,
                BackendStatus::Failed(_) => 2,
                BackendStatus::Exited(_) => 3,
            },
        };
        self.list
            .sync(self.transcript.items.len(), tail, header, dirty);
    }

    /// Bring the rendered Markdown up to date for items `from..` (earlier
    /// items have not changed since the last sync).
    fn sync_markdown(&mut self, from: usize, cx: &mut Context<Self>) {
        if self.markdown.is_empty() {
            return;
        }
        let env = DiagramEnv::read(cx);
        let mut jobs = Vec::new();
        let items = self.transcript.items.get(from..).unwrap_or_default();
        for (item_ix, item) in (from..).zip(items) {
            let texts: Vec<(usize, &str)> = match item {
                Item::Assistant { blocks, .. } => blocks
                    .iter()
                    .enumerate()
                    .filter_map(|(ix, block)| match block {
                        Block::Text(text) => Some((ix, text.as_str())),
                        _ => None,
                    })
                    .collect(),
                // Helpers answer in Markdown, like the assistant.
                Item::Custom { kind, text, .. } if markdown_custom(kind) => vec![(0, text)],
                _ => continue,
            };
            for (block_ix, text) in texts {
                if !self.markdown.contains_key(&(item_ix, block_ix)) {
                    continue;
                }
                let (shown, drawn) = shown_markdown(&self.diagrams, &env, text, &mut jobs);
                sync_markdown_block(&mut self.markdown, (item_ix, block_ix), (&shown, drawn), cx);
            }
        }
        self.draw_diagrams(jobs, cx);
    }

    /// Make the Markdown of item `ix` the first time it shows.
    fn ensure_markdown(&mut self, ix: usize, cx: &mut Context<Self>) {
        let env = DiagramEnv::read(cx);
        let mut jobs = Vec::new();
        match self.transcript.items.get(ix) {
            Some(Item::Assistant { blocks, .. }) => {
                for (block_ix, block) in blocks.iter().enumerate() {
                    if let Block::Text(text) = block
                        && !self.markdown.contains_key(&(ix, block_ix))
                    {
                        let shown = shown_markdown(&self.diagrams, &env, text, &mut jobs);
                        ensure_markdown_block(
                            &mut self.markdown,
                            (ix, block_ix),
                            (&shown.0, shown.1),
                            cx,
                        );
                    }
                }
            }
            Some(Item::Custom { kind, text, .. })
                if markdown_custom(kind) && !self.markdown.contains_key(&(ix, 0)) =>
            {
                let shown = shown_markdown(&self.diagrams, &env, text, &mut jobs);
                ensure_markdown_block(&mut self.markdown, (ix, 0), (&shown.0, shown.1), cx);
            }
            _ => {}
        }
        self.draw_diagrams(jobs, cx);
    }

    /// Draw `jobs` off the UI thread, then show what they drew.
    fn draw_diagrams(
        &mut self,
        jobs: Vec<crate::features::files::MermaidJob>,
        cx: &mut Context<Self>,
    ) {
        if jobs.is_empty() {
            return;
        }
        let diagrams = self.diagrams.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    for job in &jobs {
                        diagrams.finish(job, crate::features::files::draw_mermaid(job));
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| this.redraw_diagrams(cx));
        })
        .detach();
    }

    /// Show diagrams drawn (or failed) since, and draw the ones a renderer
    /// installed since can draw now.
    pub(super) fn redraw_diagrams(&mut self, cx: &mut Context<Self>) {
        self.sync_markdown(0, cx);
        cx.notify();
    }

    /// A Markdown block of the conversation: its drawn Mermaid diagrams as
    /// images, code spans naming a file as paths ([`paths`]), and under it
    /// the renderer's banner while diagrams can't be drawn (`id` names its
    /// button).
    pub(super) fn markdown_view(
        &self,
        state: &Entity<TextViewState>,
        text: &str,
        id: impl Into<ElementId>,
        cx: &Context<Self>,
    ) -> Div {
        let diagrams = self.diagrams.clone();
        let blocks = if has_mermaid(text) {
            gentle_core::markdown::mermaid_blocks(text).len()
        } else {
            0
        };
        v_flex()
            .gap_2()
            .child(
                TextView::new(state)
                    .selectable(true)
                    .style(ui::prose_style())
                    .list_style(ui::prose_list_style())
                    .code_block_actions(|block, _, cx| code_block_header(block, cx))
                    .image_source(move |uri| {
                        diagrams.image(uri).unwrap_or_else(|| uri.clone().into())
                    })
                    .plugin(paths::AnswerPaths {
                        lookup: self.paths.clone(),
                        view: cx.entity().downgrade(),
                    }),
            )
            .children(crate::features::files::mermaid_banner(blocks, id, cx))
    }

    /// Record start/end instants of live items `from..`.
    fn track_live(&mut self, from: usize) {
        let now = Instant::now();
        let mut opened = 0;
        let items = self.transcript.items.get(from..).unwrap_or_default();
        for (ix, item) in (from..).zip(items) {
            match item {
                Item::Tool { status, .. } => {
                    opened += track(
                        &mut self.live.tools,
                        ix,
                        *status == ToolStatus::Running,
                        now,
                    )
                }
                Item::Shell { running, .. } => {
                    opened += track(&mut self.live.tools, ix, *running, now)
                }
                Item::Assistant {
                    blocks, streaming, ..
                } => {
                    for (bi, block) in blocks.iter().enumerate() {
                        if matches!(block, Block::Thinking(_)) {
                            let live = *streaming && bi + 1 == blocks.len();
                            opened += track(&mut self.live.thinking, (ix, bi), live, now);
                        }
                    }
                }
                _ => {}
            }
        }
        self.live.open_spans = self.live.open_spans.saturating_add_signed(opened);
    }

    /// Update the turn clock and the prompt in flight; whether live timers
    /// still need ticking.
    fn refresh_live(&mut self) -> bool {
        let now = Instant::now();
        let running = self.transcript.running;
        if running {
            self.live.turn.get_or_insert(now);
        } else {
            self.live.turn = None;
        }

        let ready = self.backend.is_ready();
        let items = &self.transcript.items;
        let dialogs = !self.extension.dialogs.is_empty();
        if let Some(pending) = &mut self.live.pending {
            pending.started |= running;
            let answered = items
                .get(pending.base..)
                .is_some_and(|rest| rest.iter().any(|i| matches!(i, Item::Assistant { .. })));
            // An extension command that never starts a turn: Pi acknowledged
            // and nothing followed (or it opened a dialog).
            let grace = if pending.text.starts_with('/') {
                Duration::from_millis(1500)
            } else {
                Duration::from_secs(8)
            };
            let idle_command =
                !pending.started && (dialogs || pending.acked.is_some_and(|t| t.elapsed() > grace));
            if answered || (pending.started && !running) || idle_command || !ready {
                self.live.pending = None;
            }
        }
        ticker_needed(
            running,
            self.transcript.shells_running(),
            self.live.pending.is_some(),
        )
    }

    /// Keep a ticker running exactly while something is live. Runs on every
    /// state change (not only on render), so hidden tabs stop ticking too.
    fn sync_live(&mut self, cx: &mut Context<Self>) {
        if !self.refresh_live() {
            self.live.ticker = None;
            self.live.ticking = false;
            return;
        }
        if self.live.ticking {
            return;
        }
        self.live.ticking = true;
        self.live.stamp = None;
        self.live.ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                crate::shared::diagnostics::tick();
                match this.update(cx, |this, cx| this.tick(cx)) {
                    Ok(true) => {}
                    _ => break,
                }
            }
        }));
    }

    /// One ticker step: repaint only when a timer label reads differently
    /// (or the prompt in flight resolved); false once nothing is live.
    fn tick(&mut self, cx: &mut Context<Self>) -> bool {
        let had_pending = self.live.pending.is_some();
        let live = self.refresh_live();
        self.live.ticks += 1;
        let stamp = label_stamp(
            (self.live.open_spans > 0).then_some(self.live.ticks),
            self.live.turn.map(|t| t.elapsed()),
            self.live.pending.as_ref().map(|p| p.since.elapsed()),
        );
        if self.live.stamp != Some(stamp) || had_pending != self.live.pending.is_some() {
            self.live.stamp = Some(stamp);
            cx.notify();
        }
        if !live {
            self.live.ticking = false;
        }
        live
    }

    /// Decode images of user and tool items `from..` off the UI thread (all
    /// shown images when the transcript was replaced, forgetting the rest).
    fn request_images(&mut self, from: usize, prune: bool, cx: &mut Context<Self>) {
        let start = if prune { 0 } else { from };
        let shown = self
            .transcript
            .items
            .get(start..)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| match item {
                Item::User { images, .. } | Item::Tool { images, .. } => Some(images),
                _ => None,
            })
            .flatten();
        let jobs = self.image_cache.request(shown, prune);
        self.image_cache.release_evicted(cx);
        if jobs.is_empty() {
            return;
        }
        let decoding = cx
            .background_executor()
            .spawn(async move { attachments::decode_all(jobs) });
        cx.spawn(async move |this, cx| {
            let decoded = decoding.await;
            let _ = this.update(cx, |this, cx| {
                this.image_cache.finish(decoded);
                cx.notify();
            });
        })
        .detach();
    }

    /// The rows after the transcript: the prompt just sent (until Pi echoes
    /// it) and a working indicator.
    fn tail_rows(&self) -> Vec<Tail> {
        let count = self.transcript.items.len();
        let mut tail = Vec::new();
        if let Some(pending) = &self.live.pending {
            // It takes the same slot (and animation id) as the real message
            // so nothing jumps.
            if count == pending.base && !pending.text.starts_with('/') {
                tail.push(Tail::PendingBubble(pending.base));
            }
            tail.push(Tail::Pending(pending.base));
        } else if self.transcript.running {
            // Between tool calls nothing streams; keep a pulse at the end.
            let idle_tail = match self.transcript.items.last() {
                Some(Item::Assistant { streaming, .. }) => !streaming,
                Some(Item::Tool { status, .. }) => *status != ToolStatus::Running,
                Some(Item::Shell { running, .. }) => !running,
                _ => true,
            };
            if idle_tail {
                tail.push(Tail::Between(count));
            }
        }
        tail
    }

    fn tool_duration(
        &self,
        ix: usize,
        status: ToolStatus,
        backend_ms: Option<u64>,
    ) -> Option<Duration> {
        let span = self.live.tools.get(&ix);
        if status == ToolStatus::Running {
            return span.map(Span::elapsed);
        }
        backend_ms
            .map(Duration::from_millis)
            .or_else(|| span.and_then(|s| s.took))
            .filter(|d| d.as_millis() >= 50)
    }

    fn toggle(&mut self, key: usize, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        // Fold keys carry their item index.
        let ix = key >> 16;
        // A whole-step fold changes the row's estimate too.
        if key & 3 == STEP
            && let Some(item) = self.transcript.items.get(ix)
        {
            let collapsed = self.step_collapsed(ix, item);
            self.list.set_step_collapsed(ix, collapsed);
        }
        self.list.remeasure_items(ix..ix + 1);
        cx.notify();
    }

    /// Fold or unfold the run of finished steps that starts at `start`.
    fn toggle_group(&mut self, start: usize, cx: &mut Context<Self>) {
        let key = group_key(start);
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        if let Some(changed) = self.sync_groups(start..start + 1) {
            self.list.remeasure_items(changed);
        }
        cx.notify();
    }

    /// Tell the list how the items of every run of finished steps that
    /// touches `range` render: a folded run shows only its summary line
    /// (at its first step), an unfolded one the line above its steps; items
    /// outside runs render as themselves. Returns the items whose rows
    /// changed, each touched run whole.
    fn sync_groups(&mut self, range: Range<usize>) -> Option<Range<usize>> {
        let items = &self.transcript.items;
        let mut changed: Option<Range<usize>> = None;
        let mut mark = |span: Range<usize>| {
            changed = Some(match changed.take() {
                Some(c) => c.start.min(span.start)..c.end.max(span.end),
                None => span,
            });
        };
        let mut ix = range.start;
        while ix < range.end {
            let Some(run) = steps::step_run(items, ix) else {
                if self.list.set_group(ix, None) {
                    mark(ix..ix + 1);
                }
                ix += 1;
                continue;
            };
            let open = self.expanded.contains(&group_key(run.start));
            let mut touched = false;
            for member in run.clone() {
                let group = match (member == run.start, open) {
                    (true, false) => Some(GroupRow::Summary),
                    (true, true) => Some(GroupRow::Open),
                    (false, false) => Some(GroupRow::Hidden),
                    (false, true) => None,
                };
                touched |= self.list.set_group(member, group);
            }
            if touched {
                mark(run.clone());
            }
            ix = run.end;
        }
        changed
    }

    /// Whether step `ix` shows its quiet one-line row (`Item::Shell` never
    /// does). A failed tool starts open, every other step starts folded;
    /// a click flips whichever it was (the key inverts the default).
    fn step_collapsed(&self, ix: usize, item: &Item) -> bool {
        gentle_core::transcript::step_folded(item, self.expanded.contains(&fold_key(ix, 0, STEP)))
    }

    // ── Empty and error states ─────────────────────────────────────────────

    fn render_hero(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let version = self.backend.version.clone().unwrap_or_default();
        let needs_sign_in =
            self.backend.is_ready() && self.models.loaded && self.models.available.is_empty();
        // The banner is ~175px per unit of scale; shrink it to fit the pane.
        let scale = ((self.pane_width.get() - 48.) / 175.).clamp(1.5, 4.0);
        v_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .items_center()
            .gap_1()
            .pt(rems(80. / 16.))
            .child(crate::shared::banner::banner(self.banner, scale, self.banner_reveal))
            .child(
                div()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(12.))
                    .text_color(ui::faint())
                    .child(if version.is_empty() { String::new() } else { format!("v{version}") }),
            )
            .when(needs_sign_in, |this| {
                this.child(ui::appear(
                    "hero-sign-in-card",
                    ui::card()
                        .mt_8()
                        .w_full()
                        .max_w(rems(420. / 16.))
                        .p_5()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .size(rems(36. / 16.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_xl()
                                .bg(ui::accent().opacity(0.12))
                                .child(Icon::new(IconName::KeyRound).small().text_color(ui::accent())),
                        )
                        .child(
                            div().text_size(rems(14. / 16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child("Sign in to start cooking"),
                        )
                        .child(
                            div().text_size(rems(12. / 16.))
                                .text_center()
                                .text_color(ui::muted())
                                .child("No models are available yet. Sign in with a subscription or add an API key."),
                        )
                        .child(
                            Button::new("hero-sign-in")
                                .primary()
                                .small()
                                .icon(Icon::new(IconName::LogIn))
                                .label("Sign in to a provider")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(super::SessionEvent::OpenProviders))),
                        ),
                ))
            })
    }

    fn render_backend_state(&self) -> Option<AnyElement> {
        let (title, body): (&str, String) = match &self.backend.status {
            BackendStatus::Starting => {
                return Some(
                    h_flex()
                        .pt(rems(24. / 16.))
                        .justify_center()
                        .child(self.live_slots.slot("starting", || {
                            h_flex()
                                .gap_2()
                                .text_size(rems(13. / 16.))
                                .text_color(ui::muted())
                                .child(ui::spinner(Size::Small, ui::accent()))
                                .child(ui::shimmer("Starting gentle-shell…", ui::muted(), ui::fg()))
                                .into_any_element()
                        }))
                        .into_any_element(),
                );
            }
            BackendStatus::Ready(_) => return None,
            BackendStatus::Failed(error) => ("Could not start gentle-shell", error.clone()),
            BackendStatus::Exited(code) => (
                "gentle-shell stopped",
                format!(
                    "Exit code {}.\n{}",
                    code.map_or("unknown".into(), |c| c.to_string()),
                    self.backend
                        .stderr
                        .iter()
                        .rev()
                        .take(12)
                        .rev()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ),
        };
        Some(
            ui::card()
                .mt_6()
                .mx_auto()
                .w(rems(560. / 16.))
                .p_4()
                .gap_3()
                .border_color(ui::danger().opacity(0.4))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(IconName::CircleX)
                                .small()
                                .text_color(ui::danger()),
                        )
                        .child(
                            div()
                                .text_size(rems(14. / 16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::danger())
                                .child(title),
                        ),
                )
                .child(
                    ui::well_box()
                        .p_3()
                        .text_color(ui::muted())
                        .whitespace_normal()
                        .child(body),
                )
                .into_any_element(),
        )
    }

    // ── Transcript ─────────────────────────────────────────────────────────

    fn render_item(
        &self,
        ix: usize,
        item: &Item,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match item {
            Item::User { text, images } => {
                let images = images.iter().map(|i| self.image_cache.get(i)).collect();
                let computed;
                let shown = match self.list.prepared(ix) {
                    Some(Prepared::Bubble(cell)) => cell.get_or_init(|| BubbleText::new(text)),
                    _ => {
                        computed = BubbleText::new(text);
                        &computed
                    }
                };
                let key = fold_key(ix, 0, OUTPUT);
                let open = self.expanded.contains(&key);
                let toggle = self.bubble_toggle(shown, key, open, cx);
                let bubble = user_bubble(ix, shown, images, open, toggle, &self.bubble_images);
                if text.trim().is_empty() {
                    return bubble.into_any_element();
                }
                v_flex()
                    .w_full()
                    .group("user")
                    .child(bubble)
                    .child(self.render_message_actions(ix, cx))
                    .into_any_element()
            }
            Item::Assistant {
                blocks,
                streaming,
                error,
            } => {
                let mut column = v_flex().w_full().gap_3();
                let has_text = blocks
                    .iter()
                    .any(|b| matches!(b, Block::Text(t) if !t.is_empty()));
                let mut thinking_live = false;
                // The answer starts a step away from the thought before it,
                // so the eye finds where to begin reading.
                let mut after_thought = false;
                for (block_ix, block) in blocks.iter().enumerate() {
                    match block {
                        Block::Text(text) => {
                            if let Some((state, ..)) = self.markdown.get(&(ix, block_ix)) {
                                column = column.child(
                                    div()
                                        .when(std::mem::take(&mut after_thought), |d| {
                                            d.pt(rems(8. / 16.))
                                        })
                                        .text_size(rems(14. / 16.))
                                        .text_color(ui::fg())
                                        .child(self.markdown_view(
                                            state,
                                            text,
                                            ("mermaid-install", ix * 64 + block_ix),
                                            cx,
                                        )),
                                );
                            }
                        }
                        Block::Thinking(text) if !text.trim().is_empty() => {
                            let live = *streaming && block_ix + 1 == blocks.len();
                            thinking_live |= live;
                            after_thought = true;
                            column =
                                column.child(self.render_thinking(ix, block_ix, text, live, cx));
                        }
                        _ => {}
                    }
                }
                if *streaming && !has_text && !thinking_live {
                    let since = self.live.turn.map(|t| t.elapsed());
                    column = column.child(working_indicator(
                        &self.live_slots,
                        ("working", ix),
                        "Working…",
                        since,
                    ));
                }
                if let Some(error) = error {
                    column = column.child(notice_card(NoticeLevel::Error, error));
                }
                // While the agent writes, the petal line beside the answer
                // marks it live (the composer's orbit is the turn's one
                // moving indicator); when it's done, the line fades.
                let settle = self.answer_settle(ix, *streaming);
                if !*streaming && has_text {
                    column = column
                        .group("assistant")
                        .child(self.render_answer_actions(ix, cx));
                }
                match self.petal_line(settle, narrow) {
                    Some(line) => div()
                        .relative()
                        .w_full()
                        .child(line)
                        .child(column)
                        .into_any_element(),
                    None => column.into_any_element(),
                }
            }
            Item::Tool { name, .. } if name == gentle_core::genui::TOOL => {
                self.genui.render_tool(ix, item, cx)
            }
            Item::Custom { kind, details, .. } if kind == gentle_core::genui::EVENT_MESSAGE => {
                self.genui.render_event(ix, details, cx)
            }
            Item::Tool { .. } | Item::Shell { .. } => {
                let computed;
                let spec = match self.list.prepared(ix) {
                    Some(Prepared::Card(cell)) => cell.get_or_init(|| card_spec(item)),
                    _ => {
                        computed = card_spec(item);
                        &computed
                    }
                };
                let Some(spec) = spec else {
                    return div().into_any_element();
                };
                let backend_ms = match item {
                    Item::Tool { duration_ms, .. } => *duration_ms,
                    _ => None,
                };
                let duration = self.tool_duration(ix, spec.status, backend_ms);
                self.render_card(ix, item, spec, duration, cx)
            }
            Item::Custom {
                kind,
                text,
                details,
            } => {
                let lines = match self.list.prepared(ix) {
                    Some(Prepared::Lines(lines)) => *lines,
                    _ => line_count(text),
                };
                self.render_custom(ix, kind, text, details, lines, cx)
            }
            Item::CommandOutput { command, notices } => {
                let lines = match self.list.prepared(ix) {
                    Some(Prepared::Lines(lines)) => *lines,
                    _ => notices.iter().map(|(_, text)| line_count(text)).sum(),
                };
                self.render_command_output(ix, command, notices, lines, cx)
            }
            Item::Notice { level, text } => notice_card(*level, text).into_any_element(),
        }
    }

    /// How far answer `ix` has settled: 0 while the agent writes, rising
    /// to 1 over `ANSWER_FADE` once it's done; `None` for answers that
    /// finished before this view saw them (nothing to animate).
    fn answer_settle(&self, ix: usize, streaming: bool) -> Option<f32> {
        if streaming {
            return Some(0.);
        }
        self.live
            .answer_done
            .get(&ix)
            .map(|at| (at.elapsed().as_secs_f32() / ANSWER_FADE.as_secs_f32()).min(1.))
    }

    /// The thin rose line at an answer's left, in the row's margin (so the
    /// text doesn't move). It spans the answer, so it grows down with the
    /// text while the agent writes; once the answer is done it draws back
    /// up and fades out, and finished answers have none.
    fn petal_line(&self, settle: Option<f32>, narrow: bool) -> Option<Div> {
        let settle = settle.filter(|k| *k < 1.)?;
        let k = ease_out_quint()(settle);
        Some(
            div()
                .absolute()
                .top(px(2.))
                .h(relative(1. - k))
                .left(rems((if narrow { -8. } else { -14. }) / 16.))
                .w(px(2.))
                .rounded_full()
                .bg(ui::petal().opacity(0.75 * (1. - k))),
        )
    }

    /// Answer `ix` just finished: fade its petal line (frames are asked
    /// for while it fades, see [`Self::fade_answers`]). With reduced
    /// motion it just goes.
    pub(in crate::features::session) fn answer_finished(
        &mut self,
        ix: usize,
        cx: &mut Context<Self>,
    ) {
        if cx.reduce_motion() {
            return;
        }
        self.live.answer_done.insert(ix, Instant::now());
        cx.notify();
    }

    /// Forget answers that finished fading; whether any still fades.
    fn fade_answers(&mut self) -> bool {
        self.live
            .answer_done
            .retain(|_, at| at.elapsed() < ANSWER_FADE);
        !self.live.answer_done.is_empty()
    }

    fn render_thinking(
        &self,
        ix: usize,
        block_ix: usize,
        text: &str,
        live: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let key = fold_key(ix, block_ix, THINKING);
        // Live thoughts read along; finished ones fold away. A click flips either.
        let open = live != self.expanded.contains(&key);
        let elapsed = self.live.thinking.get(&(ix, block_ix)).map(Span::elapsed);
        let star = div()
            .text_color(if live { ui::info() } else { ui::faint() })
            .child("✳");
        v_flex()
            .w_full()
            .gap_1p5()
            .child(
                h_flex()
                    .id(("thinking", key))
                    .gap_1p5()
                    .cursor_pointer()
                    .text_size(rems(13. / 16.))
                    .italic()
                    .text_color(ui::muted())
                    .hover(|s| s.text_color(ui::fg()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(key, cx)))
                    .child(star)
                    // Live, its time counts up once a second.
                    .when(live, |this| this.child(ui::each_second()))
                    .child(if live { "Thinking…" } else { "Thought" })
                    .when_some(elapsed, |this, d| {
                        this.child(
                            div()
                                .font_family(ui::mono())
                                .text_size(ui::code_rems(11.))
                                .text_color(ui::faint())
                                .child(ui::format_duration(d.as_millis() as u64)),
                        )
                    })
                    .child(
                        Icon::new(if open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall()
                        .text_color(ui::faint()),
                    ),
            )
            .when(open, |this| {
                // Trimmed once per change of the item, not per frame.
                let shown = match self.list.prepared(ix) {
                    Some(Prepared::Thoughts(cells)) => cells.get(block_ix),
                    _ => None,
                }
                .map(|cell| cell.get_or_init(|| text.trim().to_string().into()).clone())
                .unwrap_or_else(|| text.trim().to_string().into());
                this.child(
                    div()
                        .ml(rems(5. / 16.))
                        .pl_3()
                        .text_size(rems(13. / 16.))
                        .italic()
                        .text_color(ui::muted())
                        .child(shown),
                )
            })
    }

    /// An extension command's `notify` output as one card: the command as
    /// typed, then each notice in order, warnings and errors tinted.
    fn render_command_output(
        &self,
        ix: usize,
        command: &str,
        notices: &[(NoticeLevel, String)],
        lines: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const SHOWN_LINES: usize = 16;
        let key = fold_key(ix, 0, OUTPUT);
        let open = self.expanded.contains(&key);
        let foldable = lines > SHOWN_LINES;
        let worst = notices
            .iter()
            .map(|(level, _)| *level)
            .max_by_key(|level| match level {
                NoticeLevel::Info => 0,
                NoticeLevel::Warning => 1,
                NoticeLevel::Error => 2,
            })
            .unwrap_or(NoticeLevel::Info);
        let accent = match worst {
            NoticeLevel::Info => None,
            NoticeLevel::Warning => Some(ui::warning()),
            NoticeLevel::Error => Some(ui::danger()),
        };
        let rows = notices.iter().map(|(level, text)| {
            let (icon, color) = match level {
                NoticeLevel::Info => (None, ui::fg()),
                NoticeLevel::Warning => (Some(IconName::TriangleAlert), ui::warning()),
                NoticeLevel::Error => (Some(IconName::CircleX), ui::danger()),
            };
            h_flex()
                .w_full()
                .items_start()
                .gap_1p5()
                .when_some(icon, |this, icon| {
                    this.child(
                        div()
                            .pt(px(2.))
                            .child(Icon::new(icon).xsmall().text_color(color)),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(color)
                        .whitespace_normal()
                        .child(text.trim_end().to_string()),
                )
        });
        ui::card()
            .w_full()
            .overflow_hidden()
            .when_some(accent, |this, color| this.border_color(color.opacity(0.35)))
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .min_w_0()
                    .child(
                        Icon::new(IconName::SquareTerminal)
                            .small()
                            .text_color(accent.unwrap_or(ui::muted())),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(ui::mono())
                            .text_size(ui::code_rems(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui::champagne())
                            .child(command.to_string()),
                    ),
            )
            .child(
                div().px_3().pb_2().child(
                    ui::well_box()
                        .p_3()
                        .gap_1p5()
                        .when(foldable && !open, |d| {
                            d.max_h(rems((SHOWN_LINES as f32 * 18.) / 16.))
                                .overflow_hidden()
                        })
                        .children(rows),
                ),
            )
            .when(foldable, |this| {
                this.child(div().pb_1().child(self.fold_toggle(
                    key,
                    if open {
                        "Show less".into()
                    } else {
                        format!("{} more lines", lines - SHOWN_LINES)
                    },
                    open,
                    cx,
                )))
            })
            .into_any_element()
    }

    /// Under a finished answer, shown on hover: copy it.
    fn render_answer_actions(&self, ix: usize, cx: &mut Context<Self>) -> Div {
        h_flex()
            .h(rems(20. / 16.))
            .mt(rems(-4. / 16.))
            .invisible()
            .group_hover("assistant", |s| s.visible())
            .child(
                Button::new(("copy-answer", ix))
                    .ghost()
                    .xsmall()
                    .text_color(ui::faint())
                    .icon(Icon::new(IconName::Copy))
                    .tooltip("Copy answer")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let Some(Item::Assistant { blocks, .. }) = this.transcript.items.get(ix)
                        else {
                            return;
                        };
                        let text = blocks
                            .iter()
                            .filter_map(|b| match b {
                                Block::Text(t) if !t.trim().is_empty() => Some(t.trim()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        this.copy_text(text, cx);
                    })),
            )
    }

    /// Under a message you sent, shown on hover: copy its text.
    fn render_message_actions(&self, ix: usize, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .justify_end()
            .h(rems(20. / 16.))
            .mt(px(2.))
            .invisible()
            .group_hover("user", |s| s.visible())
            .child(
                Button::new(("copy-message", ix))
                    .ghost()
                    .xsmall()
                    .text_color(ui::faint())
                    .icon(Icon::new(IconName::Copy))
                    .tooltip("Copy message")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(Item::User { text, .. }) = this.transcript.items.get(ix) {
                            this.copy_text(text.trim().to_string(), cx);
                        }
                    })),
            )
    }

    fn render_card(
        &self,
        ix: usize,
        item: &Item,
        spec: &CardSpec,
        duration: Option<Duration>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (input, output) = card_texts(item);
        let out_key = fold_key(ix, 0, OUTPUT);
        let open = self.expanded.contains(&out_key);
        let has_output = spec.has_output;
        let show_output =
            has_output && (spec.always_output || open || spec.status != ToolStatus::Done);
        let foldable = has_output && !spec.always_output;
        // A subagent run's card would only say "Running": its line opens
        // the Agents panel on that agent instead. A failed run keeps its
        // card, which shows the error.
        if let Item::Tool { name, args, .. } = item
            && name == "subagent_run"
            && spec.status != ToolStatus::Failed
            && let Some(agent) = args["agent"].as_str()
        {
            let agent = agent.to_string();
            let prompt = ["prompt", "task"]
                .iter()
                .find_map(|key| args[*key].as_str())
                .map(str::to_string);
            return self
                .step_line(ix, spec, duration, None, cx)
                .cursor_pointer()
                .rounded_lg()
                .hover(|s| s.bg(ui::surface_hover()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let helper = this.helper_for_run(&agent, prompt.as_deref());
                    this.open_agents(helper, cx);
                }))
                .into_any_element();
        }
        // A finished tool step shows its quiet line first; a click unfolds
        // the whole card. What you ran yourself (`Shell`) always shows it.
        let step_key = fold_key(ix, 0, STEP);
        let step_open = !self.step_collapsed(ix, item);
        if !step_open {
            return self
                .step_line(ix, spec, duration, Some((step_key, false)), cx)
                .into_any_element();
        }
        let header = if matches!(item, Item::Shell { .. }) {
            self.card_header(ix, spec, out_key, foldable, cx)
                .into_any_element()
        } else {
            self.step_line(ix, spec, duration, Some((step_key, true)), cx)
                .into_any_element()
        };
        ui::card()
            .w_full()
            .overflow_hidden()
            .child(header)
            .when_some(
                input.zip(spec.input_lines),
                |this, ((text, diff), lines)| {
                    this.child(div().px_3().pb_2().child(self.code_well(
                        fold_key(ix, 0, INPUT),
                        text,
                        lines,
                        diff,
                        &spec.input_rows,
                        cx,
                    )))
                },
            )
            .when(show_output, |this| {
                this.child(self.output_block(
                    out_key,
                    output,
                    spec.output_lines,
                    &spec.output_rows,
                    cx,
                ))
            })
            .when_some(
                match item {
                    Item::Tool { images, .. } if !images.is_empty() => Some(images),
                    _ => None,
                },
                |this, images| {
                    // A page screenshot, whole: its width, at most 360 px high.
                    this.child(v_flex().px_3().pb_2().gap_2().children(images.iter().map(
                        |image| {
                            match self.image_cache.get(image) {
                                Some(image) => div()
                                    .w_full()
                                    .rounded_lg()
                                    .overflow_hidden()
                                    .border_1()
                                    .border_color(ui::border_strong())
                                    .child(
                                        img(image)
                                            .w_full()
                                            .max_h(rems(360. / 16.))
                                            .object_fit(ObjectFit::Contain),
                                    ),
                                None => image_tile(None, rems(160. / 16.), rems(100. / 16.)),
                            }
                        },
                    )))
                },
            )
            .child(status_footer(spec.status, duration))
            .into_any_element()
    }

    /// A collapsed step: one quiet line, no card around it. The icon takes
    /// its kind's tint ([`step_tone`]; danger's cross when the step failed,
    /// pulsing on the live overlay while it runs),
    /// the rest is today's header restyled: title, subject, +n/−n, duration
    /// and a chevron. Clicking it unfolds the whole step when `fold` (its
    /// key and whether the chevron points up) is given.
    fn step_line(
        &self,
        ix: usize,
        spec: &CardSpec,
        duration: Option<Duration>,
        fold: Option<(usize, bool)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let failed = spec.status == ToolStatus::Failed;
        let icon = if failed { IconName::CircleX } else { spec.icon };
        let icon_color = if failed {
            ui::danger()
        } else {
            step_tone(spec.icon)
        };
        let icon = Icon::new(icon)
            .small()
            .text_color(icon_color)
            .into_any_element();
        self.step_row(
            ("step", ix),
            spec,
            icon,
            StepRowStyle::Quiet,
            duration,
            fold,
            cx,
        )
        .w_full()
    }

    /// Today's tool card header: icon, title, subject, +n/−n and a chevron
    /// when the output folds. Kept for `Item::Shell`, which never folds as
    /// a whole step.
    fn card_header(
        &self,
        ix: usize,
        spec: &CardSpec,
        out_key: usize,
        foldable: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.step_row(
            ("tool", ix),
            spec,
            Icon::new(spec.icon)
                .small()
                .text_color(step_tone(spec.icon))
                .into_any_element(),
            StepRowStyle::Card,
            None,
            foldable.then(|| (out_key, self.expanded.contains(&out_key))),
            cx,
        )
    }

    /// The row both a step's quiet line and a card's header build from:
    /// icon, title, chip, subject, +n/−n, duration and chevron, so the two
    /// can't drift. The callers pass their differences: `icon` (the quiet
    /// line pulses it and tones failure), `style` (title and subject tone),
    /// `duration`, and `fold` — a toggling row's key and whether its
    /// chevron points up.
    #[allow(clippy::too_many_arguments)]
    fn step_row(
        &self,
        id: impl Into<ElementId>,
        spec: &CardSpec,
        icon: AnyElement,
        style: StepRowStyle,
        duration: Option<Duration>,
        fold: Option<(usize, bool)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (title_color, title_weight, subject_color) = match style {
            StepRowStyle::Quiet => (ui::muted(), FontWeight::NORMAL, ui::faint()),
            StepRowStyle::Card => (ui::champagne(), FontWeight::SEMIBOLD, ui::muted()),
        };
        h_flex()
            .id(id)
            .debug_selector(|| format!("step-row:{}:{}", spec.title, spec.subject))
            .px_3()
            .py_2()
            .gap_2()
            .min_w_0()
            .when_some(fold, |this, (key, _)| {
                this.cursor_pointer()
                    .rounded_lg()
                    .hover(|s| s.bg(ui::surface_hover()))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(key, cx)))
            })
            .child(icon)
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.)
                    .max_w(rems(420. / 16.))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(rems(13. / 16.))
                    .font_weight(title_weight)
                    .text_color(title_color)
                    .child(spec.title.clone()),
            )
            .when_some(spec.chip.clone(), |this, chip| {
                this.child(ui::chip(chip, ui::info()).flex_shrink_0())
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(12.))
                    .text_color(subject_color)
                    .child(spec.subject.clone()),
            )
            .when_some(spec.added, |this, n| {
                this.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(ui::success())
                        .child(format!("+{n}")),
                )
            })
            .when_some(spec.removed, |this, n| {
                this.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(ui::danger())
                        .child(format!("−{n}")),
                )
            })
            .when_some(duration, |this, d| {
                this.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(11.))
                        .text_color(ui::faint())
                        .child(ui::format_duration(d.as_millis() as u64)),
                )
            })
            .when_some(fold, |this, (_, up)| {
                this.child(
                    Icon::new(if up {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    })
                    .xsmall()
                    .text_color(ui::faint()),
                )
            })
    }

    /// A command, file body or diff (`lines` long), clipped from the bottom with a fade.
    fn code_well(
        &self,
        key: usize,
        text: &str,
        lines: usize,
        diff: bool,
        rows: &BodyLines,
        cx: &mut Context<Self>,
    ) -> Div {
        let max = if diff { DIFF_LINES } else { INPUT_LINES };
        let open = self.expanded.contains(&key);
        let shown_max = if open { 400 } else { max };
        let hidden = lines.saturating_sub(shown_max);
        let shown = rows.get(open, || {
            let (shown, _) = head_slice(text, lines, shown_max);
            shown
                .lines()
                .map(|line| {
                    if diff {
                        CodeLine::diff(line)
                    } else {
                        CodeLine::plain(line)
                    }
                })
                .collect()
        });
        let more = lines.saturating_sub(max);
        let clipped = !open && hidden > 0;
        let lines = v_flex()
            .relative()
            .py_2()
            .overflow_hidden()
            .children(shown.iter().map(|line| {
                if diff {
                    diff_row(line)
                } else {
                    div()
                        .w_full()
                        .min_w_0()
                        .px_3()
                        .whitespace_normal()
                        .text_color(ui::fg())
                        .child(line.text.clone())
                }
            }))
            .when(clipped, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(rems(28. / 16.))
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(ui::well().opacity(0.), 0.),
                            linear_color_stop(ui::well(), 1.),
                        )),
                )
            });
        ui::well_box()
            .w_full()
            .overflow_hidden()
            .child(lines)
            .when(open && hidden > 0, |this| {
                this.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_color(ui::faint())
                        .child(format!("… {hidden} more lines not shown")),
                )
            })
            .when(more > 0, |this| {
                this.child(div().pb_1().child(self.fold_toggle(
                    key,
                    if open {
                        "Show less".into()
                    } else {
                        format!("{more} more lines")
                    },
                    open,
                    cx,
                )))
            })
    }

    /// Command output (`lines` long), clipped from the top so the latest lines show.
    fn output_block(
        &self,
        key: usize,
        body: &str,
        lines: usize,
        rows: &BodyLines,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.expanded.contains(&key);
        let shown_max = if open { 200 } else { OUTPUT_LINES };
        let hidden = lines.saturating_sub(shown_max);
        let shown = rows.get(open, || {
            let (shown, _) = tail_slice(body, lines, shown_max);
            shown.lines().map(CodeLine::plain).collect()
        });
        let more = lines.saturating_sub(OUTPUT_LINES);
        v_flex()
            .w_full()
            .pt_1p5()
            .pb_1()
            .when(more > 0, |this| {
                this.child(self.fold_toggle(
                    key,
                    if open {
                        "Show fewer lines".into()
                    } else {
                        format!("{more} earlier lines")
                    },
                    !open,
                    cx,
                ))
            })
            .child(
                v_flex()
                    .px_3()
                    .py_1()
                    .overflow_hidden()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(12.))
                    .text_color(ui::muted())
                    .when(open && hidden > 0, |this| {
                        this.child(
                            div()
                                .text_color(ui::faint())
                                .child(format!("… {hidden} earlier lines not shown")),
                        )
                    })
                    .children(shown.iter().map(|line| {
                        div()
                            .debug_selector(|| "output-wrap-padding-row".into())
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .child(line.text.clone())
                    })),
            )
    }

    /// A folded message's "N more lines" / "Show less", if it folds.
    fn bubble_toggle(
        &self,
        shown: &BubbleText,
        key: usize,
        open: bool,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (_, hidden) = shown.folded.as_ref()?;
        let label = if open {
            "Show less".into()
        } else {
            format!("{hidden} more lines")
        };
        Some(self.fold_toggle(key, label, open, cx))
    }

    fn fold_toggle(
        &self,
        key: usize,
        label: String,
        up: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        h_flex()
            .id(("fold", key))
            .gap_1()
            .px_3()
            .py_0p5()
            .font_family(cx.theme().font_family.clone())
            .text_size(rems(11. / 16.))
            .text_color(ui::muted())
            .cursor_pointer()
            .hover(|s| s.text_color(ui::fg()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(key, cx)))
            .debug_selector(|| format!("fold:{label}"))
            .child(label)
            .child(
                Icon::new(if up {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .xsmall(),
            )
    }
}

impl Focusable for SessionView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SessionView {
    /// The summary line of the run of finished steps that starts at item
    /// `start`, a quiet step line itself: per kind of step its icon (toned
    /// like the steps') and a count in words, then the run's total time
    /// and a chevron (up when `open`). A click folds or unfolds the run.
    fn run_line(&self, start: usize, open: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = &self.transcript.items;
        let run = steps::step_run(items, start).unwrap_or(start..start + 1);
        let took: Duration = (run.clone())
            .filter_map(|ix| match &items[ix] {
                Item::Tool {
                    status,
                    duration_ms,
                    ..
                } => self.tool_duration(ix, *status, *duration_ms),
                _ => None,
            })
            .sum();
        let parts = steps::run_summary(items, run)
            .into_iter()
            .enumerate()
            .flat_map(|(n, (kind, count))| {
                let icon = kind_icon(kind);
                let separator =
                    (n > 0).then(|| div().text_color(ui::faint()).child("·").into_any_element());
                separator.into_iter().chain([h_flex()
                    .gap_2()
                    .flex_shrink_0()
                    .child(Icon::new(icon).small().text_color(step_tone(icon)))
                    .child(div().text_color(ui::muted()).child(kind.label(count)))
                    .into_any_element()])
            });
        h_flex()
            .id(("run", start))
            .w_full()
            .px_3()
            .py_2()
            .gap_2()
            .min_w_0()
            .cursor_pointer()
            .rounded_lg()
            .hover(|s| s.bg(ui::surface_hover()))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(start, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_2()
                    .whitespace_nowrap()
                    .text_size(rems(13. / 16.))
                    .children(parts),
            )
            .when(!took.is_zero(), |this| {
                this.child(
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(11.))
                        .text_color(ui::faint())
                        .child(ui::format_duration(took.as_millis() as u64)),
                )
            })
            .child(
                Icon::new(if open {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .xsmall()
                .text_color(ui::faint()),
            )
    }

    /// One row of the transcript list; `cx.processor` calls it for the
    /// rows on screen (and in the overdraw) only.
    fn render_row(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.list.row(ix) else {
            return div().into_any_element();
        };
        let narrow =
            self.pane_width.get() < f32::from(rems(NARROW / 16.).to_pixels(window.rem_size()));
        let hero = self.hero_shown();
        // `render_backend_state` shows something unless the backend is ready.
        let header = hero || !self.backend.is_ready();
        let now = Instant::now();
        let content = match row {
            Row::Header => {
                if !header {
                    return div().into_any_element();
                }
                v_flex()
                    .w_full()
                    .gap_4()
                    .when(hero, |this| this.child(self.render_hero(cx)))
                    .children(self.render_backend_state())
                    .into_any_element()
            }
            Row::Item(i) => {
                // A folded run of finished steps shows only its summary
                // line, at its first step: the others render no row.
                let group = self.list.group(i);
                if group == Some(GroupRow::Hidden) {
                    return div().into_any_element();
                }
                self.ensure_markdown(i, cx);
                let Some(item) = self.transcript.items.get(i) else {
                    return div().into_any_element();
                };
                // A finished assistant message with nothing to show (it only
                // announced tool calls) renders no row at all: no padding, no
                // gap, and a run of steps flows across it.
                if !assistant_visible(item) {
                    return div().into_any_element();
                }
                let element = match group {
                    Some(GroupRow::Summary) => self.run_line(i, false, cx).into_any_element(),
                    // Unfolded: the summary line above the first step.
                    Some(GroupRow::Open) => v_flex()
                        .w_full()
                        .when(!self.step_collapsed(i, item), |d| d.gap(rems(8. / 16.)))
                        .child(self.run_line(i, true, cx))
                        .child(self.render_item(i, item, narrow, cx))
                        .into_any_element(),
                    _ => self.render_item(i, item, narrow, cx),
                };
                // Keyed by index: items animate in once, never again.
                self.list
                    .appear(("item", i), now, div().w_full().child(element))
            }
            Row::Tail(Tail::PendingBubble(base)) => {
                let Some(pending) = &self.live.pending else {
                    return div().into_any_element();
                };
                let shown = pending
                    .bubble
                    .get_or_init(|| BubbleText::new(&pending.text));
                // Keyed like the item its echo becomes, so it opens the same.
                let key = fold_key(base, 0, OUTPUT);
                let open = self.expanded.contains(&key);
                let toggle = self.bubble_toggle(shown, key, open, cx);
                let bubble = user_bubble(
                    base,
                    shown,
                    pending.images.iter().cloned().map(Some).collect(),
                    open,
                    toggle,
                    &self.bubble_images,
                );
                self.list
                    .appear(("item", base), now, div().w_full().child(bubble))
            }
            Row::Tail(Tail::Pending(base)) => {
                let since = self.live.pending.as_ref().map(|p| p.since.elapsed());
                let indicator =
                    working_indicator(&self.live_slots, ("pending", base), "Thinking…", since);
                self.list
                    .appear(("pending", base), now, div().w_full().child(indicator))
            }
            Row::Tail(Tail::Between(count)) => {
                let since = self.live.turn.map(|t| t.elapsed());
                let indicator =
                    working_indicator(&self.live_slots, ("between", count), "Working…", since);
                self.list
                    .appear(("between", count), now, div().w_full().child(indicator))
            }
            // The column's bottom padding.
            Row::Footer => return div().h(rems(32. / 16.)).into_any_element(),
        };
        let narrow = self.pane_width.get() < NARROW;
        // Rows of consecutive steps sit close together: the top padding
        // stays before and after a run of steps, not between its rows. An
        // open step is a card: it keeps a small gap from its neighbours so
        // two cards never touch.
        let tight = matches!(row, Row::Item(i)
            if follows_step(&self.transcript.items, i));
        let card_gap = match row {
            Row::Item(i) if tight => {
                let items = &self.transcript.items;
                let open = |ix: usize| !self.step_collapsed(ix, &items[ix]);
                // A run's summary line is a quiet line: the row starts with
                // it, and a folded run ends with it.
                let group = |ix: usize| self.list.group(ix);
                let card_top = group(i).is_none() && open(i);
                let card_bottom = |ix: usize| {
                    !matches!(group(ix), Some(GroupRow::Summary | GroupRow::Hidden)) && open(ix)
                };
                let previous = items[..i].iter().rposition(assistant_visible);
                card_top || previous.is_some_and(card_bottom)
            }
            _ => false,
        };
        h_flex()
            .w_full()
            .justify_center()
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .max_w(rems(760. / 16.))
                    .map(|d| if narrow { d.px_3() } else { d.px_6() })
                    // The column's top padding before each row.
                    .when(!tight, |d| d.pt(rems(24. / 16.)))
                    .when(card_gap, |d| d.pt(rems(8. / 16.)))
                    .child(content),
            )
            .into_any_element()
    }
}

#[cfg(test)]
thread_local! {
    /// `SessionView` renders on this thread, for tests that count a tab's
    /// redraws (each GPUI test runs on its own thread).
    pub(super) static RENDERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Render for SessionView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::shared::diagnostics::render("SessionView");
        #[cfg(test)]
        RENDERS.with(|renders| renders.set(renders.get() + 1));
        // Its indicators on the live layer are placed again as it draws.
        self.live_slots.clear(cx.entity_id());
        // A tab starts its backend once shown.
        self.ensure_started(window, cx);
        self.sync_view(cx);
        self.watch_surfaces(window, cx);
        self.step_glide(window);
        // Timed effects (the composer's, answers fading) ask for each next
        // frame only while they play.
        let composing = self.composer_animating();
        if self.fade_answers() || composing {
            ui::request_animation_frame(window);
        }
        let rem_size = window.rem_size();
        self.list.follow_rem_size(rem_size);
        let scroll = self.scroll_estimate();
        let width = self.pane_width.clone();
        let view = cx.entity_id();
        let narrow = self.pane_width.get() < f32::from(rems(NARROW / 16.).to_pixels(rem_size));
        // An expanded generative UI surface takes the conversation's place;
        // the composer stays, to talk about it. A web view there hides while
        // the `/` menu, which opens over it, is up, or the tab isn't shown.
        let expanded = self.genui.expanded();
        let surface_open = expanded.is_some();
        if expanded.is_some() {
            let obscured = !self.web_shown || self.slash_menu_open(cx);
            self.genui.set_obscured(obscured, cx);
        }
        let main = v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(ui::surface_hover()))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attach_paths(paths.paths().to_vec(), window, cx);
            }))
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
            .map(|main| match expanded {
                Some(surface) => main.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .map(|d| if narrow { d.px_3() } else { d.px_6() })
                        .pt_4()
                        .pb_2()
                        .child(
                            surface
                                .view()
                                .cached(StyleRefinement::default().size_full()),
                        ),
                ),
                None => main.child(
                    div()
                        .id("transcript")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        // A wheel or trackpad scroll takes over from a glide and
                        // shows the rail's thumb.
                        .on_scroll(cx.listener(|this, _, _, cx| this.on_user_scroll(cx)))
                        .child(
                            list(
                                self.list.state(),
                                cx.processor(|this, ix, window, cx| {
                                    this.render_row(ix, window, cx)
                                }),
                            )
                            .size_full(),
                        )
                        .children(self.render_rail(&scroll, cx))
                        // Text fades out as it reaches the composer, after
                        // Tern's, instead of cutting off on a hard edge. As
                        // tall as the column's bottom padding, so the last
                        // line, scrolled to the end, stays clear. Only on an
                        // opaque pane: GPUI can't mask, so the fade is the
                        // pane's color laid over the text, and over glass
                        // that color doubles up into a dark band.
                        .when(ui::panel().a > 0.99, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .bottom_0()
                                    .h(rems(32. / 16.))
                                    .bg(linear_gradient(
                                        180.,
                                        linear_color_stop(ui::panel().opacity(0.), 0.),
                                        linear_color_stop(ui::panel(), 1.),
                                    )),
                            )
                        })
                        .children(self.render_jump_down(cx)),
                ),
            })
            .map(|main| {
                let (composer, strip) = self.render_composer(rem_size, cx);
                // Bar docks the composer in a strip across the pane, on a
                // hairline; Prompt adds its status strip along the bottom.
                let dock = super::input_style::style(cx) == InputStyle::Bar;
                main.child(
                    h_flex()
                        .w_full()
                        .justify_center()
                        .map(|d| if narrow { d.px_3() } else { d.px_6() })
                        .when(dock, |d| {
                            d.pt_3()
                                .border_t_1()
                                .border_color(ui::border())
                                .bg(ui::surface().opacity(0.35))
                                // Its bottom corners follow the pane's.
                                .rounded_b_xl()
                        })
                        .pb_2()
                        .child(composer),
                )
                .children(strip)
            })
            // The Todos island floats over the top of the conversation, where
            // an expanded surface keeps its header controls (collapse among
            // them): it steps aside while one is expanded.
            .when_some(
                (!surface_open)
                    .then(|| self.render_todo_island(rem_size, cx))
                    .flatten(),
                |this, island| this.child(island),
            )
            .children(self.genui.render_templates(self.live_slots.cover(), cx));

        h_flex()
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::abort))
            .on_action(cx.listener(Self::on_stop))
            .on_action(cx.listener(Self::toggle_agents))
            .on_action(cx.listener(Self::on_new_session))
            .capture_key_down(cx.listener(Self::on_dialog_key))
            .relative()
            .size_full()
            .child(main)
            .children(self.genui.render_pinned(cx))
    }
}

// ── Building blocks ────────────────────────────────────────────────────────

/// Everything a tool or shell card shows except its text bodies, derived
/// once per change of its item; the bodies are borrowed from the item when
/// drawn ([`card_texts`]).
struct CardSpec {
    icon: IconName,
    title: SharedString,
    subject: SharedString,
    chip: Option<SharedString>,
    added: Option<usize>,
    removed: Option<usize>,
    /// Lines of the code well (command, file body or diff), when there is one.
    input_lines: Option<usize>,
    output_lines: usize,
    has_output: bool,
    /// Output is the point of the tool (commands): show it without a click.
    always_output: bool,
    status: ToolStatus,
    /// The code well's and the output's lines as drawn.
    input_rows: BodyLines,
    output_rows: BodyLines,
}

/// A card body's shown lines, folded and open, cut from its item the first
/// time each is drawn and kept until the item changes (its [`CardSpec`] is
/// rebuilt then).
#[derive(Default)]
struct BodyLines {
    folded: OnceCell<Vec<CodeLine>>,
    open: OnceCell<Vec<CodeLine>>,
}

impl BodyLines {
    fn get(&self, open: bool, cut: impl FnOnce() -> Vec<CodeLine>) -> &[CodeLine] {
        if open {
            self.open.get_or_init(cut)
        } else {
            self.folded.get_or_init(cut)
        }
    }
}

/// One drawn line of a card body: a diff row's sign, line number and text
/// ([`parse_diff_line`]), or plain text.
struct CodeLine {
    sign: char,
    number: SharedString,
    text: SharedString,
}

impl CodeLine {
    /// Empty lines hold a space so they keep their height.
    fn shown(text: &str) -> SharedString {
        if text.is_empty() {
            " ".into()
        } else {
            text.to_string().into()
        }
    }

    fn plain(line: &str) -> Self {
        Self {
            sign: ' ',
            number: SharedString::default(),
            text: Self::shown(line),
        }
    }

    fn diff(line: &str) -> Self {
        let (sign, number, text) = parse_diff_line(line);
        Self {
            sign,
            number: number.to_string().into(),
            text: Self::shown(text),
        }
    }
}

/// A message as its bubble shows it: the Markdown it renders
/// ([`gentle_core::markdown::sent_message`], image tokens as `🖼 #N`) and a
/// long one's folded start. Made once per message: every frame hands the
/// text view the same strings, which it recognizes without parsing again.
struct BubbleText {
    markdown: SharedString,
    /// The first [`USER_LINES`] lines of `markdown` and how many more it
    /// has, when it is long enough to fold. Cut from the whole message's
    /// Markdown, so HTML closed past the fold still shows literally.
    folded: Option<(SharedString, usize)>,
}

impl BubbleText {
    fn new(text: &str) -> Self {
        let markdown = gentle_core::markdown::sent_message(&attachments::chip_text(text));
        let lines = line_count(&markdown);
        let folded = (lines > USER_LINES + USER_SLACK).then(|| {
            let (head, _) = head_slice(&markdown, lines, USER_LINES);
            let head = gentle_core::markdown::trim_folded(head);
            // The blank lines trimmed off its end are hidden too.
            (
                SharedString::from(head.to_string()),
                lines - line_count(head),
            )
        });
        Self {
            markdown: markdown.into(),
            folded,
        }
    }

    /// The Markdown the bubble shows, `open` or not.
    fn visible(&self, open: bool) -> &SharedString {
        match &self.folded {
            Some((head, _)) if !open => head,
            _ => &self.markdown,
        }
    }
}

/// How the shared step row ([`SessionView::step_row`]) styles its text:
/// the quiet collapsed line or the unfolded card's header.
enum StepRowStyle {
    Quiet,
    Card,
}

/// Whether item `ix` renders a step row right after another step row: it
/// is a step ([`step_open_by_default`]) and the first visible item before
/// it is too — invisible assistant messages in between don't count (they
/// render no row). Such rows drop their top padding; the padding stays
/// before and after a run of steps.
fn follows_step(items: &[Item], ix: usize) -> bool {
    let is_step = |item: &Item| gentle_core::transcript::step_open_by_default(item).is_some();
    match items.get(ix) {
        Some(item) if is_step(item) => {}
        _ => return false,
    }
    items[..ix]
        .iter()
        .rev()
        .find(|item| assistant_visible(item))
        .is_some_and(is_step)
}

/// The card data of a tool or shell item.
fn card_spec(item: &Item) -> Option<CardSpec> {
    let (input, output) = card_texts(item);
    let (input_lines, output_lines) = (input.map(|(text, _)| line_count(text)), line_count(output));
    let has_output = !output.trim().is_empty();
    match item {
        Item::Tool {
            name,
            args,
            status,
            details,
            ..
        } => {
            let summary = tools::summarize(name, args, details);
            let arg = |key: &str| args[key].as_str().filter(|s| !s.trim().is_empty());
            let command_like = matches!(
                name.as_str(),
                "bash" | "bash_background" | "monitor" | "codemode"
            );
            let chip = match name.as_str() {
                "bash" | "bash_background" | "monitor" => {
                    Some(command_language(arg("command").unwrap_or_default()).into())
                }
                "codemode" => Some(arg("language").unwrap_or("code").to_string().into()),
                "read" | "edit" | "write" => {
                    language_for_path(arg("path").unwrap_or_default()).map(SharedString::from)
                }
                n if n.starts_with("subagent") => arg("agent").map(|a| a.to_string().into()),
                _ => None,
            };
            let title = match arg("description") {
                Some(description) if command_like => gentle_core::text::first_line(description, 80),
                _ => summary.title,
            };
            // The quiet step line shows what the step was about: the command
            // for command tools, the path for file tools. The card body shows
            // the same thing when the step unfolds.
            let subject = if command_like {
                match input {
                    Some((command, _)) => gentle_core::text::first_line(command, 120),
                    None => summary.subject,
                }
            } else {
                summary.subject
            };
            // A chip that only repeats the title ("bash" next to "Bash") is
            // noise; one that adds information stays.
            let chip = chip.filter(|chip| !chip.eq_ignore_ascii_case(&title));
            Some(CardSpec {
                icon: tool_icon(name),
                title: title.into(),
                subject: subject.into(),
                chip,
                added: summary.added,
                removed: summary.removed,
                input_lines,
                output_lines,
                has_output,
                always_output: command_like,
                status: *status,
                input_rows: BodyLines::default(),
                output_rows: BodyLines::default(),
            })
        }
        Item::Shell {
            command,
            exit_code,
            running,
            ..
        } => Some(CardSpec {
            icon: IconName::CodeXml,
            title: "Shell".into(),
            subject: SharedString::default(),
            chip: Some(command_language(command).into()),
            added: None,
            removed: None,
            input_lines,
            output_lines,
            has_output,
            always_output: true,
            status: match (running, exit_code) {
                (true, _) => ToolStatus::Running,
                (false, Some(0)) => ToolStatus::Done,
                _ => ToolStatus::Failed,
            },
            input_rows: BodyLines::default(),
            output_rows: BodyLines::default(),
        }),
        _ => None,
    }
}

/// A card's text bodies, borrowed from its item: the code well (and
/// whether it is a diff) and the output.
fn card_texts(item: &Item) -> (Option<(&str, bool)>, &str) {
    match item {
        Item::Tool {
            name,
            args,
            status,
            output,
            details,
            ..
        } => {
            let arg = |key: &str| args[key].as_str().filter(|s| !s.trim().is_empty());
            let input = match name.as_str() {
                "bash" | "bash_background" | "monitor" => arg("command").map(|c| (c, false)),
                "codemode" => arg("code").or(arg("command")).map(|c| (c, false)),
                "write" => arg("content").map(|c| (c, false)),
                "edit" => details["diff"].as_str().map(|d| (d, true)),
                _ => None,
            };
            // A file tool's result is a one-line receipt; only failures say more.
            let output = if matches!(name.as_str(), "edit" | "write")
                && input.is_some()
                && *status != ToolStatus::Failed
            {
                ""
            } else {
                output.as_str()
            };
            (input, output)
        }
        Item::Shell {
            command, output, ..
        } => (Some((command.as_str(), false)), output.as_str()),
        _ => (None, ""),
    }
}

/// A step's icon tint by kind, so a glance tells commands, reads, edits,
/// helpers and the web apart without reading: commands in champagne,
/// reading and searching in info, edits in teal, helpers in purple, the
/// web in the accent, anything else muted.
fn step_tone(icon: IconName) -> Hsla {
    match icon {
        IconName::CodeXml => ui::champagne(),
        IconName::FileText | IconName::Search | IconName::Folder => ui::info(),
        IconName::FilePen => ui::teal(),
        IconName::Users => ui::purple(),
        IconName::Globe => ui::accent(),
        _ => ui::muted(),
    }
}

fn tool_icon(name: &str) -> IconName {
    match name {
        "bash" | "bash_background" | "monitor" | "codemode" => IconName::CodeXml,
        "read" => IconName::FileText,
        "edit" | "write" => IconName::FilePen,
        "grep" | "find" => IconName::Search,
        "ls" => IconName::Folder,
        "todo" => IconName::ListTodo,
        "screen" => IconName::Monitor,
        n if n.starts_with("subagent") => IconName::Users,
        n if n.starts_with("web")
            || n.contains("fetch")
            || n.contains("browser")
            || n.contains("search") =>
        {
            IconName::Globe
        }
        _ => IconName::Wrench,
    }
}

/// The icon a run's summary line shows for a kind of step: the one its
/// steps' own lines show ([`tool_icon`]).
fn kind_icon(kind: StepKind) -> IconName {
    match kind {
        StepKind::Read => IconName::FileText,
        StepKind::Search => IconName::Search,
        StepKind::Edit => IconName::FilePen,
        StepKind::Command => IconName::CodeXml,
        StepKind::Delegate => IconName::Users,
        StepKind::Web => IconName::Globe,
        StepKind::Other => IconName::Wrench,
    }
}

fn language_for_path(path: &str) -> Option<&'static str> {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let (_, ext) = file.rsplit_once('.')?;
    Some(match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "md" | "mdx" => "markdown",
        "json" | "jsonl" | "jsonc" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "sh" | "bash" | "zsh" | "fish" => "bash",
        "go" => "go",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "java" => "java",
        "rb" => "ruby",
        "lua" => "lua",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "c++",
        "cs" => "c#",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "sql" => "sql",
        _ => return None,
    })
}

/// The interpreter a shell command runs, for the card chip.
fn command_language(command: &str) -> &'static str {
    let program = command.split_whitespace().next().unwrap_or_default();
    let program = program.rsplit('/').next().unwrap_or(program);
    match program {
        p if p.starts_with("python") => "python",
        "node" | "bun" | "deno" => "javascript",
        "ruby" => "ruby",
        _ => "bash",
    }
}

/// Pi's edit diff lines: `+12 text`, `-3 text`, ` 4 ctx` → (sign, number, text).
fn parse_diff_line(line: &str) -> (char, &str, &str) {
    let sign = match line.as_bytes().first() {
        Some(b'+') => '+',
        Some(b'-') => '-',
        Some(b' ') => ' ',
        _ => return (' ', "", line),
    };
    let rest = &line[1..];
    let trimmed = rest.trim_start_matches(' ');
    let digits = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits == 0 {
        return (sign, "", rest);
    }
    let text = &trimmed[digits..];
    (
        sign,
        &trimmed[..digits],
        text.strip_prefix(' ').unwrap_or(text),
    )
}

fn diff_row(line: &CodeLine) -> Div {
    let sign = line.sign;
    let (color, bg, glyph) = match sign {
        '+' => (ui::success(), Some(ui::success().opacity(0.08)), "+"),
        '-' => (ui::danger(), Some(ui::danger().opacity(0.08)), "−"),
        _ => (ui::muted(), None, " "),
    };
    h_flex()
        .w_full()
        .px_3()
        .gap_2()
        .items_start()
        .when_some(bg, |this, bg| this.bg(bg))
        .child(
            div()
                .w(rems(32. / 16.))
                .flex_shrink_0()
                .text_right()
                .text_color(ui::faint())
                .child(line.number.clone()),
        )
        .child(
            div()
                .w(rems(10. / 16.))
                .flex_shrink_0()
                .text_color(color)
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_normal()
                .text_color(if sign == ' ' { ui::muted() } else { ui::fg() })
                .child(line.text.clone()),
        )
}

/// How many lines `str::lines` reads in `text` with its trailing whitespace trimmed.
fn line_count(text: &str) -> usize {
    let text = text.trim_end();
    if text.is_empty() {
        0
    } else {
        text.bytes().filter(|&b| b == b'\n').count() + 1
    }
}

/// The first `max` of the `lines` lines of `text` ([`line_count`]) as one
/// slice, plus how many were hidden. Read it back with `str::lines`.
fn head_slice(text: &str, lines: usize, max: usize) -> (&str, usize) {
    let text = text.trim_end();
    if lines <= max {
        return (text, 0);
    }
    if max == 0 {
        return ("", lines);
    }
    let end = text
        .match_indices('\n')
        .nth(max - 1)
        .map_or(text.len(), |(at, _)| at);
    let head = &text[..end];
    (head.strip_suffix('\r').unwrap_or(head), lines - max)
}

/// The last `max` of the `lines` lines of `text` ([`line_count`]) as one
/// slice, plus how many were hidden. Read it back with `str::lines`.
fn tail_slice(text: &str, lines: usize, max: usize) -> (&str, usize) {
    let text = text.trim_end();
    if lines <= max {
        return (text, 0);
    }
    let hidden = lines - max;
    if max == 0 {
        return ("", hidden);
    }
    // From the end: the output can be long and only its tail shows.
    let start = text
        .rmatch_indices('\n')
        .nth(max - 1)
        .map_or(0, |(at, _)| at + 1);
    (&text[start..], hidden)
}

fn clock(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// A tool card's last line: how it ended, or "Running" while it runs.
fn status_footer(status: ToolStatus, duration: Option<Duration>) -> Div {
    let (icon, label, color) = match status {
        ToolStatus::Running => (None, "Running", ui::info()),
        ToolStatus::Done => (Some(IconName::Check), "Done", ui::success()),
        ToolStatus::Failed => (Some(IconName::X), "Failed", ui::danger()),
    };
    h_flex()
        .px_3()
        .py_1p5()
        .gap_1p5()
        .text_size(rems(12. / 16.))
        .text_color(color)
        .when_some(icon, |this, icon| this.child(Icon::new(icon).xsmall()))
        .child(label)
        .when_some(duration, |this, d| {
            this.child(
                div()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(11.))
                    .text_color(ui::faint())
                    .child(ui::format_duration(d.as_millis() as u64)),
            )
        })
}

/// A user message: its images (numbered like their `[Image #N]` tokens)
/// over the text, tokens shown as `🖼 #N`, a long text folded unless
/// `open`, with its `toggle`. `None` is an image that could not be decoded.
/// The text is selectable and copies with the platform shortcut, like an
/// answer's; `ix` keys its selection. Images its Markdown shows load only
/// from `data:` URLs (decoded into `embedded`) and local files: one linked
/// on the network shows nothing, so a message fetches nothing.
fn user_bubble(
    ix: usize,
    shown: &BubbleText,
    images: Vec<Option<Arc<Image>>>,
    open: bool,
    toggle: Option<Stateful<Div>>,
    embedded: &Arc<crate::features::files::EmbeddedImages>,
) -> Div {
    let markdown = shown.visible(open);
    // min_w_0 on both levels: without it the bubble's minimum width is its
    // unwrapped text, so in a narrow pane it overflows (clipped on the left).
    h_flex().w_full().min_w_0().justify_end().child(
        v_flex()
            .min_w_0()
            .max_w(rems(560. / 16.))
            .gap_2()
            .px_4()
            .py_2p5()
            .rounded_2xl()
            .bg(ui::raised())
            .text_size(rems(14. / 16.))
            .text_color(ui::fg())
            .when(!images.is_empty(), |this| {
                this.child(h_flex().flex_wrap().gap_2().children(
                    images.into_iter().enumerate().map(|(ix, image)| {
                        image_tile(image, rems(160. / 16.), rems(120. / 16.))
                            .child(number_badge(ix + 1))
                    }),
                ))
            })
            .when(!markdown.trim().is_empty(), |this| {
                this.child(
                    div()
                        .debug_selector(move || format!("user-text-{ix}"))
                        .child(
                            // The answers' prose style; the bubble's own
                            // text size and color, inherited.
                            TextView::markdown(("user-text", ix), markdown.clone())
                                .selectable(true)
                                .style(ui::prose_style())
                                .list_style(ui::prose_list_style())
                                .image_source({
                                    let embedded = embedded.clone();
                                    move |uri| {
                                        crate::features::files::offline_image(&embedded, uri)
                                            .unwrap_or_else(no_image)
                                    }
                                }),
                        ),
                )
            })
            // The toggle's own padding, undone so its label lines up with the text.
            .children(
                toggle.map(|toggle| div().ml(rems(-12. / 16.)).mt(rems(-4. / 16.)).child(toggle)),
            ),
    )
}

/// An image source that fails at once, without loading anything: the
/// image shows nothing.
fn no_image() -> ImageSource {
    ImageSource::Custom(Arc::new(|_, _| {
        Some(Err(ImageCacheError::Asset("not loaded".into())))
    }))
}

/// A rounded, cropped image (or a placeholder) for attachment strips.
fn image_tile(image: Option<Arc<Image>>, width: Rems, height: Rems) -> Div {
    div()
        .relative()
        .flex_shrink_0()
        .w(width)
        .h(height)
        .rounded_lg()
        .overflow_hidden()
        .border_1()
        .border_color(ui::border_strong())
        .bg(ui::well())
        .child(match image {
            Some(image) => img(image)
                .size_full()
                .rounded_lg()
                .object_fit(ObjectFit::Cover)
                .into_any_element(),
            None => h_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Icon::new(IconName::Image).text_color(ui::faint()))
                .into_any_element(),
        })
}

/// `#N` in an image's lower-left corner, matching its `[Image #N]` token.
fn number_badge(n: usize) -> Div {
    div()
        .absolute()
        .left(rems(3. / 16.))
        .bottom(rems(3. / 16.))
        .px_1()
        .rounded_sm()
        .bg(ui::well())
        .font_family(ui::mono())
        .text_size(ui::code_rems(10.))
        .text_color(ui::info())
        .child(format!("#{n}"))
}

fn notice_card(level: NoticeLevel, text: &str) -> Div {
    let (icon, color) = match level {
        NoticeLevel::Info => (IconName::Info, ui::muted()),
        NoticeLevel::Warning => (IconName::TriangleAlert, ui::warning()),
        NoticeLevel::Error => (IconName::CircleX, ui::danger()),
    };
    ui::card()
        .flex_row()
        .w_full()
        .items_start()
        .gap_2()
        .px_3()
        .py_2()
        .when(level != NoticeLevel::Info, |this| {
            this.border_color(color.opacity(0.35))
        })
        .child(
            div()
                .pt(px(2.))
                .child(Icon::new(icon).xsmall().text_color(color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(rems(12. / 16.))
                .text_color(if level == NoticeLevel::Info {
                    ui::muted()
                } else {
                    color
                })
                .whitespace_normal()
                .child(text.to_string()),
        )
}

/// Along the top of a fenced block in an answer, after Tern's: its
/// language, then Copy, which puts the code on the clipboard and says so.
/// In the interface's font, not the code's.
fn code_block_header(block: &gpui_kit::base::text::CodeBlock, cx: &App) -> Div {
    use gpui_kit::component::WindowExt as _;
    let code = block.code();
    h_flex()
        .gap_3()
        .px_1()
        .font_family(cx.theme().font_family.clone())
        .text_size(rems(12. / 16.))
        .text_color(ui::muted())
        .children(block.lang().filter(|lang| !lang.is_empty()))
        .child(
            div()
                .id("copy")
                .cursor_pointer()
                .hover(|s| s.text_color(ui::fg()))
                .child("Copy")
                .on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                    window.push_notification(
                        gpui_kit::component::notification::Notification::success(
                            "Copied to the clipboard.",
                        ),
                        cx,
                    );
                }),
        )
}

/// The petal-and-shimmer "agent is on it" row, its clock counting up once
/// a second. With the composer's orbit, the turn's only moving indicators:
/// the petal and the label move on the live overlay, under `key`.
fn working_indicator(
    slots: &LiveSlots,
    key: impl Into<ElementId>,
    label: &'static str,
    elapsed: Option<Duration>,
) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .child(slots.slot(key, move || {
            h_flex()
                .gap_2()
                .items_center()
                .child(ui::pulse(
                    div()
                        .text_size(rems(15. / 16.))
                        .text_color(ui::petal())
                        .child("✿"),
                ))
                .child(
                    ui::shimmer(label, ui::muted(), ui::fg())
                        .text_size(rems(13. / 16.))
                        .italic(),
                )
                .into_any_element()
        }))
        .when_some(elapsed, |this, d| {
            this.child(
                div()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(11.))
                    .text_color(ui::faint())
                    .child(clock(d)),
            )
            .child(ui::each_second())
        })
}

#[cfg(test)]
mod tests {
    use super::{
        BubbleText, USER_LINES, USER_SLACK, command_language, follows_step, head_slice,
        language_for_path, line_count, parse_diff_line, tail_slice,
    };
    use gentle_core::transcript::{Block, Item, ToolStatus};
    use gpui_kit::AppContext as _;

    struct CardWrapFixture(gpui_kit::Entity<super::SessionView>);

    impl gpui_kit::Render for CardWrapFixture {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};

            let long = "unbroken".repeat(20);
            self.0.update(cx, |view, cx| {
                let well =
                    |name: &'static str,
                     body: &str,
                     diff: bool,
                     cx: &mut gpui_kit::Context<super::SessionView>| {
                        gpui_kit::div()
                            .w(gpui_kit::px(200.))
                            .debug_selector(move || name.into())
                            .child(view.code_well(
                                0,
                                body,
                                1,
                                diff,
                                &super::BodyLines::default(),
                                cx,
                            ))
                    };
                let output =
                    |name: &'static str,
                     body: &str,
                     cx: &mut gpui_kit::Context<super::SessionView>| {
                        gpui_kit::div()
                            .w(gpui_kit::px(200.))
                            .debug_selector(move || name.into())
                            .child(view.output_block(0, body, 1, &super::BodyLines::default(), cx))
                    };
                gpui_kit::component::v_flex()
                    .child(well("short-input", "short", false, cx))
                    .child(well("long-input", &long, false, cx))
                    .child(output("short-output", "short", cx))
                    .child(output("long-output", &long, cx))
                    .child(well("short-diff", "+1 short", true, cx))
                    .child(well("long-diff", &format!("+1 {long}"), true, cx))
            })
        }
    }

    #[gpui::test]
    fn tool_card_wrap_unbroken_input_output_and_diff(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    super::SessionView::new(std::env::temp_dir(), None, window, cx, false);
                view.backend.started = true;
                view
            });
            CardWrapFixture(view)
        });
        cx.run_until_parked();
        for (short, long) in [
            ("short-input", "long-input"),
            ("short-output", "long-output"),
            ("short-diff", "long-diff"),
        ] {
            let short_height = cx.debug_bounds(short).expect(short).size.height;
            let long_height = cx.debug_bounds(long).expect(long).size.height;
            assert!(
                long_height > short_height,
                "{long} must wrap: {long_height:?} vs {short_height:?}"
            );
        }
    }

    #[gpui::test]
    fn output_wrap_padding(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let mut view =
                    super::SessionView::new(std::env::temp_dir(), None, window, cx, false);
                view.backend.started = true;
                view
            });
            OutputPaddingFixture(view)
        });
        cx.run_until_parked();
        let outer = cx.debug_bounds("output-wrap-padding-outer").unwrap();
        let row = cx.debug_bounds("output-wrap-padding-row").unwrap();
        assert!(row.size.height > gpui_kit::px(20.), "output must wrap");
        assert_eq!(row.origin.x - outer.origin.x, gpui_kit::px(12.));
        assert_eq!(
            outer.origin.x + outer.size.width - (row.origin.x + row.size.width),
            gpui_kit::px(12.),
            "output row must leave the right padding clear"
        );
    }

    struct OutputPaddingFixture(gpui_kit::Entity<super::SessionView>);

    impl gpui_kit::Render for OutputPaddingFixture {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            cx: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};

            self.0.update(cx, |view, cx| {
                gpui_kit::div()
                    .w(gpui_kit::px(200.))
                    .debug_selector(|| "output-wrap-padding-outer".into())
                    .child(view.output_block(
                        0,
                        &"0".repeat(160),
                        1,
                        &super::BodyLines::default(),
                        cx,
                    ))
            })
        }
    }

    #[gpui::test]
    fn user_bubble_selection_changes_and_text_updates(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{
            AppContext as _, InteractiveElement as _, Modifiers, MouseButton, ParentElement as _,
            Render, Styled as _, base::TextSelection, point, px,
        };

        struct Bubble {
            text: &'static str,
            focus: gpui_kit::FocusHandle,
        }
        impl Render for Bubble {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                gpui_kit::div()
                    .id("focus")
                    .track_focus(&self.focus)
                    .size_full()
                    .child(super::user_bubble(
                        0,
                        &super::BubbleText::new(self.text),
                        Vec::new(),
                        true,
                        None,
                        &Default::default(),
                    ))
            }
        }
        cx.update(gpui_kit::init);
        let (root, cx) = cx.add_window_view(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            let view = cx.new(|_| Bubble {
                text: "hello `café` world",
                focus,
            });
            gpui_kit::base::Root::new(view, window, cx)
        });
        let view = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Bubble>().unwrap()
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("user-text-0").unwrap();
        let start = point(bounds.left() + px(1.), bounds.center().y);
        let end = point(bounds.right() - px(1.), bounds.center().y);
        let drag = |cx: &mut gpui_kit::VisualTestContext, end| {
            cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
            cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
            cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        };
        let copy = |cx: &mut gpui_kit::VisualTestContext| {
            cx.update(|_, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("sentinel".into()))
            });
            cx.simulate_keystrokes(if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-c"
            });
            cx.run_until_parked();
            cx.update(|_, cx| {
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .unwrap_or_default()
            })
        };
        drag(cx, end);
        // TextView's selection API includes a block separator; the copy action
        // omits that separator from the clipboard.
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            assert_eq!(
                TextSelection::selected_text(window, cx),
                "hello café world\n"
            );
        });
        assert_eq!(copy(cx), "hello café world");
        drag(cx, point(bounds.left() + px(35.), bounds.center().y));
        let partial = cx.update(TextSelection::selected_text);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let after = cx.update(TextSelection::selected_text);
        assert_eq!(partial, after);
        assert_eq!(after, "hell\n");
        assert_eq!(copy(cx), "hell");
        view.update(cx, |view, cx| {
            view.text = "replacement `é` text";
            cx.notify();
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            assert_eq!(TextSelection::selected_text(window, cx), "");
        });
        cx.update(TextSelection::clear);
        cx.update(|window, cx| assert_eq!(TextSelection::selected_text(window, cx), ""));
        assert_eq!(copy(cx), "sentinel");
        cx.run_until_parked();
        let replacement = cx.debug_bounds("user-text-0").unwrap();
        let replacement_start = point(replacement.left() + px(1.), replacement.center().y);
        let replacement_end = point(replacement.right() - px(1.), replacement.center().y);
        cx.simulate_mouse_down(replacement_start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(replacement_end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(replacement_end, MouseButton::Left, Modifiers::none());
        assert_eq!(copy(cx), "replacement é text");
    }

    #[gpui::test]
    fn user_bubble_wrapped_selection_copies_visible_text(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{
            Modifiers, MouseButton, ParentElement as _, Render, Styled as _, base::TextSelection,
            point, px,
        };
        const TEXT: &str =
            "first `café code followed by enough words to wrap onto several lines` end";
        struct Bubble;
        impl Render for Bubble {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                gpui_kit::div()
                    .size_full()
                    .child(gpui_kit::base::TextSelectionLayer)
                    .child(gpui_kit::div().w(px(180.)).child(super::user_bubble(
                        0,
                        &super::BubbleText::new(TEXT),
                        Vec::new(),
                        true,
                        None,
                        &Default::default(),
                    )))
            }
        }
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Bubble);
        cx.run_until_parked();
        let bounds = cx.debug_bounds("user-text-0").unwrap();
        assert!(bounds.size.height > px(30.), "fixture must wrap");
        let start = point(bounds.left() + px(1.), bounds.top() + px(5.));
        let end = point(bounds.right() - px(1.), bounds.bottom() - px(5.));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            assert_eq!(
                TextSelection::selected_text(window, cx),
                "first café code followed by enough words to wrap onto several lines end\n"
            );
        });
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()),
            "first café code followed by enough words to wrap onto several lines end"
        );
    }

    #[gpui::test]
    fn user_bubble_backticks_paint_code_background(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::{ParentElement as _, Render, Styled as _};

        struct Bubble;
        impl Render for Bubble {
            fn render(
                &mut self,
                _: &mut gpui_kit::Window,
                _: &mut gpui_kit::Context<Self>,
            ) -> impl gpui_kit::IntoElement {
                gpui_kit::div()
                    .size_full()
                    .child(gpui_kit::base::TextSelectionLayer)
                    .child(super::user_bubble(
                        0,
                        &super::BubbleText::new("hello `café` world"),
                        Vec::new(),
                        true,
                        None,
                        &Default::default(),
                    ))
            }
        }
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Bubble);
        cx.run_until_parked();
        // The answers' inline code backing, not a style of the bubble's own.
        let backing: gpui_kit::Background = crate::shared::ui::prose_style()
            .inline_code
            .background_color
            .expect("prose code has a backing")
            .into();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
            assert!(
                window
                    .painted_quads()
                    .into_iter()
                    .any(|quad| quad.background == backing),
                "inline code backing must be painted"
            );
        });
    }

    /// What a card shows from its data, comparable.
    type Shown = (
        gpui_kit::assets::IconName,
        String,
        String,
        Option<String>,
        Option<usize>,
        Option<usize>,
        Option<usize>,
        usize,
        bool,
        bool,
        ToolStatus,
    );

    fn shown(spec: &super::CardSpec) -> Shown {
        (
            spec.icon,
            spec.title.to_string(),
            spec.subject.to_string(),
            spec.chip.as_ref().map(|chip| chip.to_string()),
            spec.added,
            spec.removed,
            spec.input_lines,
            spec.output_lines,
            spec.has_output,
            spec.always_output,
            spec.status,
        )
    }

    /// Item `ix`'s card data, if it was built yet.
    fn built(view: &super::SessionView, ix: usize) -> Option<Option<Shown>> {
        match view.list.prepared(ix) {
            Some(super::Prepared::Card(cell)) => cell.get().map(|spec| spec.as_ref().map(shown)),
            _ => panic!("item {ix} has no card"),
        }
    }

    /// A loaded conversation builds a tool or shell card's data when its row
    /// first draws, not when the conversation loads.
    #[gpui::test]
    fn a_cards_data_is_built_when_its_row_first_draws(cx: &mut gpui_kit::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let mut view = super::SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view
        });
        let (tool, shell) = (2, 3);
        view.update(cx, |view, cx| {
            view.transcript.load_messages(&[
                serde_json::json!({"role": "user", "content": "run them"}),
                serde_json::json!({"role": "assistant", "stopReason": "toolUse", "content": [
                    {"type": "toolCall", "id": "t1", "name": "bash",
                     "arguments": {"command": "cargo test", "description": "Run the tests"}},
                ]}),
                serde_json::json!({"role": "toolResult", "toolCallId": "t1", "isError": false,
                    "content": [{"type": "text", "text": "ok\nok\nok"}], "details": {}}),
                serde_json::json!({"role": "bashExecution", "command": "git status",
                    "output": "clean", "exitCode": 0}),
            ]);
            view.sync_view(cx);
            assert_eq!(built(view, tool), None, "not built when loaded");
            assert_eq!(built(view, shell), None, "not built when loaded");
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            for ix in [tool, shell] {
                let expected = super::card_spec(&view.transcript.items[ix]).map(|s| shown(&s));
                assert!(expected.is_some());
                assert_eq!(
                    built(view, ix),
                    Some(expected),
                    "item {ix} built when drawn"
                );
            }
        });
    }

    fn step(name: &str, status: ToolStatus) -> Item {
        Item::Tool {
            call_id: format!("{name}-1"),
            name: name.into(),
            args: serde_json::json!({}),
            status,
            output: String::new(),
            images: Vec::new(),
            details: serde_json::json!({}),
            duration_ms: None,
        }
    }

    fn textless() -> Item {
        Item::Assistant {
            blocks: vec![Block::ToolCall],
            streaming: false,
            error: None,
        }
    }

    fn text(text: &str) -> Item {
        Item::Assistant {
            blocks: vec![Block::Text(text.into())],
            streaming: false,
            error: None,
        }
    }

    #[test]
    fn consecutive_step_rows_sit_close_together() {
        let items = vec![
            text("A"),
            step("bash", ToolStatus::Done),
            textless(),
            step("read", ToolStatus::Done),
            step("edit", ToolStatus::Done),
            text("B"),
        ];
        // The first step after text keeps its padding; the steps that follow
        // it (even across an invisible assistant message) sit close.
        assert!(!follows_step(&items, 1));
        assert!(follows_step(&items, 3));
        assert!(follows_step(&items, 4));
        // The run's end gets its padding back.
        assert!(!follows_step(&items, 5));
        // What you ran yourself is not a step row.
        let shell = vec![
            step("bash", ToolStatus::Done),
            Item::Shell {
                command: "ls".into(),
                output: String::new(),
                exit_code: Some(0),
                running: false,
            },
        ];
        assert!(!follows_step(&shell, 1));
    }

    #[test]
    fn long_messages_fold_to_their_first_lines() {
        let message = |n: usize, end: &str| {
            (1..=n)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join(end)
        };
        // Folding would hide only USER_SLACK lines: it doesn't.
        assert!(
            BubbleText::new(&message(USER_LINES + USER_SLACK, "\n"))
                .folded
                .is_none()
        );
        // Trailing blank lines are not lines to hide.
        let padded = format!("{}\n\n\n\n", message(USER_LINES + USER_SLACK, "\n"));
        assert!(BubbleText::new(&padded).folded.is_none());

        let long = message(USER_LINES + USER_SLACK + 1, "\r\n");
        let bubble = BubbleText::new(&long);
        let (head, hidden) = bubble.folded.clone().unwrap();
        assert_eq!(hidden, USER_SLACK + 1);
        assert_eq!(
            head.lines().map(str::trim_end).collect::<Vec<_>>(),
            message(USER_LINES, "\n").lines().collect::<Vec<_>>()
        );
        assert!(!head.ends_with('\r'));
        // Open, the whole text shows; folded, the head.
        assert!(bubble.visible(true).starts_with(head.as_ref()));
        assert_eq!(bubble.visible(false), &head);

        // The head is cut from the whole message's Markdown: a tag closed
        // past the fold still shows literally.
        let html = format!("<div>\n{}\n</div>", message(USER_LINES + USER_SLACK, "\n"));
        let (head, _) = BubbleText::new(&html).folded.unwrap();
        assert!(head.starts_with(r"\<div\>"), "{head}");
        // A blank line inside it, last before the fold, is no stray `\`:
        // it is hidden with the rest.
        let gap = format!(
            "<div>\n{}\n\n{}\n</div>",
            message(USER_LINES - 2, "\n"),
            message(USER_SLACK + 2, "\n")
        );
        let (head, hidden) = BubbleText::new(&gap).folded.unwrap();
        assert!(head.ends_with("line 6"), "{head}");
        assert_eq!(hidden, USER_SLACK + 4);
    }

    #[test]
    fn diff_lines_split_sign_number_and_text() {
        assert_eq!(parse_diff_line("+12 text"), ('+', "12", "text"));
        assert_eq!(parse_diff_line("-  3 old line"), ('-', "3", "old line"));
        assert_eq!(parse_diff_line("  4 ctx"), (' ', "4", "ctx"));
        assert_eq!(parse_diff_line("+7 "), ('+', "7", ""));
        assert_eq!(parse_diff_line("..."), (' ', "", "..."));
    }

    #[test]
    fn head_and_tail_slices_match_line_splitting() {
        // What the old helpers' joined strings read back as (`str::lines`).
        fn head(text: &str, max: usize) -> (Vec<&str>, usize) {
            let lines: Vec<&str> = text.trim_end().lines().collect();
            let hidden = lines.len().saturating_sub(max);
            let read = lines[..lines.len() - hidden].join("\n").lines().count();
            (lines[..read].to_vec(), hidden)
        }
        fn tail(text: &str, max: usize) -> (Vec<&str>, usize) {
            let lines: Vec<&str> = text.trim_end().lines().collect();
            let hidden = lines.len().saturating_sub(max);
            let joined = lines[hidden..].join("\n");
            let read = joined.lines().count();
            (lines[hidden..hidden + read].to_vec(), hidden)
        }
        let texts = [
            "",
            "  \n",
            "a",
            "a\nb\nc\n",
            "a\n\nb\n\n\n",
            "one\r\ntwo\r\nthree\r\n",
            "x\ny\nz\nw\nv",
        ];
        for text in texts {
            let lines = line_count(text);
            assert_eq!(lines, text.trim_end().lines().count(), "{text:?}");
            for max in 0..7 {
                let (shown, hidden) = head_slice(text, lines, max);
                let shown: Vec<&str> = shown.lines().collect();
                assert_eq!((shown, hidden), head(text, max), "{text:?} {max}");
                let (shown, hidden) = tail_slice(text, lines, max);
                let shown: Vec<&str> = shown.lines().collect();
                assert_eq!((shown, hidden), tail(text, max), "{text:?} {max}");
            }
        }
        assert_eq!(head_slice("a\nb\nc\n", 3, 2), ("a\nb", 1));
        assert_eq!(tail_slice("a\nb\nc\n", 3, 2), ("b\nc", 1));
    }

    #[test]
    fn languages_come_from_paths_and_commands() {
        assert_eq!(language_for_path("crates/app/src/ui.rs"), Some("rust"));
        assert_eq!(language_for_path("v1.2/Makefile"), None);
        assert_eq!(command_language("python3 - <<'EOF'"), "python");
        assert_eq!(command_language("/usr/bin/node x.js"), "javascript");
        assert_eq!(command_language("cargo test"), "bash");
    }
}
