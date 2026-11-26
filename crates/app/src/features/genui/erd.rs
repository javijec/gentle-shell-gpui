//! The erd kind on the shared canvas: tables as cards (name, then one row
//! per column with its key icon and type), relations as curves between
//! column rows ending in crow's-foot markers. Drag a table by its header
//! to move it, drag ● from a column to another to relate them,
//! double-click to rename; the details island edits columns and
//! cardinalities.

use std::rc::Rc;

use gentle_core::genui::edits::{Editable as _, Edits};
use gentle_core::genui::erd::{
    self, COLUMN_ROW, Cardinality, Change, Column, ColumnPatch, End, Erd, ErdLayout, HEADER,
    Relation, Table,
};
use gentle_core::genui::layout::{Point as P, Rect};
use gentle_core::genui::refs::CodeRef;
use gentle_core::genui::style::Tint;
use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::canvas::{self, Canvas, Drawn, Head, Shape, moved};
use super::diagram::{DiagramEvent, snap};
use super::look;
use super::panel::{Body, PanelView};
use super::share::RefState;
use crate::shared::ui;

/// How close (px) a press must be to a relation to pick it.
const PICK: f32 = 6.;

pub(super) enum ErdDrag {
    /// A table moving: where it was, where the press was (diagram units).
    Table {
        id: String,
        start: P,
        grab: P,
        at: P,
        from: Point<Pixels>,
        moved: bool,
    },
    /// A relation drawn from a column to the pointer.
    Connect { from: End, start: P, to: P },
}

pub(super) struct ErdState {
    pub edits: Edits<Erd>,
    pub canvas: Canvas,
    /// `t:<table>`, `c:<table>\n<column>` or `r:<relation>`.
    pub selected: Option<String>,
    pub drag: Option<ErdDrag>,
    laid: Option<(Erd, Rc<ErdLayout>)>,
}

impl ErdState {
    pub fn new(spec: Erd) -> Self {
        Self {
            edits: Edits::new(spec),
            canvas: Canvas::new(),
            selected: None,
            drag: None,
            laid: None,
        }
    }

    pub fn rebase(&mut self, spec: Erd) {
        self.edits.rebase(spec);
        self.drag = None;
        let working = self.edits.working();
        let gone = match self.selected.as_deref().and_then(|k| k.split_once(':')) {
            Some(("t", id)) => working.table(id).is_none(),
            Some(("c", end)) => end_of(end).is_none_or(|end| !working.has(&end)),
            Some((_, id)) => working.relation(id).is_none(),
            None => false,
        };
        if gone {
            self.selected = None;
        }
    }

    /// The working spec with a table being dragged where it would land, or
    /// `None` when no table drag moves anything (the working spec is shown).
    fn preview(&self) -> Option<Erd> {
        let Some(ErdDrag::Table {
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
            table: id.clone(),
            x: snap(start.x + at.x - grab.x),
            y: snap(start.y + at.y - grab.y),
        });
        Some(shown)
    }

    /// The layout of `preview`, else of the working spec.
    fn layout_of(&mut self, preview: Option<&Erd>) -> Rc<ErdLayout> {
        let spec = preview.unwrap_or(self.edits.working());
        match &self.laid {
            Some((laid, layout)) if laid == spec => layout.clone(),
            _ => {
                let layout = Rc::new(erd::layout(spec));
                self.laid = Some((spec.clone(), layout.clone()));
                layout
            }
        }
    }
}

/// `table\ncolumn` back to an end.
fn end_of(key: &str) -> Option<End> {
    let (table, column) = key.split_once('\n')?;
    Some(End::new(table, column))
}

fn column_key(prefix: &str, end: &End) -> String {
    format!("{prefix}:{}\n{}", end.table, end.column)
}

fn state(this: &mut PanelView) -> Option<&mut ErdState> {
    match &mut this.body {
        Some(Body::Erd(s)) => Some(s),
        _ => None,
    }
}

