//! The timeline kind on the shared canvas: a Gantt chart. Task names in a
//! column on the left, bars on a date axis that stays at the top of the
//! canvas (days, weeks or months by zoom), a today line, milestones as
//! diamonds, dependencies as arrows (the critical path in `accent`, one a
//! task starts too early for in `danger`). Drag a bar to move it, its
//! ends to resize it, ● to another task to link them; double-click to
//! rename.

use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use gentle_core::genui::edits::{Editable as _, Edits, plural};
use gentle_core::genui::layout::{Point as P, Rect};
use gentle_core::genui::style::Tint;
use gentle_core::genui::timeline::{
    self, BAR_H, Change, DAY, LABEL_W, ROW, Task, Timeline, TimelineLayout, day_parts, month_label,
    short_date,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::canvas::{self, Canvas, Drawn, Head, Shape, moved};
use super::look;
use super::panel::{Body, PanelView};
use crate::shared::ui;

/// How close (px) a press must be to a dependency to pick it.
const PICK: f32 = 6.;
/// The axis strip's height (px at 100 %), and the room left for it above
/// the rows (diagram units).
const AXIS_H: f32 = 26.;
const AXIS_ROOM: f32 = 34.;
/// A bar's grips for resizing (diagram units).
const GRIP: f32 = 7.;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Grip {
    Move,
    Start,
    End,
}

pub(super) enum TimelineDrag {
    Bar {
        id: String,
        grip: Grip,
        from: Point<Pixels>,
        grab: f32,
        at: f32,
        moved: bool,
    },
    /// A dependency drawn from a task's end to the pointer.
    Link { from: String, start: P, to: P },
}

pub(super) struct TimelineState {
    pub edits: Edits<Timeline>,
    pub canvas: Canvas,
    /// `t:<task>` or `d:<task>\n<dependency>`.
    pub selected: Option<String>,
    pub drag: Option<TimelineDrag>,
    /// Today's day number (UTC).
    pub today: i64,
    laid: Option<(Timeline, Option<i64>, Rc<TimelineLayout>)>,
}

impl TimelineState {
    pub fn new(spec: Timeline) -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Self {
            edits: Edits::new(spec),
            canvas: Canvas::new(),
            selected: None,
            drag: None,
            today: secs.div_euclid(86_400),
            laid: None,
        }
    }

    pub fn rebase(&mut self, spec: Timeline) {
        self.edits.rebase(spec);
        self.drag = None;
        let working = self.edits.working();
        let gone = match self.selected.as_deref().and_then(|k| k.split_once(':')) {
            Some(("t", id)) => working.task(id).is_none(),
            Some((_, pair)) => link_of(pair).is_none_or(|(task, on)| {
                working
                    .task(&task)
                    .is_none_or(|t| !t.depends_on.contains(&on))
            }),
            None => false,
        };
        if gone {
            self.selected = None;
        }
    }

    /// The dates a bar drag would give: (task, start, end).
    fn dragged(&self) -> Option<(String, i64, i64)> {
        let Some(TimelineDrag::Bar {
            id,
            grip,
            grab,
            at,
            moved: true,
            ..
        }) = &self.drag
        else {
            return None;
        };
        let task = self.edits.working().task(id)?;
        let days = ((at - grab) / DAY).round() as i64;
        let (start, end) = match grip {
            Grip::Move => (task.start + days, task.end + days),
            Grip::Start => ((task.start + days).min(task.end), task.end),
            Grip::End => (task.start, (task.end + days).max(task.start)),
        };
        Some((id.clone(), start, end))
    }

    /// The working timeline with a drag's dates and the axis to keep, or
    /// `None` when no bar drag moves anything (the working one is shown).
    fn preview(&self) -> Option<(Timeline, Option<i64>)> {
        let (task, start, end) = self.dragged()?;
        let mut shown = self.edits.working().clone();
        let _ = shown.apply(&Change::SetDates { task, start, end });
        let origin = self.laid.as_ref().map(|(_, _, l)| l.origin);
        Some((shown, origin))
    }

    /// The layout of `preview`, else of the working timeline.
    fn layout_of(&mut self, preview: Option<&Timeline>, origin: Option<i64>) -> Rc<TimelineLayout> {
        let spec = preview.unwrap_or(self.edits.working());
        match &self.laid {
            Some((laid, at, layout)) if laid == spec && *at == origin => layout.clone(),
            _ => {
                let layout = Rc::new(timeline::layout_from(spec, origin));
                self.laid = Some((spec.clone(), origin, layout.clone()));
                layout
            }
        }
    }
}

