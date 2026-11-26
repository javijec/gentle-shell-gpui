//! The sequence kind on the shared canvas: participant cards (in their
//! tone, with an icon badge) on top of dashed lifelines, messages as
//! chevron arrows down the page (sync solid, async dotted, returns dashed,
//! self calls looping), fragments as tagged containers and activation
//! bars. Drag a header sideways or a message up and down to
//! reorder, drag from one lifeline to another to add a message,
//! double-click to rename; the details island changes a message's kind.

use std::rc::Rc;

use gentle_core::genui::edits::{Editable as _, Edits};
use gentle_core::genui::layout::{Cubic, EdgePath, Point as P, Rect};
use gentle_core::genui::sequence::{
    self as seq, Change, HEAD_H, Message, MessageKind, Participant, ParticipantKind, SELF_LOOP,
    Sequence, SequenceLayout,
};
use gentle_core::genui::style::Tint;
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::canvas::{self, Canvas, Drawn, Head, Shape, moved};
use super::look;
use super::panel::{Body, PanelView};
use crate::shared::ui;

pub(super) enum SeqDrag {
    /// A participant's header moving sideways (`x`: the pointer, diagram units).
    Participant {
        id: String,
        from: Point<Pixels>,
        x: f32,
        moved: bool,
    },
    /// A message moving up or down.
    Message {
        id: String,
        from: Point<Pixels>,
        y: f32,
        moved: bool,
    },
    /// A new message drawn from `lane`'s lifeline at `start`.
    Connect {
        lane: usize,
        from: Point<Pixels>,
        start: P,
        to: P,
        moved: bool,
    },
}

pub(super) struct SequenceState {
    pub edits: Edits<Sequence>,
    pub canvas: Canvas,
    /// `p:<id>` or `m:<id>`.
    pub selected: Option<String>,
    pub drag: Option<SeqDrag>,
    /// What was laid out last and its layout.
    laid: Option<(Sequence, Rc<SequenceLayout>)>,
}

impl SequenceState {
    pub fn new(spec: Sequence) -> Self {
        Self {
            edits: Edits::new(spec),
            canvas: Canvas::new(),
            selected: None,
            drag: None,
            laid: None,
        }
    }

    pub fn rebase(&mut self, spec: Sequence) {
        self.edits.rebase(spec);
        self.drag = None;
        let working = self.edits.working();
        let gone = match self.selected.as_deref() {
            Some(key) => match key.split_once(':') {
                Some(("p", id)) => working.participant(id).is_none(),
                Some((_, id)) => working.message(id).is_none(),
                None => true,
            },
            None => false,
        };
        if gone {
            self.selected = None;
        }
    }

    /// The working spec, with a drag's reorder previewed.
    fn shown(&self, layout: &SequenceLayout) -> Sequence {
        let working = self.edits.working();
        let mut shown = working.clone();
        match &self.drag {
            Some(SeqDrag::Participant {
                id, x, moved: true, ..
            }) => {
                let order = Sequence::moved(
                    working.participants.iter().map(|p| p.id.as_str()),
                    id,
                    layout.lane_at(*x),
                );
                let _ = shown.apply(&Change::OrderParticipants {
                    participants: order,
                });
            }
            Some(SeqDrag::Message {
                id, y, moved: true, ..
            }) => {
                let index = layout.insert_at(*y, working.index(id));
                let order =
                    Sequence::moved(working.messages.iter().map(|m| m.id.as_str()), id, index);
                let _ = shown.apply(&Change::OrderMessages { messages: order });
            }
            _ => {}
        }
        shown
    }

    /// The layout of `preview`, else of the working spec.
    fn layout_of(&mut self, preview: Option<&Sequence>) -> Rc<SequenceLayout> {
        let spec = preview.unwrap_or(self.edits.working());
        match &self.laid {
            Some((laid, layout)) if laid == spec => layout.clone(),
            _ => {
                let layout = Rc::new(seq::layout(spec));
                self.laid = Some((spec.clone(), layout.clone()));
                layout
            }
        }
    }
}

