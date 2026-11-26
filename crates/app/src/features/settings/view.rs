//! Layout and render logic for the Configuration dialog.
//!
//! Keep the viewport-constrained flex hierarchy and the scrolling body together
//! so future changes to the header or navigation do not hide lower settings.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::{SettingsEvent, SettingsView, navigation};
use crate::shared::ui;

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep settings usable when there is not enough horizontal space
        // for both a category rail and the existing settings controls.
        let compact = window.viewport_size().width < rems(820. / 16.).to_pixels(window.rem_size());
        let page = self.page;
        let body: AnyElement = match &self.snapshot {
            Some(snapshot) => self.render_page(page, snapshot, cx),
            None => h_flex()
                .p_6()
                .gap_2()
                .justify_center()
                .text_sm()
                .text_color(ui::muted())
                .child(Spinner::new().small().color(ui::accent()))
                .child("Reading your Gentle Shell configuration…")
                .into_any_element(),
        };
        let (backdrop, panel) = ui::workspace_screen(Some(960.));
        backdrop.occlude().child(ui::pop_in(
            "settings-screen",
            panel
                .w_full()
                .max_w(rems(960. / 16.))
                // A definite, viewport-clamped height is required for nested
                // flex children to shrink and for settings-body to scroll.
                // ui::screen already applies max_h_full() to the panel.
                .h(rems(720. / 16.))
                .child(
                    ui::screen_header()
                        .flex_shrink_0()
                        .child(
                            Icon::new(IconName::Settings)
                                .small()
                                .text_color(ui::muted()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_base()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ui::fg())
                                .child("Configuration"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child("Manage Gentle Shell and the desktop experience"),
                        )
                        .child(
                            Button::new("settings-refresh")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::RefreshCw))
                                .tooltip("Reload and check for updates")
                                .on_click(cx.listener(|this, _, _, cx| this.reload_with(true, cx))),
                        )
                        .child(
                            Button::new("settings-close")
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(IconName::X))
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                        ),
                )
                .child(
                    h_flex()
                        // h_flex defaults to items_center. That lets the
                        // settings column grow taller than this row and clips
                        // its scrollable body instead of constraining it.
                        .items_stretch()
                        .flex_1()
                        .min_h_0()
                        .when(!compact, |container| {
                            container.child(navigation::sidebar(page, &self.settings_search, cx))
                        })
                        .child(
                            v_flex()
                                // Fill the row's bounded height, not the
                                // intrinsic height of every settings card.
                                .h_full()
                                .flex_1()
                                .min_w_0()
                                .min_h_0()
                                .when(compact, |content| {
                                    content.child(navigation::compact(
                                        page,
                                        &self.settings_search,
                                        self.compact_categories_open,
                                        cx,
                                    ))
                                })
                                .child(
                                    v_flex()
                                        .flex_shrink_0()
                                        .px_4()
                                        .py_3()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(ui::fg())
                                                .child(page.label()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(ui::muted())
                                                .child(page.description()),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("settings-body")
                                        .flex_1()
                                        .min_h_0()
                                        .overflow_y_scroll()
                                        .track_scroll(&self.body_scroll)
                                        .child(body),
                                ),
                        ),
                ),
        ))
    }
}