/// `task\non` back to the pair.
fn link_of(pair: &str) -> Option<(String, String)> {
    let (task, on) = pair.split_once('\n')?;
    Some((task.to_string(), on.to_string()))
}

fn state(this: &mut PanelView) -> Option<&mut TimelineState> {
    match &mut this.body {
        Some(Body::Timeline(s)) => Some(s),
        _ => None,
    }
}

fn span(t: &Task) -> String {
    if t.milestone {
        format!("{} · milestone", short_date(t.start))
    } else if t.start == t.end {
        format!("{} · 1 day", short_date(t.start))
    } else {
        format!(
            "{} – {} · {}",
            short_date(t.start),
            short_date(t.end),
            plural(t.days() as usize, "day")
        )
    }
}

/// A group's tone: the next in a fixed cycle by its place among the
/// groups, so groups read apart without the agent choosing.
fn group_tone(index: usize) -> Hsla {
    ui::tint(Tint::cycle(index))
}

/// A task's tone: its `color`, else its group's, else blue.
fn task_tone(t: &Task, groups: &[(String, Rect)]) -> Hsla {
    let group = t
        .group
        .as_deref()
        .and_then(|name| groups.iter().position(|(g, _)| g == name));
    look::tone(t.color, group.map_or(ui::tint(Tint::Blue), group_tone))
}

impl PanelView {
    fn timeline(&self) -> Option<&TimelineState> {
        match &self.body {
            Some(Body::Timeline(s)) => Some(s),
            _ => None,
        }
    }

    fn timeline_edit(&mut self, change: Change, cx: &mut Context<Self>) -> bool {
        self.edit_with(cx, |body| match body {
            Body::Timeline(s) => Some(s.edits.edit(&change)),
            _ => None,
        })
    }

    pub(super) fn render_timeline(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rem = window.rem_size();
        let Some(s) = state(self) else {
            return div().into_any_element();
        };
        let (preview, origin) = s.preview().unzip();
        let origin = origin.flatten();
        let l = s.layout_of(preview.as_ref(), origin);
        let mut content = l.bounds;
        content.y -= AXIS_ROOM;
        content.h += AXIS_ROOM;
        let tweening = s.canvas.prepare(content, rem);
        self.canvas_frames(tweening, window, cx);
        let drawn = self.timeline_drawn(preview.as_ref(), &l, rem, cx);
        self.render_canvas(drawn, rem, cx)
    }