fn state(this: &mut PanelView) -> Option<&mut SequenceState> {
    match &mut this.body {
        Some(Body::Sequence(s)) => Some(s),
        _ => None,
    }
}

fn kind_icon(kind: ParticipantKind) -> IconName {
    match kind {
        ParticipantKind::Actor => IconName::User,
        ParticipantKind::Service => IconName::Server,
        ParticipantKind::Db => IconName::Database,
        ParticipantKind::Queue => IconName::Layers,
        ParticipantKind::External => IconName::Globe,
    }
}

/// A kind's tone when the participant sets no `color`.
fn kind_color(kind: ParticipantKind) -> Hsla {
    ui::tint(match kind {
        ParticipantKind::Actor => Tint::Pink,
        ParticipantKind::Service => Tint::Blue,
        ParticipantKind::Db => Tint::Green,
        ParticipantKind::Queue => Tint::Yellow,
        ParticipantKind::External => Tint::Gray,
    })
}

/// A participant's tone (its `color`, else its kind's) and icon (its
/// `icon`, else its kind's): its card, lifeline and activation bars wear
/// them.
fn participant_tone(p: &Participant) -> Hsla {
    look::tone(p.color, kind_color(p.kind))
}

fn participant_icon(p: &Participant) -> IconName {
    p.icon.and_then(look::icon).unwrap_or(kind_icon(p.kind))
}

fn fragment_name(kind: seq::FragmentKind) -> &'static str {
    kind.name()
}

/// Where a message's label and rename field sit.
fn label_rect(layout: &SequenceLayout, index: usize) -> Rect {
    let p = layout.messages[index].label;
    Rect {
        x: p.x - 80.,
        y: p.y - 12.,
        w: 160.,
        h: 24.,
    }
}

impl PanelView {
    fn sequence(&self) -> Option<&SequenceState> {
        match &self.body {
            Some(Body::Sequence(s)) => Some(s),
            _ => None,
        }
    }

    fn sequence_edit(&mut self, change: Change, cx: &mut Context<Self>) -> bool {
        self.edit_with(cx, |body| match body {
            Body::Sequence(s) => Some(s.edits.edit(&change)),
            _ => None,
        })
    }

    pub(super) fn render_sequence(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rem = window.rem_size();
        let Some(s) = state(self) else {
            return div().into_any_element();
        };
        // A drag previews its reorder on the layout without it; otherwise
        // the working spec is shown as is.
        let preview = s
            .drag
            .as_ref()
            .map(|_| s.shown(&seq::layout(s.edits.working())));
        let l = s.layout_of(preview.as_ref());
        let tweening = s.canvas.prepare(l.bounds, rem);
        self.canvas_frames(tweening, window, cx);
        let drawn = self.sequence_drawn(preview.as_ref(), &l, rem, cx);
        self.render_canvas(drawn, rem, cx)
    }

