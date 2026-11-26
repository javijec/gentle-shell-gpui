//! The state kind on the shared canvas, edited like the diagram: state
//! cards in their tone (the current one in `accent`), the initial dot,
//! final rings and choice diamonds, composite states as tinted containers
//! and transitions as chevron arrows labeled `event [guard] / action`.
//! Drag states to move them, drag ● to another state for a transition,
//! double-click to rename; the details island changes a state's kind.

use std::rc::Rc;

use gentle_core::genui::edits::{Editable as _, Edits};
use gentle_core::genui::layout::{
    GROUP_HEADER, GROUP_PAD, Layout, Point as P, Rect, STATES as SIZE, label_size,
};
use gentle_core::genui::states::{self, Change, Machine, State, StateKind, Transition};
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::canvas::{self, Canvas, Drawn, Head, Shape, moved};
use super::diagram::snap;
use super::look;
use super::panel::{Body, PanelView};
use crate::shared::ui;

/// How close (px) a press must be to a transition to pick it.
const PICK: f32 = 6.;

pub(super) enum StateDrag {
    Move {
        id: String,
        start: P,
        grab: P,
        at: P,
        from: Point<Pixels>,
        moved: bool,
    },
    /// A transition drawn from a state's handle to the pointer.
    Connect { from: String, start: P, to: P },
}

pub(super) struct MachineState {
    pub edits: Edits<Machine>,
    pub canvas: Canvas,
    /// `s:<state>` or `t:<transition>`.
    pub selected: Option<String>,
    pub drag: Option<StateDrag>,
    laid: Option<(Machine, Rc<Layout>)>,
}

impl MachineState {
    pub fn new(spec: Machine) -> Self {
        Self {
            edits: Edits::new(spec),
            canvas: Canvas::new(),
            selected: None,
            drag: None,
            laid: None,
        }
    }

    pub fn rebase(&mut self, spec: Machine) {
        self.edits.rebase(spec);
        self.drag = None;
        let working = self.edits.working();
        let gone = match self.selected.as_deref().and_then(|k| k.split_once(':')) {
            Some(("s", id)) => working.state(id).is_none(),
            Some((_, id)) => working.transition(id).is_none(),
            None => false,
        };
        if gone {
            self.selected = None;
        }
    }

    /// The working machine with a state being dragged where it would land,
    /// or `None` when no state drag moves anything (the working one is shown).
    fn preview(&self) -> Option<Machine> {
        let Some(StateDrag::Move {
            id,
            start,
            grab,
            at,
            moved: true,
            ..
        }) = &self.drag
        else {
            return None;
        };
        let mut shown = self.edits.working().clone();
        let _ = shown.apply(&Change::Move {
            state: id.clone(),
            x: snap(start.x + at.x - grab.x),
            y: snap(start.y + at.y - grab.y),
        });
        Some(shown)
    }

    /// The layout of `preview`, else of the working machine.
    fn layout_of(&mut self, preview: Option<&Machine>) -> Rc<Layout> {
        let spec = preview.unwrap_or(self.edits.working());
        match &self.laid {
            Some((laid, layout)) if laid == spec => layout.clone(),
            _ => {
                let layout = Rc::new(states::layout(spec));
                self.laid = Some((spec.clone(), layout.clone()));
                layout
            }
        }
    }
}

fn state(this: &mut PanelView) -> Option<&mut MachineState> {
    match &mut this.body {
        Some(Body::State(s)) => Some(s),
        _ => None,
    }
}

/// The slot a state's card fills (pseudo-states draw small in its middle).
fn slot(rect: Rect, kind: StateKind) -> Rect {
    if !kind.pseudo() {
        return rect;
    }
    let c = rect.center();
    Rect {
        x: c.x - SIZE.node_w / 2.,
        y: c.y - SIZE.node_h / 2.,
        w: SIZE.node_w,
        h: SIZE.node_h,
    }
}

fn kind_name(kind: StateKind) -> &'static str {
    kind.name()
}

/// A state's tone: `accent` while it's the current one, else its `color`,
/// else neutral so a machine reads calm until an agent tints it.
fn state_tone(st: &State, current: bool) -> Hsla {
    if current {
        ui::accent()
    } else {
        look::tone(st.color, ui::muted())
    }
}

