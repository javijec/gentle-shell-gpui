//! The canvas kinds (sequence, erd, state, timeline): the real
//! [`PanelView`] in a headless window, edited by dragging, clicking and
//! typing, checking the `/gentle-ui:event` command Send hands over.

use gpui_kit::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, point, px};
use serde_json::{Value, json};

use super::ops;
use super::panels::{Host, bounds, click, double_click, drag, last_event, open, pending};

/// A canvas kind, drawn twice: the first frame measures the canvas, the
/// second fits the content in it.
fn canvas<'a>(
    cx: &'a mut TestAppContext,
    kind: &str,
    spec: Value,
) -> (Entity<Host>, &'a mut VisualTestContext) {
    let (host, cx) = open(cx, kind, spec);
    for _ in 0..2 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
    (host, cx)
}

#[gpui::test]
fn sequence_draw_a_message_reorder_and_retype(cx: &mut TestAppContext) {
    let (host, cx) = canvas(
        cx,
        "sequence",
        json!({
            "participants": [
                {"id": "user", "label": "User", "kind": "actor"},
                {"id": "api", "label": "API"},
                {"id": "db", "label": "Orders DB", "kind": "db"}
            ],
            "messages": [
                {"from": "user", "to": "api", "label": "POST /orders"},
                {"from": "api", "to": "db", "label": "insert"}
            ]
        }),
    );
    // From User's lifeline to the DB's, below the last message.
    let user = bounds(cx, "genui-seq-lifeline-user");
    let db = bounds(cx, "genui-seq-lifeline-db");
    let y = user.bottom() - px(12.);
    drag(cx, point(user.center().x, y), point(db.center().x, y));
    cx.simulate_input("audit");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(pending(&host, cx), 1);

    // The DB's header to the far left.
    let header = bounds(cx, "genui-seq-participant-db").center();
    let first = bounds(cx, "genui-seq-participant-user").center();
    drag(cx, header, point(first.x, header.y));
    assert!(
        bounds(cx, "genui-seq-participant-db").origin.x
            < bounds(cx, "genui-seq-participant-user").origin.x
    );

    // The first message becomes async from its details.
    click(cx, "genui-seq-message-m1");
    click(cx, "genui-seq-message-kind-async");

    click(cx, "genui-panel-send");
    let event = last_event(&host, cx);
    assert_eq!(
        ops(&event),
        vec!["order_participants", "add_message", "update_message"]
    );
    assert_eq!(event["changes"][1]["message"]["label"], "audit");
    assert_eq!(event["changes"][1]["index"], 2);
    assert_eq!(
        event["summary"],
        json!([
            "reordered the participants: Orders DB, User, API",
            "added message “audit” from User to Orders DB (step 3)",
            "made “POST /orders” async"
        ])
    );
}

#[gpui::test]
fn erd_relate_columns_move_and_rename_a_table(cx: &mut TestAppContext) {
    let (host, cx) = canvas(
        cx,
        "erd",
        json!({"tables": [
            {"id": "users", "columns": [{"name": "id", "type": "uuid", "pk": true}, "email"]},
            {"id": "orders", "columns": [{"name": "id", "type": "uuid", "pk": true}, {"name": "user_id", "type": "uuid"}]}
        ]}),
    );
    // orders.user_id's handle onto users.id.
    let handle = bounds(cx, "genui-erd-handle-orders.user_id-r").center();
    let target = bounds(cx, "genui-erd-col-users.id").center();
    drag(cx, handle, target);
    assert_eq!(pending(&host, cx), 1);

    // Move users by its header, then rename it.
    let header = bounds(cx, "genui-erd-table-users").center();
    drag(cx, header, point(header.x + px(120.), header.y + px(40.)));
    double_click(cx, "genui-erd-table-users");
    cx.simulate_input("accounts");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    click(cx, "genui-panel-send");
    let event = last_event(&host, cx);
    assert_eq!(ops(&event), vec!["update_table", "move", "add_relation"]);
    assert_eq!(event["changes"][2]["relation"]["from"], "orders.user_id");
    assert_eq!(event["changes"][2]["relation"]["cardinality"], "n-1");
    assert_eq!(
        event["summary"][2],
        "related orders.user_id → accounts.id (n-1)"
    );
}