/// A crow's-foot end at `p`, `d` pointing out of the table: three prongs
/// for "many" or a bar for "one", then a bar (required) or a ring
/// (optional) further out, all stroked like the relation's line.
fn marker(p: P, d: P, many: bool, optional: bool, color: Hsla, shapes: &mut Vec<Shape>) {
    let along = |k: f32| P {
        x: p.x + d.x * k,
        y: p.y + d.y * k,
    };
    let side = |q: P, k: f32| P {
        x: q.x - d.y * k,
        y: q.y + d.x * k,
    };
    let line = |from: P, to: P| Shape::Line {
        from,
        to,
        color,
        width: look::EDGE_WIDTH,
        dash: None,
    };
    if many {
        let apex = along(14.);
        shapes.push(line(apex, side(p, 7.)));
        shapes.push(line(apex, side(p, -7.)));
        shapes.push(line(apex, p));
    } else {
        let at = along(8.);
        shapes.push(line(side(at, 7.), side(at, -7.)));
    }
    if optional {
        let c = along(23.);
        shapes.push(Shape::Box {
            rect: Rect {
                x: c.x - 4.,
                y: c.y - 4.,
                w: 8.,
                h: 8.,
            },
            fill: ui::well(),
            border: Some(color),
            border_width: 1.,
            radius: 4.,
            dashed: false,
        });
    } else {
        let at = along(20.);
        shapes.push(line(side(at, 7.), side(at, -7.)));
    }
}

/// A table's tone: its `color`, else the next in a fixed cycle so tables
/// read apart without the agent choosing.
fn table_tone(t: &Table, ix: usize) -> Hsla {
    look::tone(t.color, ui::tint(Tint::cycle(ix)))
}

fn short(c: Cardinality) -> String {
    c.name().replace('-', ":")
}

impl PanelView {
    fn erd(&self) -> Option<&ErdState> {
        match &self.body {
            Some(Body::Erd(s)) => Some(s),
            _ => None,
        }
    }

    fn erd_edit(&mut self, change: Change, cx: &mut Context<Self>) -> bool {
        self.edit_with(cx, |body| match body {
            Body::Erd(s) => Some(s.edits.edit(&change)),
            _ => None,
        })
    }

    pub(super) fn render_erd(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rem = window.rem_size();
        let Some(s) = state(self) else {
            return div().into_any_element();
        };
        let preview = s.preview();
        let l = s.layout_of(preview.as_ref());
        let tweening = s.canvas.prepare(l.bounds, rem);
        self.canvas_frames(tweening, window, cx);
        let drawn = self.erd_drawn(preview.as_ref(), &l, rem, cx);
        self.render_canvas(drawn, rem, cx)
    }