    fn timeline_drawn(
        &self,
        preview: Option<&Timeline>,
        l: &TimelineLayout,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> Drawn {
        let Some(s) = self.timeline() else {
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
        let visible = c.visible(rem);
        let selected = s.selected.as_deref();
        let critical = shown.critical_path();
        let late = shown.late();
        let bottom = l.bounds.bottom();
        let right = l.bounds.right();
        let mut shapes = Vec::new();
        let mut children: Vec<AnyElement> = Vec::new();

        // Day, week or month lines by how wide a day is on screen.
        let day_px = DAY * scale;
        let first = l.day_at(visible.x.max(0.), false);
        let last = l.day_at(visible.right().min(right), false) + 1;
        let mut ticks: Vec<(i64, String)> = Vec::new();
        for day in first..=last {
            let (date, monday) = day_parts(day);
            let tick = if day_px >= 18. {
                Some(if date == 1 || day == first {
                    month_label(day)
                } else {
                    date.to_string()
                })
            } else if day_px * 7. >= 36. {
                monday.then(|| short_date(day))
            } else {
                (date == 1).then(|| month_label(day))
            };
            if day_px >= 18. && matches!((day + 3).rem_euclid(7), 5 | 6) {
                shapes.push(Shape::Box {
                    rect: Rect {
                        x: l.x_of(day),
                        y: 0.,
                        w: DAY,
                        h: bottom,
                    },
                    fill: ui::surface().opacity(0.3),
                    border: None,
                    border_width: 0.,
                    radius: 0.,
                    dashed: false,
                });
            }
            if let Some(label) = tick {
                shapes.push(Shape::Line {
                    from: P {
                        x: l.x_of(day),
                        y: 0.,
                    },
                    to: P {
                        x: l.x_of(day),
                        y: bottom,
                    },
                    color: ui::border(),
                    width: 1.,
                    dash: None,
                });
                ticks.push((day, label));
            }
        }
        for (_, rect) in &l.groups {
            shapes.push(Shape::Box {
                rect: *rect,
                fill: ui::surface().opacity(0.6),
                border: None,
                border_width: 0.,
                radius: 0.,
                dashed: false,
            });
        }
        for row in &l.rows {
            shapes.push(Shape::Line {
                from: P {
                    x: -LABEL_W,
                    y: row.bottom(),
                },
                to: P {
                    x: right,
                    y: row.bottom(),
                },
                color: ui::border().opacity(0.7),
                width: 1.,
                dash: None,
            });
        }
        shapes.push(Shape::Line {
            from: P { x: 0., y: 0. },
            to: P { x: 0., y: bottom },
            color: ui::border_strong(),
            width: 1.,
            dash: None,
        });
        let today_x = l.x_of(s.today) + DAY / 2.;
        let today_on = s.today >= l.origin && s.today <= l.last;
        if today_on {
            shapes.push(Shape::Line {
                from: P {
                    x: today_x,
                    y: -AXIS_ROOM,
                },
                to: P {
                    x: today_x,
                    y: bottom,
                },
                color: ui::accent().opacity(0.8),
                width: 1.5,
                dash: None,
            });
        }
        for (di, ti, path) in &l.links {
            let (task, on) = (&shown.tasks[*ti].id, &shown.tasks[*di].id);
            let picked = selected == Some(&format!("d:{task}\n{on}"));
            let on_path = critical.windows(2).any(|w| w[0] == *on && w[1] == *task);
            let color = if picked {
                ui::accent()
            } else if late.contains(&(*di, *ti)) {
                ui::danger()
            } else if on_path {
                ui::accent().opacity(0.8)
            } else {
                look::line()
            };
            shapes.push(Shape::Curve {
                path: path.clone(),
                color,
                width: if picked { 2. } else { look::EDGE_WIDTH },
                dash: None,
                head: Head::Arrow,
            });
        }
        for (t, bar) in shown.tasks.iter().zip(&l.bars) {
            if !t.milestone {
                continue;
            }
            // A diamond in the task's tone; on the critical path it sits in
            // an `accent` outline.
            let c = bar.center();
            let r = bar.w / 2. + 2.;
            let diamond = |r: f32| {
                vec![
                    P { x: c.x, y: c.y - r },
                    P { x: c.x + r, y: c.y },
                    P { x: c.x, y: c.y + r },
                    P { x: c.x - r, y: c.y },
                ]
            };
            if critical.contains(&t.id) {
                shapes.push(Shape::Polygon {
                    points: diamond(r + 2.),
                    color: ui::accent(),
                });
            }
            shapes.push(Shape::Polygon {
                points: diamond(r),
                color: task_tone(t, &l.groups),
            });
        }
        if let Some(TimelineDrag::Link { start, to, .. }) = &s.drag {
            shapes.push(Shape::Rubber {
                from: *start,
                to: *to,
            });
        }

        // Group headers (a dot in the group's tone, then the name) and task
        // names on the left.
        for (gi, (name, rect)) in l.groups.iter().enumerate() {
            let (x, y) = c.at(-LABEL_W + 10., rect.y, rem);
            children.push(
                h_flex()
                    .absolute()
                    .left(x)
                    .top(y)
                    .h(px(rect.h * scale))
                    .w(px((LABEL_W - 20.) * scale))
                    .gap(px(6. * scale))
                    .child(
                        div()
                            .flex_none()
                            .size(px(8. * scale))
                            .rounded_full()
                            .bg(group_tone(gi)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5 * scale))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(ui::muted())
                            .child(name.clone()),
                    )
                    .into_any_element(),
            );
        }
        for (ix, (t, row)) in shown.tasks.iter().zip(&l.rows).enumerate() {
            let key = format!("t:{}", t.id);
            let picked = selected == Some(key.as_str());
            let (x, y) = c.at(-LABEL_W + 4., row.y + 3., rem);
            children.push(
                h_flex()
                    .id(("genui-tl-label", ix))
                    .debug_selector({
                        let id = t.id.clone();
                        move || format!("genui-tl-label-{id}")
                    })
                    .absolute()
                    .left(x)
                    .top(y)
                    .h(px((ROW - 6.) * scale))
                    .w(px((LABEL_W - 12.) * scale))
                    .px(px(6. * scale))
                    .gap(px(6. * scale))
                    .rounded(px(6. * scale))
                    .cursor_pointer()
                    .when(picked, |d| d.bg(ui::accent().opacity(0.10)))
                    .hover(|d| d.bg(ui::surface_hover()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.timeline_pick(key.clone(), event.click_count >= 2, window, cx)
                        }),
                    )
                    .when(t.milestone, |d| {
                        d.child(
                            Icon::new(IconName::Flag)
                                .size(px(11. * scale))
                                .text_color(task_tone(t, &l.groups)),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5 * scale))
                            .text_color(if picked { ui::accent() } else { ui::fg() })
                            .child(t.label.clone()),
                    )
                    .into_any_element(),
            );
        }
        for (ix, (t, bar)) in shown.tasks.iter().zip(&l.bars).enumerate() {
            if !bar.inflate(20.).intersects(&visible) {
                continue;
            }
            let is_late = late.iter().any(|(_, ti)| *ti == ix);
            children.push(self.task_bar(
                ix,
                t,
                *bar,
                task_tone(t, &l.groups),
                critical.contains(&t.id),
                is_late,
                scale,
                rem,
                cx,
            ));
        }

        // The axis strip, pinned to the canvas' top.
        let mut axis = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(rems(AXIS_H / 16.))
            .bg(ui::surface().opacity(0.92))
            .border_b_1()
            .border_color(ui::border())
            .overflow_hidden();
        for (day, label) in ticks {
            let (x, _) = c.at(l.x_of(day), 0., rem);
            axis = axis.child(
                div()
                    .absolute()
                    .left(x + px(4.))
                    .top(rems(5. / 16.))
                    .whitespace_nowrap()
                    .font_family(ui::mono())
                    .text_size(ui::code_rems(10.5))
                    .text_color(ui::faint())
                    .child(label),
            );
        }
        if today_on {
            let (x, _) = c.at(today_x, 0., rem);
            axis = axis.child(
                div()
                    .absolute()
                    .left(x - rems(22. / 16.).to_pixels(rem))
                    .top(rems(4. / 16.))
                    .child(ui::chip("Today", ui::accent())),
            );
        }
        children.push(axis.into_any_element());

        let minimap = shown
            .tasks
            .iter()
            .zip(&l.bars)
            .map(|(t, r)| (*r, task_tone(t, &l.groups).opacity(0.7)))
            .collect();
        let details = selected
            .and_then(|key| self.timeline_details(shown, key, &critical, &late, &l.groups, cx));
        Drawn {
            shapes,
            children,
            minimap,
            details,
            cursor: match &s.drag {
                Some(TimelineDrag::Link { .. }) => Some(CursorStyle::Crosshair),
                Some(TimelineDrag::Bar {
                    grip: Grip::Start | Grip::End,
                    ..
                }) => Some(CursorStyle::ResizeLeftRight),
                _ => None,
            },
            empty: shown
                .tasks
                .is_empty()
                .then_some("No tasks yet. Add one with +."),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn task_bar(
        &self,
        ix: usize,
        t: &Task,
        bar: Rect,
        tone: Hsla,
        on_path: bool,
        late: bool,
        scale: f32,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(s) = self.timeline() else {
            return div().into_any_element();
        };
        let picked = s.selected.as_deref() == Some(&format!("t:{}", t.id));
        let (x, y) = s.canvas.at(bar.x, bar.y, rem);
        let press = |grip: Grip| {
            let id = t.id.clone();
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.press_bar(&id, grip, event, window, cx)
            })
        };
        let link_id = t.id.clone();
        let link = div()
            .id(("genui-tl-link", ix))
            .debug_selector({
                let id = t.id.clone();
                move || format!("genui-tl-link-{id}")
            })
            .absolute()
            .right(px(-16. * scale))
            .top(px((bar.h / 2. - 5.) * scale))
            .size(px(10. * scale))
            .rounded_full()
            .bg(ui::surface())
            .border_2()
            .border_color(ui::accent())
            .cursor(CursorStyle::Crosshair)
            .when(!picked, |d| {
                d.opacity(0.).group_hover("genui-tl-bar", |d| d.opacity(1.))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_link(&link_id, event, window, cx)
                }),
            );
        let grip = |end: Grip, name: &'static str| {
            div()
                .id((name, ix))
                .debug_selector({
                    let id = t.id.clone();
                    move || format!("{name}-{id}")
                })
                .absolute()
                .top_0()
                .bottom_0()
                .w(px(GRIP * scale))
                .cursor(CursorStyle::ResizeLeftRight)
                .map(|d| {
                    if end == Grip::Start {
                        d.left_0()
                    } else {
                        d.right_0()
                    }
                })
                .on_mouse_down(MouseButton::Left, press(end))
        };
        let base = div()
            .id(("genui-tl-bar", ix))
            .debug_selector({
                let id = t.id.clone();
                move || format!("genui-tl-bar-{id}")
            })
            .group("genui-tl-bar")
            .absolute()
            .left(x)
            .top(y)
            .w(px(bar.w * scale))
            .h(px(bar.h * scale))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(MouseButton::Left, press(Grip::Move));
        if t.milestone {
            return base.rounded_full().child(link).into_any_element();
        }
        let progress = t.progress.map(|p| f32::from(p) / 100.);
        // The bar keeps its tone; the critical path shows as a heavier
        // `accent` hairline (a late task's `danger` one wins).
        base.rounded(px(6. * scale))
            .bg(tone.opacity(0.22))
            .border(px(if picked {
                2.
            } else if on_path {
                1.5
            } else {
                1.
            }))
            .border_color(if picked {
                ui::accent()
            } else if late {
                ui::danger()
            } else if on_path {
                ui::accent()
            } else {
                tone.opacity(0.7)
            })
            .hover(|d| d.bg(tone.opacity(0.3)))
            // The fill is clipped to the bar; the link handle sits outside it.
            .when_some(progress, |d, p| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(5. * scale))
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(px(bar.w * p * scale))
                                .bg(tone.opacity(0.45)),
                        ),
                )
            })
            .when_some(progress.filter(|_| bar.w * scale >= 44.), |d, p| {
                d.child(
                    div()
                        .absolute()
                        .left(px(8. * scale))
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .font_family(ui::mono())
                        .text_size(px(10. * scale * ui::code_scale()))
                        .text_color(ui::fg())
                        .child(format!("{}%", (p * 100.).round())),
                )
            })
            .child(grip(Grip::Start, "genui-tl-start"))
            .child(grip(Grip::End, "genui-tl-end"))
            .child(link)
            .into_any_element()
    }

    fn timeline_details(
        &self,
        shown: &Timeline,
        key: &str,
        critical: &[String],
        late: &[(usize, usize)],
        groups: &[(String, Rect)],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (what, rest) = key.split_once(':')?;
        if what == "t" {
            let ti = shown.index(rest)?;
            let t = &shown.tasks[ti];
            let subject = format!("task “{}”", t.label);
            let gone = t.id.clone();
            let rename = {
                let key = key.to_string();
                canvas::action("genui-tl-rename", IconName::Pencil, "Rename").on_click(
                    cx.listener(move |this, _, window, cx| this.timeline_rename(&key, window, cx)),
                )
            };
            let line =
                |text: String, tone: Hsla| div().text_color(tone).child(text).into_any_element();
            let mut rows = vec![line(span(t), ui::muted())];
            if let Some(p) = t.progress {
                rows.push(line(format!("{p}% done"), ui::muted()));
            }
            if !t.depends_on.is_empty() {
                let names: Vec<String> = t
                    .depends_on
                    .iter()
                    .map(|d| shown.task(d).map_or(d.clone(), |t| t.label.clone()))
                    .collect();
                rows.push(line(format!("After {}", names.join(", ")), ui::muted()));
            }
            for (di, _) in late.iter().filter(|(_, x)| *x == ti) {
                rows.push(line(
                    format!("Starts before “{}” ends", shown.tasks[*di].label),
                    ui::danger(),
                ));
            }
            if critical.len() > 1 && critical.contains(&t.id) {
                rows.push(line("On the critical path".into(), ui::accent()));
            }
            rows.push(canvas::actions(vec![
                rename,
                canvas::action("genui-tl-ask", IconName::MessageSquare, "Ask about this")
                    .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                canvas::action("genui-tl-delete", IconName::Trash, "Delete").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.timeline_remove(Change::RemoveTask { task: gone.clone() }, cx)
                    },
                )),
            ]));
            return Some(canvas::details(
                if t.milestone {
                    IconName::Flag
                } else {
                    IconName::ChartGantt
                },
                task_tone(t, groups),
                t.label.clone(),
                rows,
                cx,
            ));
        }
        let (task, on) = link_of(rest)?;
        let name = |id: &str| shown.task(id).map_or(id.to_string(), |t| t.label.clone());
        let title = format!("{} waits for {}", name(&task), name(&on));
        let subject = format!("the dependency “{title}”");
        Some(canvas::details(
            IconName::Link,
            ui::info(),
            title,
            vec![canvas::actions(vec![
                canvas::action("genui-tl-unlink", IconName::Unlink, "Remove").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.timeline_remove(
                            Change::RemoveDependency {
                                task: task.clone(),
                                on: on.clone(),
                            },
                            cx,
                        )
                    }),
                ),
                canvas::action("genui-tl-ask", IconName::MessageSquare, "Ask about this")
                    .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
            ])],
            cx,
        ))
    }

    fn timeline_pick(
        &mut self,
        key: String,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        if rename {
            self.timeline_rename(&key, window, cx);
        } else {
            self.canvas_select(Some(key), cx);
        }
    }

    fn press_bar(
        &mut self,
        id: &str,
        grip: Grip,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let key = format!("t:{id}");
        if event.click_count >= 2 {
            self.timeline_rename(&key, window, cx);
            return;
        }
        let rem = window.rem_size();
        if let Some(s) = state(self) {
            s.selected = Some(key);
            let x = s.canvas.to_world(event.position, rem).x;
            s.drag = Some(TimelineDrag::Bar {
                id: id.to_string(),
                grip,
                from: event.position,
                grab: x,
                at: x,
                moved: false,
            });
        }
        cx.notify();
    }

    fn press_link(
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
        let start = self.timeline().and_then(|s| {
            let (laid, _, l) = s.laid.as_ref()?;
            let bar = l.bars[laid.index(id)?];
            Some(P {
                x: bar.right(),
                y: bar.y + bar.h / 2.,
            })
        });
        if let (Some(start), Some(s)) = (start, state(self)) {
            let to = s.canvas.to_world(event.position, rem);
            s.drag = Some(TimelineDrag::Link {
                from: id.to_string(),
                start,
                to,
            });
        }
        cx.notify();
    }

    /// The background: a press near a dependency picks it.
    pub(super) fn timeline_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let rem = window.rem_size();
        let Some(s) = self.timeline() else {
            return false;
        };
        let Some((laid, _, l)) = &s.laid else {
            return false;
        };
        let at = s.canvas.to_world(event.position, rem);
        let tolerance = PICK / s.canvas.scale(rem);
        let hit = l
            .links
            .iter()
            .map(|(di, ti, path)| (*di, *ti, path.distance_to(at)))
            .filter(|(.., d)| *d <= tolerance)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(di, ti, _)| format!("d:{}\n{}", laid.tasks[ti].id, laid.tasks[di].id));
        match hit {
            Some(key) => {
                self.timeline_pick(key, false, window, cx);
                true
            }
            None => false,
        }
    }

    pub(super) fn timeline_move(
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
            Some(TimelineDrag::Bar {
                at, from, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *at = world.x;
            }
            Some(TimelineDrag::Link { to, .. }) if pressed => *to = world,
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn timeline_up(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = state(self) else { return };
        let dragged = s.dragged();
        let Some(drag) = s.drag.take() else { return };
        match drag {
            TimelineDrag::Bar { .. } => {
                if let Some((task, start, end)) = dragged {
                    self.timeline_edit(Change::SetDates { task, start, end }, cx);
                }
            }
            TimelineDrag::Link { from, to, .. } => {
                let Some((laid, _, l)) = &s.laid else { return };
                let Some(target) = l.task_at(to).map(|ix| laid.tasks[ix].id.clone()) else {
                    return;
                };
                if target == from {
                    return;
                }
                let key = format!("d:{target}\n{from}");
                if self.timeline_edit(
                    Change::AddDependency {
                        task: target,
                        on: from,
                    },
                    cx,
                ) {
                    self.canvas_select(Some(key), cx);
                }
            }
        }
    }

    fn timeline_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.timeline() else { return };
        let Some(id) = key.strip_prefix("t:") else {
            return;
        };
        let working = s.edits.working();
        let l = timeline::layout(working);
        let Some(ix) = working.index(id) else { return };
        let row = l.rows[ix];
        let text = working.tasks[ix].label.clone();
        let at = Rect {
            x: -LABEL_W + 4.,
            y: row.y + (ROW - BAR_H) / 2. - 4.,
            w: LABEL_W - 12.,
            h: BAR_H + 8.,
        };
        if let Some(s) = state(self) {
            s.selected = Some(key.to_string());
            s.drag = None;
        }
        self.start_canvas_edit(key.to_string(), text, at, window, cx);
    }

    pub(super) fn timeline_text(&mut self, target: &str, text: String, cx: &mut Context<Self>) {
        let Some(id) = target.strip_prefix("t:") else {
            return;
        };
        let unchanged = self
            .timeline()
            .and_then(|s| s.edits.working().task(id))
            .is_none_or(|t| t.label == text);
        if text.is_empty() || unchanged {
            return;
        }
        self.timeline_edit(Change::rename(id, text), cx);
    }

    fn timeline_remove(&mut self, change: Change, cx: &mut Context<Self>) {
        if self.timeline_edit(change, cx) {
            self.canvas_select(None, cx);
        }
    }

    /// The header's +: a five-day task after the last one (or from today),
    /// named at once.
    pub(super) fn add_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.timeline() else { return };
        let working = s.edits.working();
        let start = working
            .tasks
            .iter()
            .map(|t| t.end + 1)
            .max()
            .unwrap_or(s.today);
        let label = format!("Task {}", working.tasks.len() + 1);
        let task = Task::new(working.fresh_task_id(&label), label, start, start + 4);
        let key = format!("t:{}", task.id);
        if self.timeline_edit(Change::AddTask { task }, cx) {
            self.timeline_rename(&key, window, cx);
        }
    }

    pub(super) fn timeline_key(
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
        let Some(key) = self.timeline().and_then(|s| s.selected.clone()) else {
            return false;
        };
        let Some((what, rest)) = key.split_once(':') else {
            return false;
        };
        match (keystroke.key.as_str(), what) {
            ("backspace" | "delete", "t") => self.timeline_remove(
                Change::RemoveTask {
                    task: rest.to_string(),
                },
                cx,
            ),
            ("backspace" | "delete", _) => {
                let Some((task, on)) = link_of(rest) else {
                    return false;
                };
                self.timeline_remove(Change::RemoveDependency { task, on }, cx);
            }
            ("enter", "t") => self.timeline_rename(&key, window, cx),
            ("left" | "right", "t") if keystroke.modifiers.alt => {
                let Some(t) = self.timeline().and_then(|s| s.edits.working().task(rest)) else {
                    return false;
                };
                let by = if keystroke.key == "left" { -1 } else { 1 };
                let change = Change::SetDates {
                    task: t.id.clone(),
                    start: t.start + by,
                    end: t.end + by,
                };
                self.timeline_edit(change, cx);
            }
            ("escape", _) => self.canvas_select(None, cx),
            _ => return false,
        }
        true
    }
}