#[gpui::test]
fn state_connect_name_and_change_a_kind(cx: &mut TestAppContext) {
    let (host, cx) = canvas(
        cx,
        "state",
        json!({
            "states": [{"id": "start", "kind": "initial"}, "Idle", "Running"],
            "transitions": [
                {"from": "start", "to": "Idle"},
                {"from": "Idle", "to": "Running", "event": "start"}
            ],
            "current": "Idle"
        }),
    );
    let handle = bounds(cx, "genui-state-handle-Running").center();
    let idle = bounds(cx, "genui-state-Idle").center();
    drag(cx, handle, idle);
    cx.simulate_input("stop");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    click(cx, "genui-state-Running");
    click(cx, "genui-state-kind-choice");

    click(cx, "genui-panel-send");
    let event = last_event(&host, cx);
    assert_eq!(ops(&event), vec!["update_state", "add_transition"]);
    assert_eq!(event["changes"][1]["transition"]["event"], "stop");
    assert_eq!(
        event["summary"],
        json!([
            "made “Running” a choice state",
            "added transition “stop” (“Running” → “Idle”)"
        ])
    );
}

#[gpui::test]
fn state_dragging_a_state_moves_it(cx: &mut TestAppContext) {
    let (host, cx) = canvas(
        cx,
        "state",
        json!({
            "states": [{"id": "start", "kind": "initial"}, "Idle", "Running"],
            "transitions": [
                {"from": "start", "to": "Idle"},
                {"from": "Idle", "to": "Running", "event": "start"}
            ]
        }),
    );
    let before = bounds(cx, "genui-state-Idle");
    let from = before.center();
    let to = point(from.x + px(120.), from.y + px(40.));
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for step in 1..=6 {
        let t = step as f32 / 6.;
        let at = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
        cx.simulate_mouse_move(at, Some(MouseButton::Left), Modifiers::none());
        cx.run_until_parked();
    }
    // Mid-drag the card is drawn where it would land; nothing is recorded yet.
    let during = bounds(cx, "genui-state-Idle");
    assert!(during.origin.x > before.origin.x + px(60.));
    assert!(during.origin.y > before.origin.y + px(20.));
    assert_eq!(pending(&host, cx), 0);

    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(pending(&host, cx), 1);
    assert_eq!(bounds(cx, "genui-state-Idle").origin, during.origin);

    click(cx, "genui-panel-send");
    let event = last_event(&host, cx);
    assert_eq!(ops(&event), vec!["move"]);
    assert_eq!(event["changes"][0]["state"], "Idle");
    assert_eq!(event["summary"], json!(["moved “Idle”"]));
}

#[gpui::test]
fn timeline_move_resize_and_link_tasks(cx: &mut TestAppContext) {
    let (host, cx) = canvas(
        cx,
        "timeline",
        json!({"tasks": [
            {"id": "design", "label": "Design", "start": "2026-03-02", "end": "2026-03-06"},
            {"id": "build", "label": "Build", "start": "2026-03-09", "duration": 5, "dependsOn": ["design"]},
            {"id": "docs", "label": "Docs", "start": "2026-03-09", "end": "2026-03-11"}
        ]}),
    );
    // A day's width on screen, from Design's five days.
    let day = bounds(cx, "genui-tl-bar-design").size.width / 5.;
    let build = bounds(cx, "genui-tl-bar-build").center();
    drag(cx, build, point(build.x + day * 2., build.y));
    let end = bounds(cx, "genui-tl-end-docs").center();
    drag(cx, end, point(end.x + day, end.y));
    let link = bounds(cx, "genui-tl-link-design").center();
    let docs = bounds(cx, "genui-tl-bar-docs").center();
    drag(cx, link, docs);
    assert_eq!(pending(&host, cx), 3);

    click(cx, "genui-panel-send");
    let event = last_event(&host, cx);
    assert_eq!(
        ops(&event),
        vec!["set_dates", "set_dates", "add_dependency"]
    );
    assert_eq!(event["changes"][0]["start"], "2026-03-11");
    assert_eq!(event["changes"][1]["end"], "2026-03-12");
    assert_eq!(
        event["summary"],
        json!([
            "moved “Build” 2 days later (Mar 11 – Mar 15)",
            "made “Docs” 4 days (Mar 9 – Mar 12)",
            "made “Docs” wait for “Design”"
        ])
    );
}