    fn erd_drawn(
        &self,
        preview: Option<&Erd>,
        l: &ErdLayout,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> Drawn {
        let Some(s) = self.erd() else {
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
        let selected = s.selected.as_deref();
        let mut shapes = Vec::new();
        let mut children: Vec<AnyElement> = Vec::new();
        let nullable = |end: &End| {
            shown
                .table(&end.table)
                .and_then(|t| t.column(&end.column))
                .is_some_and(|c| c.nullable)
        };
        for (r, g) in shown.relations.iter().zip(&l.relations) {
            let picked = selected == Some(&format!("r:{}", r.id));
            let color = if picked { ui::accent() } else { look::line() };
            shapes.push(Shape::Curve {
                path: g.path.clone(),
                color,
                width: if picked { 2. } else { look::EDGE_WIDTH },
                dash: None,
                head: Head::None,
            });
            // An end is optional when the other end's column may be null
            // (an order with no user: zero or one user).
            let (many_from, many_to) = r.cardinality.many();
            marker(
                g.path.start(),
                g.from_dir,
                many_from,
                nullable(&r.to),
                color,
                &mut shapes,
            );
            marker(
                g.path.end(),
                g.to_dir,
                many_to,
                nullable(&r.from),
                color,
                &mut shapes,
            );
        }
        if let Some(ErdDrag::Connect { start, to, .. }) = &s.drag {
            shapes.push(Shape::Rubber {
                from: *start,
                to: *to,
            });
        }

        // Relation pills: the cardinality (and label) halfway, on the cross run.
        for (ix, (r, g)) in shown.relations.iter().zip(&l.relations).enumerate() {
            if scale < 0.35 {
                break;
            }
            let p = g.path.point_at(0.5);
            let picked = selected == Some(&format!("r:{}", r.id));
            let text = match &r.label {
                Some(label) => format!("{} · {label}", short(r.cardinality)),
                None => short(r.cardinality),
            };
            let (x, y) = c.at(p.x - 70., p.y - 10., rem);
            let id = r.id.clone();
            children.push(
                h_flex()
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(px(140. * scale))
                    .h(px(20. * scale))
                    .justify_center()
                    .child(
                        look::label_pill(scale)
                            .id(("genui-erd-relation", ix))
                            .debug_selector({
                                let id = r.id.clone();
                                move || format!("genui-erd-relation-{id}")
                            })
                            .when(picked, |d| {
                                d.border_color(ui::accent().opacity(0.6))
                                    .text_color(ui::accent())
                            })
                            .max_w_full()
                            .truncate()
                            .cursor_pointer()
                            .font_family(ui::mono())
                            .text_size(px(10.5 * scale * ui::code_scale()))
                            .line_height(px(13. * scale * ui::code_scale()))
                            .child(text)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.erd_pick(
                                        format!("r:{id}"),
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

        let visible = c.visible(rem).inflate(40.);
        for (ti, (t, rect)) in shown.tables.iter().zip(&l.tables).enumerate() {
            if !rect.intersects(&visible) {
                continue;
            }
            children.push(self.table_card(ti, t, *rect, scale, rem, cx));
        }

        let minimap = shown
            .tables
            .iter()
            .zip(&l.tables)
            .enumerate()
            .map(|(ti, (t, r))| (*r, table_tone(t, ti).opacity(0.6)))
            .collect();
        let details = selected.and_then(|key| self.erd_details(shown, key, cx));
        Drawn {
            shapes,
            children,
            minimap,
            details,
            cursor: matches!(s.drag, Some(ErdDrag::Connect { .. }))
                .then_some(CursorStyle::Crosshair),
            empty: shown
                .tables
                .is_empty()
                .then_some("No tables yet. Add one with +."),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn table_card(
        &self,
        ti: usize,
        t: &Table,
        rect: Rect,
        scale: f32,
        rem: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(s) = self.erd() else {
            return div().into_any_element();
        };
        let selected = s.selected.as_deref();
        let picked = selected == Some(&format!("t:{}", t.id));
        let dragging = picked && matches!(&s.drag, Some(ErdDrag::Table { moved: true, .. }));
        let (x, y) = s.canvas.at(rect.x, rect.y, rem);
        let id = t.id.clone();
        let tone = table_tone(t, ti);
        let icon = t.icon.and_then(look::icon).unwrap_or(IconName::Database);
        let header = h_flex()
            .id(("genui-erd-table", ti))
            .debug_selector({
                let id = t.id.clone();
                move || format!("genui-erd-table-{id}")
            })
            .h(px(HEADER * scale))
            .flex_shrink_0()
            .gap(px(8. * scale))
            .px(px(10. * scale))
            .border_b_1()
            .border_color(look::edge_color(tone))
            // The card keeps its corners: the handles hang outside it, so
            // it can't clip the header.
            .rounded_t(px((look::CARD_RADIUS - 1.) * scale))
            .bg(tone.opacity(0.14))
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_table(&id, event, window, cx)
                }),
            )
            .child(look::badge(tone, icon, 22., scale))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(13. * scale))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::fg())
                    .child(t.name.clone()),
            )
            .child(
                div()
                    .font_family(ui::mono())
                    .text_size(px(10.5 * scale * ui::code_scale()))
                    .text_color(ui::faint())
                    .child(t.columns.len().to_string()),
            )
            .when_some(
                t.code_ref.clone().filter(|_| scale >= 0.5),
                |d, code_ref| d.child(self.erd_ref_button(ti, &t.id, code_ref, scale, cx)),
            );
        let rows = t.columns.iter().map(|col| {
            let end = End::new(&t.id, &col.name);
            let key = column_key("c", &end);
            let row_picked = selected == Some(key.as_str());
            let handle = |left: bool| {
                let end = end.clone();
                let name = format!(
                    "genui-erd-handle-{}.{}-{}",
                    end.table,
                    end.column,
                    if left { "l" } else { "r" }
                );
                div()
                    .debug_selector(move || name)
                    .absolute()
                    .top(px((COLUMN_ROW / 2. - 5.) * scale))
                    .size(px(10. * scale))
                    .rounded_full()
                    .bg(ui::surface())
                    .border_2()
                    .border_color(ui::accent())
                    .cursor(CursorStyle::Crosshair)
                    .map(|d| {
                        if left {
                            d.left(px(-5. * scale))
                        } else {
                            d.right(px(-5. * scale))
                        }
                    })
                    .opacity(0.)
                    .group_hover("genui-erd-column", |d| d.opacity(1.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_column_handle(&end, left, event, window, cx)
                        }),
                    )
            };
            let (icon, tone) = if col.pk {
                (Some(IconName::KeyRound), ui::warning())
            } else if col.fk.is_some() {
                (Some(IconName::Link), ui::info())
            } else {
                (None, ui::faint())
            };
            let target = key.clone();
            h_flex()
                .id(SharedString::from(format!(
                    "genui-erd-col-{}.{}",
                    t.id, col.name
                )))
                .debug_selector({
                    let name = format!("genui-erd-col-{}.{}", t.id, col.name);
                    move || name
                })
                .group("genui-erd-column")
                .relative()
                .h(px(COLUMN_ROW * scale))
                .flex_shrink_0()
                .gap(px(6. * scale))
                .px(px(10. * scale))
                .when(row_picked, |d| d.bg(ui::accent().opacity(0.10)))
                .hover(|d| d.bg(ui::surface_hover()))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.erd_pick(target.clone(), event.click_count >= 2, window, cx)
                    }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(13. * scale))
                        .flex()
                        .justify_center()
                        .children(
                            icon.map(|icon| Icon::new(icon).size(px(11. * scale)).text_color(tone)),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12. * scale))
                        .text_color(if col.pk { ui::fg() } else { ui::muted() })
                        .when(col.pk, |d| d.font_weight(FontWeight::MEDIUM))
                        .child(col.name.clone()),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .max_w(px(90. * scale))
                        .truncate()
                        .font_family(ui::mono())
                        .text_size(px(10.5 * scale * ui::code_scale()))
                        .text_color(ui::faint())
                        .child(format!(
                            "{}{}{}",
                            col.ty.as_deref().unwrap_or(""),
                            if col.nullable { "?" } else { "" },
                            if col.unique && !col.pk { " uq" } else { "" }
                        )),
                )
                .child(handle(true))
                .child(handle(false))
        });
        look::card(tone, scale)
            .flex()
            .flex_col()
            .absolute()
            .left(x)
            .top(y)
            .w(px(rect.w * scale))
            .h(px(rect.h * scale))
            .when(picked, |d| d.border_color(ui::accent()))
            .when(dragging, |d| d.shadow_lg())
            .child(header)
            .children(rows)
            .when(t.columns.is_empty(), |d| {
                d.child(
                    div()
                        .px(px(10. * scale))
                        .h(px(COLUMN_ROW * scale))
                        .flex()
                        .items_center()
                        .text_size(px(11. * scale))
                        .text_color(ui::faint())
                        .child("no columns"),
                )
            })
            .into_any_element()
    }

    /// A table's code link in its header: the file icon, muted when the
    /// file isn't there; a click opens it (the details show the path).
    fn erd_ref_button(
        &self,
        ti: usize,
        table: &str,
        code_ref: CodeRef,
        scale: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = format!("genui-erd-ref-{table}");
        let found = matches!(
            self.env.ref_state(&code_ref),
            RefState::Found(_) | RefState::Unknown
        );
        div()
            .id(("genui-erd-ref", ti))
            .debug_selector(move || selector)
            .flex_shrink_0()
            .size(px(18. * scale))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5. * scale))
            .cursor_pointer()
            .text_color(if found { ui::accent() } else { ui::faint() })
            .hover(|d| d.bg(ui::surface_hover()))
            .tooltip({
                let tip: SharedString = code_ref.text().into();
                move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                }
            })
            .child(
                Icon::new(if found {
                    IconName::FileCode
                } else {
                    IconName::FileX
                })
                .size(px(12. * scale)),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(DiagramEvent::OpenRef(code_ref.clone()));
            }))
            .into_any_element()
    }

    fn erd_details(&self, shown: &Erd, key: &str, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (what, rest) = key.split_once(':')?;
        let rename =
            |id: &'static str, label: &'static str, target: String, cx: &mut Context<Self>| {
                canvas::action(id, IconName::Pencil, label).on_click(
                    cx.listener(move |this, _, window, cx| this.erd_rename(&target, window, cx)),
                )
            };
        // A table (and its columns) take the table's tone, as on its card.
        let tone_of = |t: &Table| {
            let ix = shown.tables.iter().position(|x| x.id == t.id);
            table_tone(t, ix.unwrap_or(0))
        };
        match what {
            "t" => {
                let t = shown.table(rest)?;
                let subject = format!("table “{}”", t.name);
                let (add, gone) = (t.id.clone(), t.id.clone());
                let mut rows = vec![
                    div()
                        .text_color(ui::muted())
                        .child(format!(
                            "{} · {}",
                            gentle_core::genui::edits::plural(t.columns.len(), "column"),
                            t.id
                        ))
                        .into_any_element(),
                ];
                rows.extend(t.code_ref.as_ref().map(|code_ref| {
                    super::refs::chip(
                        format!("genui-erd-details-ref-{}", t.id),
                        code_ref,
                        &self.env,
                        cx,
                    )
                }));
                rows.push(canvas::actions(vec![
                    rename("genui-erd-rename", "Rename", key.to_string(), cx),
                    canvas::action("genui-erd-add-column", IconName::Plus, "Column").on_click(
                        cx.listener(move |this, _, window, cx| this.add_column(&add, window, cx)),
                    ),
                    canvas::action("genui-erd-ask", IconName::MessageSquare, "Ask about this")
                        .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                    canvas::action("genui-erd-delete", IconName::Trash, "Delete").on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.erd_remove(
                                Change::RemoveTable {
                                    table: gone.clone(),
                                },
                                cx,
                            )
                        }),
                    ),
                ]));
                Some(canvas::details(
                    t.icon.and_then(look::icon).unwrap_or(IconName::Database),
                    tone_of(t),
                    t.name.clone(),
                    rows,
                    cx,
                ))
            }
            "c" => {
                let end = end_of(rest)?;
                let t = shown.table(&end.table)?;
                let col = t.column(&end.column)?;
                let flag = |name: &'static str, on: bool, cx: &mut Context<Self>| {
                    let end = end.clone();
                    canvas::pills(
                        "genui-erd-flag",
                        &[name],
                        if on { name } else { "" },
                        move |this, _, cx| {
                            let mut patch = ColumnPatch::default();
                            match name {
                                "pk" => patch.pk = Some(!on),
                                "nullable" => patch.nullable = Some(!on),
                                _ => patch.unique = Some(!on),
                            }
                            this.erd_edit(
                                Change::UpdateColumn {
                                    table: end.table.clone(),
                                    column: end.column.clone(),
                                    patch,
                                },
                                cx,
                            );
                        },
                        cx,
                    )
                };
                let subject = format!("column {}.{}", t.name, col.name);
                let gone = end.clone();
                let mut rows = vec![
                    div()
                        .font_family(ui::mono())
                        .text_size(ui::code_rems(12.))
                        .text_color(ui::muted())
                        .child(col.ty.clone().unwrap_or_else(|| "no type".into()))
                        .into_any_element(),
                    h_flex()
                        .gap_1()
                        .child(flag("pk", col.pk, cx))
                        .child(flag("nullable", col.nullable, cx))
                        .child(flag("unique", col.unique, cx))
                        .into_any_element(),
                ];
                rows.extend(col.fk.as_ref().map(|fk| {
                    div()
                        .text_color(ui::info())
                        .child(format!("→ {}.{}", fk.table, fk.column))
                        .into_any_element()
                }));
                rows.push(canvas::actions(vec![
                    rename("genui-erd-rename", "Rename", key.to_string(), cx),
                    rename("genui-erd-type", "Type", column_key("y", &end), cx),
                    canvas::action("genui-erd-ask", IconName::MessageSquare, "Ask about this")
                        .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                    canvas::action("genui-erd-delete", IconName::Trash, "Delete").on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.erd_remove(
                                Change::RemoveColumn {
                                    table: gone.table.clone(),
                                    column: gone.column.clone(),
                                },
                                cx,
                            )
                        }),
                    ),
                ]));
                Some(canvas::details(
                    if col.pk {
                        IconName::KeyRound
                    } else {
                        IconName::Table
                    },
                    tone_of(t),
                    format!("{}.{}", t.name, col.name),
                    rows,
                    cx,
                ))
            }
            _ => {
                let r = shown.relation(rest)?;
                let title = format!(
                    "{}.{} → {}.{}",
                    r.from.table, r.from.column, r.to.table, r.to.column
                );
                let subject = format!("the relation {title}");
                let (id, gone) = (r.id.clone(), r.id.clone());
                Some(canvas::details(
                    IconName::Link,
                    ui::info(),
                    title,
                    vec![
                        canvas::pills(
                            "genui-erd-cardinality",
                            &Cardinality::ALL.map(Cardinality::name),
                            r.cardinality.name(),
                            move |this, value, cx| {
                                this.erd_edit(
                                    Change::UpdateRelation {
                                        relation: id.clone(),
                                        cardinality: Cardinality::parse(value),
                                        label: None,
                                    },
                                    cx,
                                );
                            },
                            cx,
                        ),
                        canvas::actions(vec![
                            rename("genui-erd-rename", "Label", key.to_string(), cx),
                            canvas::action(
                                "genui-erd-ask",
                                IconName::MessageSquare,
                                "Ask about this",
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.ask(&subject, cx))),
                            canvas::action("genui-erd-delete", IconName::Trash, "Delete").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.erd_remove(
                                        Change::RemoveRelation {
                                            relation: gone.clone(),
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
        }
    }

    /// Select `key` (a column or a relation); a double-click renames it.
    fn erd_pick(&mut self, key: String, rename: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        if rename {
            self.erd_rename(&key, window, cx);
        } else {
            self.canvas_select(Some(key), cx);
        }
    }

    fn press_table(
        &mut self,
        id: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let key = format!("t:{id}");
        if event.click_count >= 2 {
            self.erd_rename(&key, window, cx);
            return;
        }
        let rem = window.rem_size();
        let Some(rect) = self
            .erd()
            .and_then(|s| Some(s.laid.as_ref()?.1.tables[s.edits.working().table_index(id)?]))
        else {
            return;
        };
        if let Some(s) = state(self) {
            s.selected = Some(key);
            let grab = s.canvas.to_world(event.position, rem);
            s.drag = Some(ErdDrag::Table {
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

    fn press_column_handle(
        &mut self,
        end: &End,
        left: bool,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.focus(&self.focus, cx);
        self.finish_canvas_edit(true, cx);
        let rem = window.rem_size();
        let start = self.erd().and_then(|s| {
            let (_, l) = s.laid.as_ref()?;
            let working = s.edits.working();
            let t = working.table_index(&end.table)?;
            let c = working.tables[t]
                .columns
                .iter()
                .position(|c| c.name == end.column)?;
            let rect = l.tables[t];
            Some(P {
                x: if left { rect.x } else { rect.right() },
                y: erd::column_y(&rect, c),
            })
        });
        if let (Some(start), Some(s)) = (start, state(self)) {
            let to = s.canvas.to_world(event.position, rem);
            s.drag = Some(ErdDrag::Connect {
                from: end.clone(),
                start,
                to,
            });
        }
        cx.notify();
    }

    /// The background: a press near a relation picks it.
    pub(super) fn erd_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let rem = window.rem_size();
        let Some(s) = self.erd() else { return false };
        let Some((_, l)) = &s.laid else { return false };
        let at = s.canvas.to_world(event.position, rem);
        let tolerance = PICK / s.canvas.scale(rem);
        let hit = s
            .edits
            .working()
            .relations
            .iter()
            .zip(&l.relations)
            .map(|(r, g)| (r.id.clone(), g.path.distance_to(at)))
            .filter(|(_, d)| *d <= tolerance)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        match hit {
            Some((id, _)) => {
                self.erd_pick(format!("r:{id}"), event.click_count >= 2, window, cx);
                true
            }
            None => false,
        }
    }

    pub(super) fn erd_move(
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
            Some(ErdDrag::Table {
                at, from, moved: m, ..
            }) if pressed => {
                *m |= moved(*from, event.position);
                *at = world;
            }
            Some(ErdDrag::Connect { to, .. }) if pressed => *to = world,
            _ => return,
        }
        cx.notify();
    }

    pub(super) fn erd_up(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = state(self) else { return };
        let Some(drag) = s.drag.take() else { return };
        match drag {
            ErdDrag::Table {
                id,
                start,
                grab,
                at,
                moved: true,
                ..
            } => {
                self.erd_edit(
                    Change::Move {
                        table: id,
                        x: snap(start.x + at.x - grab.x),
                        y: snap(start.y + at.y - grab.y),
                    },
                    cx,
                );
            }
            ErdDrag::Connect { from, to, .. } => {
                let working = s.edits.working().clone();
                let Some((_, l)) = &s.laid else { return };
                let Some((t, c)) = l.column_at(&working, to) else {
                    return;
                };
                let target = End::new(&working.tables[t].id, &working.tables[t].columns[c].name);
                if target == from {
                    return;
                }
                let relation = Relation {
                    id: working.fresh_relation_id(&from, &target),
                    cardinality: working.implied(&from, &target),
                    from,
                    to: target,
                    label: None,
                };
                let key = format!("r:{}", relation.id);
                if self.erd_edit(Change::AddRelation { relation }, cx) {
                    self.canvas_select(Some(key), cx);
                }
            }
            _ => {}
        }
    }

    /// The rename field for a table, a column's name or type, or a
    /// relation's label, placed on the working spec's layout (it may have
    /// just changed).
    fn erd_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.erd() else { return };
        let working = s.edits.working();
        let l = erd::layout(working);
        let Some((what, rest)) = key.split_once(':') else {
            return;
        };
        let found = match what {
            "t" => working.table_index(rest).map(|t| {
                let r = l.tables[t];
                (
                    working.tables[t].name.clone(),
                    Rect {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: HEADER,
                    },
                )
            }),
            "c" | "y" => end_of(rest).and_then(|end| {
                let t = working.table_index(&end.table)?;
                let c = working.tables[t]
                    .columns
                    .iter()
                    .position(|c| c.name == end.column)?;
                let r = l.tables[t];
                let col = &working.tables[t].columns[c];
                let text = if what == "c" {
                    col.name.clone()
                } else {
                    col.ty.clone().unwrap_or_default()
                };
                Some((
                    text,
                    Rect {
                        x: r.x,
                        y: erd::column_y(&r, c) - COLUMN_ROW / 2.,
                        w: r.w,
                        h: COLUMN_ROW,
                    },
                ))
            }),
            _ => working
                .relations
                .iter()
                .position(|r| r.id == rest)
                .map(|ix| {
                    let p = l.relations[ix].path.point_at(0.5);
                    (
                        working.relations[ix].label.clone().unwrap_or_default(),
                        Rect {
                            x: p.x - 80.,
                            y: p.y - 12.,
                            w: 160.,
                            h: 24.,
                        },
                    )
                }),
        };
        let Some((text, at)) = found else { return };
        let select = match what {
            "y" => format!("c:{rest}"),
            _ => key.to_string(),
        };
        if let Some(s) = state(self) {
            s.selected = Some(select);
            s.drag = None;
        }
        self.start_canvas_edit(key.to_string(), text, at, window, cx);
    }

    pub(super) fn erd_text(&mut self, target: &str, text: String, cx: &mut Context<Self>) {
        let Some(s) = self.erd() else { return };
        let working = s.edits.working();
        let Some((what, rest)) = target.split_once(':') else {
            return;
        };
        let change = match what {
            "t" => {
                if text.is_empty() || working.table(rest).is_none_or(|t| t.name == text) {
                    return;
                }
                Change::UpdateTable {
                    table: rest.to_string(),
                    name: Some(text),
                    note: None,
                }
            }
            "c" | "y" => {
                let Some(end) = end_of(rest) else { return };
                let Some(col) = working
                    .table(&end.table)
                    .and_then(|t| t.column(&end.column))
                else {
                    return;
                };
                let patch = if what == "c" {
                    if text.is_empty() || col.name == text {
                        return;
                    }
                    ColumnPatch {
                        name: Some(text.clone()),
                        ..ColumnPatch::default()
                    }
                } else {
                    let ty = (!text.is_empty()).then_some(text);
                    if col.ty == ty {
                        return;
                    }
                    ColumnPatch {
                        ty: Some(ty),
                        ..ColumnPatch::default()
                    }
                };
                let renamed = patch.name.clone();
                let change = Change::UpdateColumn {
                    table: end.table.clone(),
                    column: end.column.clone(),
                    patch,
                };
                if self.erd_edit(change, cx)
                    && let Some(name) = renamed
                {
                    self.canvas_select(Some(column_key("c", &End::new(&end.table, &name))), cx);
                }
                return;
            }
            _ => {
                let label = (!text.is_empty()).then_some(text);
                if working.relation(rest).is_none_or(|r| r.label == label) {
                    return;
                }
                Change::UpdateRelation {
                    relation: rest.to_string(),
                    cardinality: None,
                    label: Some(label),
                }
            }
        };
        self.erd_edit(change, cx);
    }

    fn erd_remove(&mut self, change: Change, cx: &mut Context<Self>) {
        if self.erd_edit(change, cx) {
            self.canvas_select(None, cx);
        }
    }

    /// A column at the end of `table`, named at once.
    fn add_column(&mut self, table: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.erd().and_then(|s| s.edits.working().table(table)) else {
            return;
        };
        let column = Column::new(t.fresh_column("column"), Some("text".into()));
        let key = column_key("c", &End::new(table, &column.name));
        if self.erd_edit(
            Change::AddColumn {
                table: table.to_string(),
                column,
            },
            cx,
        ) {
            self.erd_rename(&key, window, cx);
        }
    }

    /// The header's +: a table with an id column, named at once.
    pub(super) fn add_table(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.erd() else { return };
        let working = s.edits.working();
        let name = format!("table_{}", working.tables.len() + 1);
        let mut table = Table::new(working.fresh_table_id(&name), name);
        let mut id = Column::new("id", Some("uuid".into()));
        id.pk = true;
        table.columns.push(id);
        let key = format!("t:{}", table.id);
        if self.erd_edit(Change::AddTable { table }, cx) {
            self.erd_rename(&key, window, cx);
        }
    }

    pub(super) fn erd_key(
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
        let Some(key) = self.erd().and_then(|s| s.selected.clone()) else {
            return false;
        };
        let Some((what, rest)) = key.split_once(':') else {
            return false;
        };
        match keystroke.key.as_str() {
            "backspace" | "delete" => {
                let change = match what {
                    "t" => Change::RemoveTable {
                        table: rest.to_string(),
                    },
                    "c" => {
                        let Some(end) = end_of(rest) else {
                            return false;
                        };
                        Change::RemoveColumn {
                            table: end.table,
                            column: end.column,
                        }
                    }
                    _ => Change::RemoveRelation {
                        relation: rest.to_string(),
                    },
                };
                self.erd_remove(change, cx);
            }
            "enter" => self.erd_rename(&key, window, cx),
            "escape" => self.canvas_select(None, cx),
            _ => return false,
        }
        true
    }
}