    fn sequence_drawn(
        &self,
        preview: Option<&Sequence>,
        l: &SequenceLayout,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> Drawn {
        let Some(s) = self.sequence() else {
            return Drawn {
                shapes: Vec::new(),
                children: Vec::new(),
                minimap: Vec::new(),
                details: None,
                cursor: None,
                empty: None,
            };
        };
        let shown = preview.unwrap_or(s.edits.working());
        let c = &s.canvas;
        let scale = c.scale(rem);
        let visible = c.visible(rem).inflate(60.);
        let selected = s.selected.as_deref();
        let mut shapes = Vec::new();
        let mut children: Vec<AnyElement> = Vec::new();

        // Fragments are containers like the diagram's groups, in the
        // neutral tone; their tags sit on top as elements.
        let frame = ui::muted();
        for f in &l.fragments {
            shapes.push(Shape::Box {
                rect: f.rect,
                fill: look::group_fill(frame),
                border: Some(look::group_border(frame)),
                border_width: 1.5,
                radius: look::GROUP_RADIUS,
                dashed: false,
            });
            for (y, _) in &f.branches {
                shapes.push(Shape::Line {
                    from: P { x: f.rect.x, y: *y },
                    to: P {
                        x: f.rect.right(),
                        y: *y,
                    },
                    color: look::group_border(frame),
                    width: 1.,
                    dash: Some([5., 4.]),
                });
            }
        }
        // Lifelines and activation bars in their participant's tone.
        let tones: Vec<Hsla> = shown.participants.iter().map(participant_tone).collect();
        let tone_of = |lane: usize| tones.get(lane).copied().unwrap_or(ui::muted());
        for (lane, head) in l.heads.iter().enumerate() {
            let x = head.x + head.w / 2.;
            shapes.push(Shape::Line {
                from: P { x, y: HEAD_H },
                to: P {
                    x,
                    y: l.lifeline_end,
                },
                color: tone_of(lane).opacity(0.4),
                width: 1.25,
                dash: Some([6., 5.]),
            });
        }
        for (lane, bar) in &l.bars {
            let tone = tone_of(*lane);
            shapes.push(Shape::Box {
                rect: *bar,
                fill: ui::well().blend(tone.opacity(0.18)),
                border: Some(tone.opacity(0.6)),
                border_width: 1.,
                radius: 2.,
                dashed: false,
            });
        }
        for (m, g) in shown.messages.iter().zip(&l.messages) {
            let picked = selected == Some(&format!("m:{}", m.id));
            let color = if picked {
                ui::accent()
            } else if m.kind == MessageKind::Return {
                ui::muted()
            } else {
                ui::fg().opacity(0.75)
            };
            let path = if g.self_loop {
                EdgePath {
                    segments: vec![Cubic {
                        from: g.from,
                        c1: P {
                            x: g.from.x + SELF_LOOP * 1.3,
                            y: g.from.y - 6.,
                        },
                        c2: P {
                            x: g.to.x + SELF_LOOP * 1.3,
                            y: g.to.y + 6.,
                        },
                        to: g.to,
                    }],
                }
            } else {
                canvas::straight(g.from, g.to)
            };
            // Every message ends in the open chevron; the kind shows in the
            // line: a return dashed and muted, an async call dotted.
            shapes.push(Shape::Curve {
                path,
                color,
                width: if picked { 2. } else { look::EDGE_WIDTH },
                dash: match m.kind {
                    MessageKind::Return => Some([5., 4.]),
                    MessageKind::Async => Some([2., 3.]),
                    MessageKind::Sync | MessageKind::SelfCall => None,
                },
                head: Head::Arrow,
            });
        }
        if let Some(SeqDrag::Connect {
            start,
            to,
            moved: true,
            ..
        }) = &s.drag
        {
            shapes.push(Shape::Rubber {
                from: *start,
                to: *to,
            });
        }

        // Lifeline grips (under everything else): drag one to another.
        for (lane, (p, head)) in shown.participants.iter().zip(&l.heads).enumerate() {
            let x = head.x + head.w / 2.;
            let (left, top) = c.at(x - 9., HEAD_H, rem);
            let id = p.id.clone();
            children.push(
                div()
                    .id(("genui-seq-lifeline", lane))
                    .debug_selector(move || format!("genui-seq-lifeline-{id}"))
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(px(18. * scale))
                    .h(px((l.lifeline_end - HEAD_H) * scale))
                    .cursor(CursorStyle::Crosshair)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_lifeline(lane, event, window, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }

        // Fragment tags, inside the top-left like a group's: the kind in
        // `ui::mono()` `accent`, then the condition, on the container's tone.
        for (f, g) in shown.fragments.iter().zip(&l.fragments) {
            if !g.rect.intersects(&visible) {
                continue;
            }
            let (x, y) = c.at(g.rect.x + 8., g.rect.y + 4., rem);
            children.push(
                h_flex()
                    .absolute()
                    .left(x)
                    .top(y)
                    .max_w(px((g.rect.w - 16.) * scale))
                    .overflow_hidden()
                    .gap(px(6. * scale))
                    .px(px(6. * scale))
                    .h(px(18. * scale))
                    .rounded(px(5. * scale))
                    // Opaque, so the lifelines it sits on break around it.
                    .bg(ui::well().blend(frame.opacity(0.16)))
                    .border_1()
                    .border_color(frame.opacity(0.4))
                    .text_size(px(10. * scale))
                    .line_height(px(12. * scale))
                    .whitespace_nowrap()
                    .child(
                        div()
                            .font_family(ui::mono())
                            .text_size(px(10. * scale * ui::code_scale()))
                            .line_height(px(12. * scale * ui::code_scale()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui::accent())
                            .child(fragment_name(f.kind)),
                    )
                    .children(
                        f.label
                            .clone()
                            .map(|label| div().text_color(ui::muted()).child(format!("[{label}]"))),
                    )
                    .into_any_element(),
            );
            for (y, label) in &g.branches {
                let Some(label) = label else { continue };
                let (x, y) = c.at(g.rect.x + 4., *y + 2., rem);
                children.push(
                    div()
                        .absolute()
                        .left(x)
                        .top(y)
                        .px(px(4. * scale))
                        .rounded(px(4. * scale))
                        // On the canvas color: lifelines break around it.
                        .bg(ui::well())
                        .text_size(px(11. * scale))
                        .text_color(ui::muted())
                        .child(format!("[{label}]"))
                        .into_any_element(),
                );
            }
        }

        // Messages: a band to grab, the label as a pill on the arrow.
        for (ix, (m, g)) in shown.messages.iter().zip(&l.messages).enumerate() {
            if !g.row.intersects(&visible) {
                continue;
            }
            let key = format!("m:{}", m.id);
            let picked = selected == Some(key.as_str());
            let dragging = picked && matches!(&s.drag, Some(SeqDrag::Message { moved: true, .. }));
            let id = m.id.clone();
            let band = Rect {
                x: g.row.x,
                y: g.from.y - 14.,
                w: g.row.w.max(24.),
                h: 24.,
            };
            let (x, y) = c.at(band.x, band.y, rem);
            children.push(
                div()
                    .id(("genui-seq-message", ix))
                    .debug_selector({
                        let id = m.id.clone();
                        move || format!("genui-seq-message-{id}")
                    })
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px(band.w * scale))
                    .h(px(band.h * scale))
                    .rounded(px(6. * scale))
                    .cursor(CursorStyle::OpenHand)
                    .when(picked, |d| d.bg(ui::accent().opacity(0.06)))
                    .when(dragging, |d| d.bg(ui::accent().opacity(0.1)))
                    .hover(|d| d.bg(ui::accent().opacity(0.06)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_message(&id, event, window, cx)
                        }),
                    )
                    .into_any_element(),
            );
            if m.label.is_empty() || scale < 0.35 {
                continue;
            }
            let rect = label_rect(l, ix);
            let (x, y) = c.at(rect.x, rect.y, rem);
            children.push(
                h_flex()
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px(rect.w * scale))
                    .h(px(rect.h * scale))
                    .justify_center()
                    .when(g.self_loop, |d| d.justify_start())
                    .child(
                        look::label_pill(scale)
                            .max_w_full()
                            .truncate()
                            .when(picked, |d| {
                                d.border_color(ui::accent().opacity(0.6))
                                    .text_color(ui::accent())
                            })
                            .child(m.label.clone()),
                    )
                    .into_any_element(),
            );
        }

        // Participant cards.
        for (p, head) in shown.participants.iter().zip(&l.heads) {
            let key = format!("p:{}", p.id);
            let picked = selected == Some(key.as_str());
            let tone = participant_tone(p);
            let (x, y) = c.at(head.x, head.y, rem);
            let id = p.id.clone();
            children.push(
                look::card(tone, scale)
                    .flex()
                    .items_center()
                    .id(SharedString::from(format!(
                        "genui-seq-participant-{}",
                        p.id
                    )))
                    .debug_selector({
                        let id = p.id.clone();
                        move || format!("genui-seq-participant-{id}")
                    })
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px(head.w * scale))
                    .h(px(head.h * scale))
                    .gap(px(8. * scale))
                    .px(px(10. * scale))
                    .when(picked, |d| d.border_color(ui::accent()))
                    .cursor(CursorStyle::OpenHand)
                    .hover(|d| d.bg(look::hover_fill(tone)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_participant(&id, event, window, cx)
                        }),
                    )
                    .child(look::badge(tone, participant_icon(p), look::BADGE, scale))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13. * scale))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(ui::fg())
                                    .child(p.label.clone()),
                            )
                            .when(scale >= 0.5, |d| {
                                d.child(
                                    div()
                                        .font_family(ui::mono())
                                        .text_size(px(10.5 * scale * ui::code_scale()))
                                        .text_color(ui::faint())
                                        .child(p.kind.name()),
                                )
                            }),
                    )
                    .into_any_element(),
            );
        }

        let mut minimap: Vec<(Rect, Hsla)> = l
            .heads
            .iter()
            .zip(&tones)
            .map(|(r, tone)| (*r, tone.opacity(0.7)))
            .collect();
        minimap.extend(
            l.bars
                .iter()
                .map(|(lane, r)| (*r, tone_of(*lane).opacity(0.5))),
        );
        let details = selected.and_then(|key| self.sequence_details(shown, key, cx));
        Drawn {
            shapes,
            children,
            minimap,
            details,
            cursor: matches!(s.drag, Some(SeqDrag::Connect { .. }))
                .then_some(CursorStyle::Crosshair),
            empty: shown
                .participants
                .is_empty()
                .then_some("Nothing drawn yet. Add a participant with +."),
        }
    }

    fn sequence_details(
        &self,
        shown: &Sequence,
        key: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (what, id) = key.split_once(':')?;
        let id = id.to_string();
        let rename = {
            let key = key.to_string();
            canvas::action("genui-seq-rename", IconName::Pencil, "Rename").on_click(
                cx.listener(move |this, _, window, cx| this.sequence_rename(&key, window, cx)),
            )
        };
        if what == "p" {
            let p = shown.participant(&id)?;
            let subject = format!("participant “{}”", p.label);
            let pid = id.clone();
            return Some(canvas::details(
                participant_icon(p),
                participant_tone(p),
                p.label.clone(),
                vec![
                    canvas::pills(
                        "genui-seq-kind",
                        &ParticipantKind::ALL.map(ParticipantKind::name),
                        p.kind.name(),
                        move |this, kind, cx| {
                            this.sequence_edit(
                                Change::UpdateParticipant {
                                    participant: pid.clone(),
                                    label: None,
                                    kind: ParticipantKind::parse(kind),
                                },
                                cx,
                            );
                        },
                        cx,
                    ),
                    canvas::actions(vec![
                        rename,
                        canvas::action("genui-seq-ask", IconName::MessageSquare, "Ask about this")
                            .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                    ]),
                ],
                cx,
            ));
        }
        let m = shown.message(&id)?;
        let who = |p: &str| {
            shown
                .participant(p)
                .map_or(p, |p| p.label.as_str())
                .to_string()
        };
        let title = if m.label.is_empty() {
            format!("{} → {}", who(&m.from), who(&m.to))
        } else {
            m.label.clone()
        };
        let subject = format!("message “{title}”");
        let mut rows = vec![
            div()
                .text_color(ui::muted())
                .child(format!(
                    "{} → {} · step {}",
                    who(&m.from),
                    who(&m.to),
                    shown.index(&id).map_or(0, |i| i + 1)
                ))
                .into_any_element(),
        ];
        if m.from != m.to {
            let mid = id.clone();
            rows.push(canvas::pills(
                "genui-seq-message-kind",
                &["sync", "async", "return"],
                m.kind.name(),
                move |this, kind, cx| {
                    this.sequence_edit(
                        Change::update_message(&mid, None, MessageKind::parse(kind), None),
                        cx,
                    );
                },
                cx,
            ));
        }
        rows.extend(m.note.clone().map(|note| {
            div()
                .text_color(ui::muted())
                .whitespace_normal()
                .child(note)
                .into_any_element()
        }));
        let gone = id.clone();
        rows.push(canvas::actions(vec![
            rename,
            canvas::action("genui-seq-ask", IconName::MessageSquare, "Ask about this")
                .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
            canvas::action("genui-seq-delete", IconName::Trash, "Delete")
                .on_click(cx.listener(move |this, _, _, cx| this.remove_message(&gone, cx))),
        ]));
        Some(canvas::details(
            IconName::ArrowLeftRight,
            ui::info(),
            title,
            rows,
            cx,
        ))
    }

    fn press_participant(
        &mut self,
        id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let key = format!("p:{id}");
        if event.click_count >= 2 {
            self.sequence_rename(&key, window, cx);
            return;
        }
        let rem = window.rem_size();
        if let Some(s) = state(self) {
            s.selected = Some(key);
            let x = s.canvas.to_world(event.position, rem).x;
            s.drag = Some(SeqDrag::Participant {
                id: id.to_string(),
                from: event.position,
                x,
                moved: false,
            });
        }
        cx.notify();
    }

    fn press_message(
        &mut self,
        id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let key = format!("m:{id}");
        if event.click_count >= 2 {
            self.sequence_rename(&key, window, cx);
            return;
        }
        let rem = window.rem_size();
        if let Some(s) = state(self) {
            s.selected = Some(key);
            let y = s.canvas.to_world(event.position, rem).y;
            s.drag = Some(SeqDrag::Message {
                id: id.to_string(),
                from: event.position,
                y,
                moved: false,
            });
        }
        cx.notify();
    }

    fn press_lifeline(
        &mut self,
        lane: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let rem = window.rem_size();
        if let Some(s) = state(self) {
            let at = s.canvas.to_world(event.position, rem);
            let start = P {
                x: seq::lane_center(lane),
                y: at.y,
            };
            s.drag = Some(SeqDrag::Connect {
                lane,
                from: event.position,
                start,
                to: at,
                moved: false,
            });
        }
        cx.notify();
    }

    /// The background: nothing of its own (it pans).
    pub(super) fn sequence_down(
        &mut self,
        _: &MouseDownEvent,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> bool {
        false
    }

    pub(super) fn sequence_move(
        &mut self,
        event: &MouseMoveEvent,
        pressed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rem = window.rem_size();
        let Some(s) = state(self) else { return };
        let at = s.canvas.to_world(event.position, rem);
        match &mut s.drag {
            Some(SeqDrag::Participant {
                from, x, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *x = at.x;
            }
            Some(SeqDrag::Message {
                from, y, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *y = at.y;
            }
            Some(SeqDrag::Connect {
                from, to, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *to = at;
            }
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn sequence_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = state(self) else { return };
        let Some(drag) = s.drag.take() else { return };
        let working = s.edits.working().clone();
        let l = seq::layout(&working);
        let change = match drag {
            SeqDrag::Participant {
                id, x, moved: true, ..
            } => {
                let order = Sequence::moved(
                    working.participants.iter().map(|p| p.id.as_str()),
                    &id,
                    l.lane_at(x),
                );
                Change::OrderParticipants {
                    participants: order,
                }
            }
            SeqDrag::Message {
                id, y, moved: true, ..
            } => {
                let index = l.insert_at(y, working.index(&id));
                Change::OrderMessages {
                    messages: Sequence::moved(
                        working.messages.iter().map(|m| m.id.as_str()),
                        &id,
                        index,
                    ),
                }
            }
            SeqDrag::Connect {
                lane,
                start,
                to,
                moved: true,
                ..
            } => {
                let target = l.lane_at(to.x);
                let (Some(a), Some(b)) = (
                    working.participants.get(lane),
                    working.participants.get(target),
                ) else {
                    return;
                };
                let message = Message::new(working.fresh_message_id(), &a.id, &b.id, "");
                let id = message.id.clone();
                let index = l.insert_at(start.y, None);
                if self.sequence_edit(Change::AddMessage { message, index }, cx) {
                    let key = format!("m:{id}");
                    if let Some(s) = state(self) {
                        s.selected = Some(key.clone());
                    }
                    self.sequence_rename(&key, window, cx);
                }
                return;
            }
            _ => return,
        };
        self.sequence_edit(change, cx);
    }

    /// The rename field for `key` (`p:<id>` or `m:<id>`).
    fn sequence_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.sequence() else { return };
        let working = s.edits.working().clone();
        let l = seq::layout(&working);
        let Some((what, id)) = key.split_once(':') else {
            return;
        };
        let (text, at) = match what {
            "p" => {
                let Some(lane) = working.lane(id) else { return };
                (working.participants[lane].label.clone(), l.heads[lane])
            }
            _ => {
                let Some(ix) = working.index(id) else { return };
                (working.messages[ix].label.clone(), label_rect(&l, ix))
            }
        };
        if let Some(s) = state(self) {
            s.selected = Some(key.to_string());
            s.drag = None;
        }
        self.start_canvas_edit(key.to_string(), text, at, window, cx);
    }

    pub(super) fn sequence_text(&mut self, target: &str, text: String, cx: &mut Context<Self>) {
        let Some(s) = self.sequence() else { return };
        let working = s.edits.working();
        let change = match target.split_once(':') {
            Some(("p", id)) => {
                let same = working.participant(id).is_none_or(|p| p.label == text);
                if same || text.is_empty() {
                    return;
                }
                Change::UpdateParticipant {
                    participant: id.to_string(),
                    label: Some(text),
                    kind: None,
                }
            }
            Some((_, id)) => {
                if working.message(id).is_none_or(|m| m.label == text) {
                    return;
                }
                Change::update_message(id, Some(text), None, None)
            }
            None => return,
        };
        self.sequence_edit(change, cx);
    }

    fn remove_message(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.sequence_edit(
            Change::RemoveMessage {
                message: id.to_string(),
            },
            cx,
        ) && let Some(s) = state(self)
        {
            s.selected = None;
        }
    }

    /// The header's +: a participant at the right end, named at once.
    pub(super) fn add_participant(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.sequence() else { return };
        let working = s.edits.working();
        let label = format!("Participant {}", working.participants.len() + 1);
        let participant = Participant {
            id: working.fresh_participant_id(&label),
            label,
            kind: ParticipantKind::Service,
            color: None,
            icon: None,
        };
        let key = format!("p:{}", participant.id);
        if self.sequence_edit(Change::AddParticipant { participant }, cx) {
            self.sequence_rename(&key, window, cx);
        }
    }

    pub(super) fn sequence_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.canvas_key(keystroke, window, cx) {
            return true;
        }
        if self.typing(window, cx) {
            return false;
        }
        let Some(s) = self.sequence() else {
            return false;
        };
        let Some(key) = s.selected.clone() else {
            return false;
        };
        let working = s.edits.working().clone();
        let Some((what, id)) = key.split_once(':') else {
            return false;
        };
        let alt = keystroke.modifiers.alt;
        match (keystroke.key.as_str(), what) {
            ("backspace" | "delete", "m") => self.remove_message(id, cx),
            ("enter", _) => self.sequence_rename(&key, window, cx),
            ("escape", _) => self.canvas_select(None, cx),
            ("up" | "down", "m") if alt => {
                let Some(at) = working.index(id) else {
                    return false;
                };
                let to = if keystroke.key == "up" {
                    at.checked_sub(1)
                } else {
                    Some(at + 1).filter(|i| *i < working.messages.len())
                };
                let Some(to) = to else { return true };
                let order = Sequence::moved(working.messages.iter().map(|m| m.id.as_str()), id, to);
                self.sequence_edit(Change::OrderMessages { messages: order }, cx);
            }
            ("left" | "right", "p") if alt => {
                let Some(at) = working.lane(id) else {
                    return false;
                };
                let to = if keystroke.key == "left" {
                    at.checked_sub(1)
                } else {
                    Some(at + 1).filter(|i| *i < working.participants.len())
                };
                let Some(to) = to else { return true };
                let order =
                    Sequence::moved(working.participants.iter().map(|p| p.id.as_str()), id, to);
                self.sequence_edit(
                    Change::OrderParticipants {
                        participants: order,
                    },
                    cx,
                );
            }
            _ => return false,
        }
        true
    }
}