impl PanelView {
    fn machine(&self) -> Option<&MachineState> {
        match &self.body {
            Some(Body::State(s)) => Some(s),
            _ => None,
        }
    }

    fn state_edit(&mut self, change: Change, cx: &mut Context<Self>) -> bool {
        self.edit_with(cx, |body| match body {
            Body::State(s) => Some(s.edits.edit(&change)),
            _ => None,
        })
    }

    pub(super) fn render_states(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rem = window.rem_size();
        let Some(s) = state(self) else {
            return div().into_any_element();
        };
        let preview = s.preview();
        let l = s.layout_of(preview.as_ref());
        let tweening = s.canvas.prepare(l.bounds, rem);
        self.canvas_frames(tweening, window, cx);
        let drawn = self.states_drawn(preview.as_ref(), &l, rem, cx);
        self.render_canvas(drawn, rem, cx)
    }

    fn states_drawn(
        &self,
        preview: Option<&Machine>,
        l: &Layout,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> Drawn {
        let Some(s) = self.machine() else {
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
        let visible = c.visible(rem).inflate(40.);
        let selected = s.selected.as_deref();
        let mut shapes = Vec::new();
        let mut children: Vec<AnyElement> = Vec::new();

        for (g, rect) in shown.groups.iter().zip(&l.groups) {
            let Some(rect) = rect else { continue };
            let tone = look::tone(g.color, ui::muted());
            shapes.push(Shape::Box {
                rect: *rect,
                fill: look::group_fill(tone),
                border: Some(look::group_border(tone)),
                border_width: 1.5,
                radius: look::GROUP_RADIUS,
                dashed: false,
            });
            // The tag sits in the header band the layout keeps above the
            // states.
            let (x, y) = c.at(rect.x + 10., rect.y + (GROUP_HEADER - 18.) / 2. + 2., rem);
            children.push(
                div()
                    .absolute()
                    .left(x)
                    .top(y)
                    .max_w(px((rect.w - GROUP_PAD) * scale))
                    .overflow_hidden()
                    .child(look::group_tag(tone, &g.label, None, scale))
                    .into_any_element(),
            );
        }
        for (t, path) in shown.transitions.iter().zip(&l.edges) {
            let picked = selected == Some(&format!("t:{}", t.id));
            shapes.push(Shape::Curve {
                path: path.clone(),
                color: if picked { ui::accent() } else { look::line() },
                width: if picked { 2.25 } else { look::EDGE_WIDTH },
                dash: None,
                head: Head::Arrow,
            });
        }
        // Pseudo-states are painted; a card-less hit box sits on each.
        for (st, rect) in shown.states.iter().zip(&l.nodes) {
            let picked = selected == Some(&format!("s:{}", st.id));
            let center = rect.center();
            match st.kind {
                // A solid dot in a soft halo of its tone.
                StateKind::Initial => {
                    let tone = if picked {
                        ui::accent()
                    } else {
                        look::tone(st.color, ui::fg())
                    };
                    let halo = rect.inflate(3.);
                    shapes.push(Shape::Box {
                        rect: halo,
                        fill: tone.opacity(0.14),
                        border: None,
                        border_width: 0.,
                        radius: halo.w / 2.,
                        dashed: false,
                    });
                    shapes.push(Shape::Box {
                        rect: *rect,
                        fill: tone,
                        border: None,
                        border_width: 0.,
                        radius: rect.w / 2.,
                        dashed: false,
                    });
                }
                StateKind::Final => {
                    let tone = if picked {
                        ui::accent()
                    } else {
                        look::tone(st.color, ui::fg())
                    };
                    shapes.push(Shape::Box {
                        rect: *rect,
                        fill: look::fill(tone),
                        border: Some(tone),
                        border_width: 1.5,
                        radius: rect.w / 2.,
                        dashed: false,
                    });
                    let inner = rect.inflate(-4.5);
                    shapes.push(Shape::Box {
                        rect: inner,
                        fill: tone,
                        border: None,
                        border_width: 0.,
                        radius: inner.w / 2.,
                        dashed: false,
                    });
                }
                // A tinted diamond: its tone's hairline around the card
                // fill, like a state card.
                StateKind::Choice => {
                    let tone = look::tone(st.color, ui::muted());
                    let r = rect.w / 2.;
                    let points = vec![
                        P {
                            x: center.x,
                            y: center.y - r,
                        },
                        P {
                            x: center.x + r,
                            y: center.y,
                        },
                        P {
                            x: center.x,
                            y: center.y + r,
                        },
                        P {
                            x: center.x - r,
                            y: center.y,
                        },
                    ];
                    shapes.push(Shape::Polygon {
                        points: points.clone(),
                        color: if picked {
                            ui::accent()
                        } else {
                            look::edge_color(tone)
                        },
                    });
                    let inner: Vec<P> = points
                        .iter()
                        .map(|p| P {
                            x: center.x + (p.x - center.x) * 0.9,
                            y: center.y + (p.y - center.y) * 0.9,
                        })
                        .collect();
                    shapes.push(Shape::Polygon {
                        points: inner,
                        color: look::fill(tone),
                    });
                }
                StateKind::Normal => {}
            }
        }
        if let Some(StateDrag::Connect { start, to, .. }) = &s.drag {
            shapes.push(Shape::Rubber {
                from: *start,
                to: *to,
            });
        }

        // Transition labels, where the layout placed them clear of each other.
        if scale >= 0.35 {
            for (ix, (t, spot)) in shown.transitions.iter().zip(&l.labels).enumerate() {
                let label = t.label();
                let Some(p) = spot.filter(|_| !label.is_empty()) else {
                    continue;
                };
                if !visible.contains(p) {
                    continue;
                }
                let picked = selected == Some(&format!("t:{}", t.id));
                let (width, height) = label_size(&label, false);
                let (x, y) = c.at(p.x - width / 2., p.y - height / 2., rem);
                let id = t.id.clone();
                children.push(
                    h_flex()
                        .absolute()
                        .left(x)
                        .top(y)
                        .w(px(width * scale))
                        .h(px(height * scale))
                        .items_center()
                        .justify_center()
                        .child(
                            look::label_pill(scale)
                                .id(("genui-state-transition", ix))
                                .debug_selector({
                                    let id = t.id.clone();
                                    move || format!("genui-state-transition-{id}")
                                })
                                .when(picked, |d| {
                                    d.border_color(ui::accent().opacity(0.6))
                                        .text_color(ui::accent())
                                })
                                .max_w_full()
                                .truncate()
                                .cursor_pointer()
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.state_pick(
                                            format!("t:{id}"),
                                            event.click_count >= 2,
                                            window,
                                            cx,
                                        )
                                    }),
                                ),
                        )
                        .into_any_element(),
                );
            }
        }

        for (ix, (st, rect)) in shown.states.iter().zip(&l.nodes).enumerate() {
            if !rect.intersects(&visible) {
                continue;
            }
            let current = shown.current.as_deref() == Some(st.id.as_str());
            children.push(self.state_card(ix, st, *rect, current, scale, rem, cx));
        }

        let minimap = shown
            .states
            .iter()
            .zip(&l.nodes)
            .map(|(st, r)| {
                let current = shown.current.as_deref() == Some(st.id.as_str());
                (*r, state_tone(st, current).opacity(0.7))
            })
            .collect();
        let details = selected.and_then(|key| self.state_details(shown, key, cx));
        Drawn {
            shapes,
            children,
            minimap,
            details,
            cursor: matches!(s.drag, Some(StateDrag::Connect { .. }))
                .then_some(CursorStyle::Crosshair),
            empty: shown
                .states
                .is_empty()
                .then_some("No states yet. Add one with +."),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn state_card(
        &self,
        ix: usize,
        st: &State,
        rect: Rect,
        current: bool,
        scale: f32,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(s) = self.machine() else {
            return div().into_any_element();
        };
        let picked = s.selected.as_deref() == Some(&format!("s:{}", st.id));
        let dragging = picked && matches!(&s.drag, Some(StateDrag::Move { moved: true, .. }));
        let (x, y) = s.canvas.at(rect.x, rect.y, rem);
        let id = st.id.clone();
        let handle_id = st.id.clone();
        let handle = div()
            .id(("genui-state-handle", ix))
            .debug_selector({
                let id = st.id.clone();
                move || format!("genui-state-handle-{id}")
            })
            .absolute()
            .size(px(10. * scale))
            .rounded_full()
            .bg(ui::surface())
            .border_2()
            .border_color(ui::accent())
            .cursor(CursorStyle::Crosshair)
            .right(px(-5. * scale))
            .top(px((rect.h / 2. - 5.) * scale))
            .when(!picked, |d| {
                d.opacity(0.).group_hover("genui-state", |d| d.opacity(1.))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_state_handle(&handle_id, event, window, cx)
                }),
            );
        let tone = state_tone(st, current);
        // Pseudo-states are painted under a bare hit box; a state is a card.
        let frame = if st.kind.pseudo() {
            div()
        } else {
            look::card(tone, scale)
        };
        let base = frame
            .id(("genui-state", ix))
            .debug_selector({
                let id = st.id.clone();
                move || format!("genui-state-{id}")
            })
            .group("genui-state")
            .absolute()
            .left(x)
            .top(y)
            .w(px(rect.w * scale))
            .h(px(rect.h * scale))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_state(&id, event, window, cx)
                }),
            );
        if st.kind.pseudo() {
            // The painted shape shows; a label sits under a choice.
            return base
                .rounded_full()
                .child(handle)
                .when(st.kind == StateKind::Choice, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px((rect.h + 4.) * scale))
                            .left(px(-40. * scale))
                            .w(px((rect.w + 80.) * scale))
                            .flex()
                            .justify_center()
                            .text_size(px(10.5 * scale))
                            .text_color(ui::muted())
                            .child(st.label.clone()),
                    )
                })
                .into_any_element();
        }
        let badge = st
            .icon
            .and_then(look::icon)
            .map(|icon| look::badge(tone, icon, look::BADGE, scale));
        base.flex()
            .items_center()
            .gap(px(8. * scale))
            .px(px(12. * scale))
            .when(picked, |d| d.border_color(ui::accent()))
            .when(current, |d| d.border_2().border_color(ui::accent()))
            .when(dragging, |d| d.shadow_lg())
            .hover(|d| d.bg(look::hover_fill(tone)))
            .children(badge)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_center()
                    .child(
                        h_flex()
                            .gap(px(6. * scale))
                            .min_w_0()
                            .when(current, |d| {
                                d.child(
                                    div()
                                        .flex_shrink_0()
                                        .size(px(7. * scale))
                                        .rounded_full()
                                        .bg(ui::accent()),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13. * scale))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(ui::fg())
                                    .child(st.label.clone()),
                            ),
                    )
                    .when_some(
                        st.description.clone().filter(|_| scale >= 0.5),
                        |d, description| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_size(px(11. * scale))
                                    .text_color(ui::muted())
                                    .child(description),
                            )
                        },
                    )
                    .when(current && scale >= 0.5 && st.description.is_none(), |d| {
                        d.child(
                            div()
                                .text_size(px(10.5 * scale))
                                .text_color(ui::accent())
                                .child("current"),
                        )
                    }),
            )
            .when(picked, |d| {
                d.child(ui::popover_in(
                    ("genui-state-ring", ix),
                    div()
                        .absolute()
                        .inset(px(-4. * scale))
                        .rounded(px((look::CARD_RADIUS + 4.) * scale))
                        .border_2()
                        .border_color(ui::accent().opacity(0.35)),
                ))
            })
            .child(handle)
            .into_any_element()
    }

    fn state_details(
        &self,
        shown: &Machine,
        key: &str,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (what, id) = key.split_once(':')?;
        let rename = {
            let key = key.to_string();
            canvas::action("genui-state-rename", IconName::Pencil, "Rename").on_click(
                cx.listener(move |this, _, window, cx| this.state_rename(&key, window, cx)),
            )
        };
        if what == "s" {
            let st = shown.state(id)?;
            let subject = format!("state “{}”", st.label);
            let (kind_id, gone) = (st.id.clone(), st.id.clone());
            let mut rows = vec![canvas::pills(
                "genui-state-kind",
                &StateKind::ALL.map(StateKind::name),
                kind_name(st.kind),
                move |this, kind, cx| {
                    if let Some(kind) = StateKind::parse(kind) {
                        this.state_edit(Change::state_kind(&kind_id, kind), cx);
                    }
                },
                cx,
            )];
            if shown.current.as_deref() == Some(id) {
                rows.push(
                    div()
                        .text_color(ui::accent())
                        .child("The machine is in this state now")
                        .into_any_element(),
                );
            }
            rows.extend(st.description.clone().map(|d| {
                div()
                    .text_color(ui::muted())
                    .whitespace_normal()
                    .child(d)
                    .into_any_element()
            }));
            rows.push(canvas::actions(vec![
                rename,
                canvas::action("genui-state-ask", IconName::MessageSquare, "Ask about this")
                    .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                canvas::action("genui-state-delete", IconName::Trash, "Delete").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.state_remove(
                            Change::RemoveState {
                                state: gone.clone(),
                            },
                            cx,
                        )
                    }),
                ),
            ]));
            return Some(canvas::details(
                st.icon.and_then(look::icon).unwrap_or(IconName::CircleDot),
                state_tone(st, shown.current.as_deref() == Some(id)),
                st.label.clone(),
                rows,
                cx,
            ));
        }
        let t = shown.transition(id)?;
        let who = |s: &str| shown.state(s).map_or(s, |s| s.label.as_str()).to_string();
        let label = t.label();
        let title = format!("{} → {}", who(&t.from), who(&t.to));
        let subject = if label.is_empty() {
            format!("the transition {title}")
        } else {
            format!("the transition “{label}” ({title})")
        };
        let gone = t.id.clone();
        Some(canvas::details(
            IconName::ArrowRight,
            ui::info(),
            title,
            vec![
                div()
                    .text_color(ui::muted())
                    .child(if label.is_empty() {
                        "no event".to_string()
                    } else {
                        label
                    })
                    .into_any_element(),
                canvas::actions(vec![
                    rename,
                    canvas::action("genui-state-ask", IconName::MessageSquare, "Ask about this")
                        .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                    canvas::action("genui-state-delete", IconName::Trash, "Delete").on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.state_remove(
                                Change::RemoveTransition {
                                    transition: gone.clone(),
                                },
                                cx,
                            )
                        }),
                    ),
                ]),
            ],
            cx,
        ))
    }

    fn state_pick(
        &mut self,
        key: String,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        if rename {
            self.state_rename(&key, window, cx);
        } else {
            self.canvas_select(Some(key), cx);
        }
    }

    fn press_state(
        &mut self,
        id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let key = format!("s:{id}");
        if event.click_count >= 2 {
            self.state_rename(&key, window, cx);
            return;
        }
        let rem = window.rem_size();
        let start = self.machine().and_then(|s| {
            let (laid, l) = s.laid.as_ref()?;
            let ix = laid.state_index(id)?;
            Some(slot(l.nodes[ix], laid.states[ix].kind))
        });
        if let (Some(rect), Some(s)) = (start, state(self)) {
            s.selected = Some(key);
            let grab = s.canvas.to_world(event.position, rem);
            s.drag = Some(StateDrag::Move {
                id: id.to_string(),
                start: P {
                    x: rect.x,
                    y: rect.y,
                },
                grab,
                at: grab,
                from: event.position,
                moved: false,
            });
        }
        cx.notify();
    }

    fn press_state_handle(
        &mut self,
        id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let rem = window.rem_size();
        let start = self.machine().and_then(|s| {
            let (laid, l) = s.laid.as_ref()?;
            Some(l.nodes[laid.state_index(id)?].center())
        });
        if let (Some(start), Some(s)) = (start, state(self)) {
            let to = s.canvas.to_world(event.position, rem);
            s.drag = Some(StateDrag::Connect {
                from: id.to_string(),
                start,
                to,
            });
        }
        cx.notify();
    }

    /// The background: a press near a transition picks it.
    pub(super) fn state_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let rem = window.rem_size();
        let Some(s) = self.machine() else {
            return false;
        };
        let Some((laid, l)) = &s.laid else {
            return false;
        };
        let at = s.canvas.to_world(event.position, rem);
        let hit = l
            .edge_at(at, PICK / s.canvas.scale(rem))
            .and_then(|ix| laid.transitions.get(ix))
            .map(|t| t.id.clone());
        match hit {
            Some(id) => {
                self.state_pick(format!("t:{id}"), event.click_count >= 2, window, cx);
                true
            }
            None => false,
        }
    }

    pub(super) fn state_move(
        &mut self,
        event: &MouseMoveEvent,
        pressed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rem = window.rem_size();
        let Some(s) = state(self) else { return };
        let world = s.canvas.to_world(event.position, rem);
        match &mut s.drag {
            Some(StateDrag::Move {
                at, from, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *at = world;
            }
            Some(StateDrag::Connect { to, .. }) if pressed => *to = world,
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn state_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = state(self) else { return };
        let Some(drag) = s.drag.take() else { return };
        match drag {
            StateDrag::Move {
                id,
                start,
                grab,
                at,
                moved: true,
                ..
            } => {
                self.state_edit(
                    Change::Move {
                        state: id,
                        x: snap(start.x + at.x - grab.x),
                        y: snap(start.y + at.y - grab.y),
                    },
                    cx,
                );
            }
            StateDrag::Connect { from, to, .. } => {
                let Some((laid, l)) = &s.laid else { return };
                // Pseudo-states are small: their whole slot takes the drop.
                let target = laid
                    .states
                    .iter()
                    .zip(&l.nodes)
                    .rposition(|(st, r)| slot(*r, st.kind).contains(to))
                    .map(|ix| laid.states[ix].id.clone());
                let Some(target) = target else { return };
                let working = s.edits.working();
                let transition = Transition {
                    id: working.fresh_transition_id(&from, &target, |_| false),
                    from,
                    to: target,
                    event: None,
                    guard: None,
                    action: None,
                };
                let key = format!("t:{}", transition.id);
                if self.state_edit(Change::AddTransition { transition }, cx) {
                    self.state_rename(&key, window, cx);
                }
            }
            _ => {}
        }
    }

    /// The rename field: a state's label or a transition's event.
    fn state_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.machine() else { return };
        let working = s.edits.working();
        let l = states::layout(working);
        let Some((what, id)) = key.split_once(':') else {
            return;
        };
        let found = match what {
            "s" => working.state_index(id).map(|ix| {
                let st = &working.states[ix];
                let rect = slot(l.nodes[ix], st.kind);
                (st.label.clone(), rect)
            }),
            _ => working
                .transitions
                .iter()
                .position(|t| t.id == id)
                .map(|ix| {
                    let p = l.labels[ix].unwrap_or_else(|| l.edges[ix].label_at());
                    (
                        working.transitions[ix].event.clone().unwrap_or_default(),
                        Rect {
                            x: p.x - 80.,
                            y: p.y - 14.,
                            w: 160.,
                            h: 28.,
                        },
                    )
                }),
        };
        let Some((text, at)) = found else { return };
        if let Some(s) = state(self) {
            s.selected = Some(key.to_string());
            s.drag = None;
        }
        self.start_canvas_edit(key.to_string(), text, at, window, cx);
    }

    pub(super) fn state_text(&mut self, target: &str, text: String, cx: &mut Context<Self>) {
        let Some(s) = self.machine() else { return };
        let working = s.edits.working();
        let change = match target.split_once(':') {
            Some(("s", id)) => {
                if text.is_empty() || working.state(id).is_none_or(|st| st.label == text) {
                    return;
                }
                Change::rename_state(id, text)
            }
            Some((_, id)) => {
                let event = (!text.is_empty()).then_some(text);
                if working.transition(id).is_none_or(|t| t.event == event) {
                    return;
                }
                Change::event(id, event)
            }
            None => return,
        };
        self.state_edit(change, cx);
    }

    fn state_remove(&mut self, change: Change, cx: &mut Context<Self>) {
        if self.state_edit(change, cx) {
            self.canvas_select(None, cx);
        }
    }

    /// The header's +: a state, named at once.
    pub(super) fn add_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.machine() else { return };
        let working = s.edits.working();
        let label = format!("State {}", working.states.len() + 1);
        let st = State::new(working.fresh_state_id(&label), label, StateKind::Normal);
        let key = format!("s:{}", st.id);
        if self.state_edit(Change::AddState { state: st }, cx) {
            self.state_rename(&key, window, cx);
        }
    }

    pub(super) fn state_key(
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
        let Some(key) = self.machine().and_then(|s| s.selected.clone()) else {
            return false;
        };
        let Some((what, id)) = key.split_once(':') else {
            return false;
        };
        match keystroke.key.as_str() {
            "backspace" | "delete" => {
                let change = if what == "s" {
                    Change::RemoveState {
                        state: id.to_string(),
                    }
                } else {
                    Change::RemoveTransition {
                        transition: id.to_string(),
                    }
                };
                self.state_remove(change, cx);
            }
            "enter" => self.state_rename(&key, window, cx),
            "escape" => self.canvas_select(None, cx),
            _ => return false,
        }
        true
    }
}
