//! Presentation settings for Configuration → Appearance. The previews are
//! native, theme-aware elements derived from each saved preset, not images.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::layout::section;
use super::{SettingsEvent, SettingsView};
use crate::features::workspaces::{
    LayoutPreset, LayoutProfile, LayoutProfiles, LayoutTool, TabLayout, ToolPlacement,
};
use crate::shared::ui;

#[derive(Clone, Copy)]
struct LayoutDrag(LayoutTool);

struct DragLabel(&'static str);

impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(ui::raised())
            .text_sm()
            .child(self.0)
    }
}

fn placement_icon(placement: ToolPlacement) -> IconName {
    match placement {
        ToolPlacement::Title => IconName::List,
        ToolPlacement::Left => IconName::PanelLeft,
        ToolPlacement::Right => IconName::PanelRight,
        ToolPlacement::Hidden => IconName::Command,
    }
}

fn preview_line(width: f32) -> Div {
    div()
        .w(relative(width))
        .h(rems(3. / 16.))
        .rounded_full()
        .bg(ui::muted().opacity(0.35))
}

fn preview_icon(icon: IconName) -> Icon {
    Icon::new(icon).xsmall().text_color(ui::muted())
}

/// Render the shortcuts in the order they will actually appear. Bound the
/// thumbnails so even a profile with every shortcut in one rail fits.
fn preview_shortcuts(profile: &LayoutProfile, placement: ToolPlacement, limit: usize) -> Vec<AnyElement> {
    let icons: Vec<_> = profile
        .shortcuts
        .iter()
        .filter(|shortcut| shortcut.placement == placement)
        .map(|shortcut| shortcut.tool.icon())
        .collect();
    let mut result: Vec<_> = icons
        .iter()
        .take(limit)
        .map(|icon| preview_icon(*icon).into_any_element())
        .collect();
    if icons.len() > limit {
        result.push(
            div()
                .text_size(rems(8. / 16.))
                .text_color(ui::faint())
                .child(format!("+{}", icons.len() - limit))
                .into_any_element(),
        );
    }
    result
}

fn preview_rail(profile: &LayoutProfile, placement: ToolPlacement) -> Div {
    v_flex()
        .w(rems(21. / 16.))
        .h_full()
        .flex_none()
        .items_center()
        .gap_1()
        .py_1()
        .rounded_md()
        .bg(ui::raised())
        .children(preview_shortcuts(profile, placement, 5))
}

/// An intentionally schematic miniature of the workspace. It follows the
/// same visibility rules as the window and uses only active theme tokens.
fn layout_preview(profile: &LayoutProfile) -> Div {
    let horizontal = profile.tabs_visible && profile.tab_layout == TabLayout::Horizontal;
    let navigation = profile.workspace_visible
        || (profile.tabs_visible && profile.tab_layout == TabLayout::Vertical);
    let has_left = profile.has(ToolPlacement::Left);
    let has_right = profile.has(ToolPlacement::Right);

    v_flex()
        .w_full()
        .h(rems(152. / 16.))
        .min_w_0()
        .rounded_lg()
        .border_1()
        .border_color(ui::border())
        .bg(ui::well())
        .overflow_hidden()
        .child(
            h_flex()
                .h(rems(23. / 16.))
                .flex_none()
                .px_1()
                .gap_1()
                .border_b_1()
                .border_color(ui::border())
                .bg(ui::surface())
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .overflow_hidden()
                        .when(horizontal, |d| {
                            d.child(
                                div()
                                    .w(rems(28. / 16.))
                                    .h(rems(12. / 16.))
                                    .rounded_sm()
                                    .bg(ui::accent().opacity(0.24))
                                    .border_1()
                                    .border_color(ui::accent().opacity(0.45)),
                            )
                            .child(div().w(rems(20. / 16.)).h(rems(12. / 16.)).rounded_sm().bg(ui::raised()))
                            .child(preview_icon(IconName::Plus))
                        })
                        .when(!horizontal && !navigation, |d| {
                            d.child(div().text_size(rems(9. / 16.)).text_color(ui::muted()).child("Gentle Shell"))
                        }),
                )
                .child(
                    h_flex()
                        .min_w_0()
                        .gap_0p5()
                        .overflow_hidden()
                        .when(profile.title_utilities, |d| d.child(preview_icon(IconName::Layers)))
                        .children(preview_shortcuts(profile, ToolPlacement::Title, 4))
                        .when(profile.original_controls, |d| d.child(preview_icon(IconName::PanelLeft)))
                        .child(preview_icon(IconName::Command))
                        .when(profile.original_controls, |d| {
                            d.child(preview_icon(IconName::Settings))
                                .child(preview_icon(IconName::PanelRight))
                        }),
                ),
        )
        .child(
            h_flex()
                .flex_1()
                .min_h_0()
                .items_stretch()
                .gap_1()
                .p_1()
                .when(has_left, |d| d.child(preview_rail(profile, ToolPlacement::Left)))
                .when(navigation, |d| {
                    d.child(
                        v_flex()
                            .flex_none()
                            .w(if profile.workspace_visible {
                                relative(0.24)
                            } else {
                                relative(0.10)
                            })
                            .min_w_0()
                            .gap_1()
                            .p_1()
                            .rounded_md()
                            .bg(ui::surface())
                            .when(profile.workspace_visible, |d| {
                                d.child(div().text_size(rems(8. / 16.)).text_color(ui::faint()).truncate().child("Workspaces"))
                            })
                            .children((0..4).map(|index| {
                                h_flex()
                                    .h(rems(13. / 16.))
                                    .gap_1()
                                    .rounded_sm()
                                    .when(index == 0, |d| d.bg(ui::accent().opacity(0.16)))
                                    .child(preview_icon(IconName::Bot))
                                    .when(profile.workspace_visible, |d| d.child(preview_line(0.50)))
                            })),
                    )
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .p_1()
                        .rounded_md()
                        .bg(ui::panel())
                        .child(h_flex().gap_1().child(preview_icon(IconName::Bot)).child(preview_line(0.40)))
                        .child(
                            v_flex()
                                .gap_1()
                                .p_1()
                                .rounded_md()
                                .bg(ui::surface())
                                .child(preview_line(0.86))
                                .child(preview_line(0.64))
                                .child(preview_line(0.76)),
                        )
                        .child(preview_line(0.72))
                        .child(div().flex_1())
                        .child(
                            h_flex()
                                .h(rems(23. / 16.))
                                .gap_1()
                                .px_1()
                                .rounded_md()
                                .border_1()
                                .border_color(ui::border())
                                .bg(ui::surface())
                                .child(div().flex_1().min_w_0().child(preview_line(0.64)))
                                .child(div().size(rems(9. / 16.)).rounded_full().bg(ui::accent())),
                        ),
                )
                .when(profile.sidebar_visible, |d| {
                    d.child(
                        v_flex()
                            .w(relative(0.28))
                            .flex_none()
                            .min_w_0()
                            .p_1()
                            .gap_1()
                            .rounded_md()
                            .bg(ui::surface())
                            .child(
                                h_flex()
                                    .gap_0p5()
                                    .child(preview_icon(profile.dock.icon()))
                                    .child(div().min_w_0().truncate().text_size(rems(8. / 16.)).text_color(ui::muted()).child(profile.dock.label())),
                            )
                            .when(profile.original_controls && matches!(profile.dock, LayoutTool::Files | LayoutTool::Git), |d| {
                                d.child(h_flex().gap_0p5().child(preview_icon(IconName::Files)).child(preview_icon(IconName::Diff)).child(preview_icon(IconName::GitBranch)))
                            })
                            .children((0..4).map(|index| {
                                h_flex()
                                    .gap_0p5()
                                    .child(preview_icon(if index == 0 { IconName::Folder } else { IconName::Files }))
                                    .child(preview_line(0.45))
                            })),
                    )
                })
                .when(has_right, |d| d.child(preview_rail(profile, ToolPlacement::Right))),
        )
}

fn preset_card(
    preset: LayoutPreset,
    profile: &LayoutProfile,
    selected: bool,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    let description = match preset {
        LayoutPreset::Minimal => "Original workspace",
        LayoutPreset::Expanded => "Toolbars and navigation",
    };
    div()
        .id(("layout-preset-card", preset as usize))
        .flex()
        .flex_col()
        .min_w_0()
        .gap_1p5()
        .p_1p5()
        .rounded_xl()
        .border_2()
        .border_color(if selected { ui::accent() } else { ui::border() })
        .when(selected, |d| d.bg(ui::accent().opacity(0.08)))
        .cursor_pointer()
        .hover(|d| d.bg(ui::surface_hover()))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.edit_presentation(|p| p.active = preset, cx);
        }))
        .child(layout_preview(profile))
        .child(
            h_flex()
                .px_1()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ui::fg())
                        .child(preset.label()),
                )
                .when(preset == LayoutPreset::Minimal, |d| {
                    d.child(ui::chip("Default", ui::muted()))
                })
                .when(selected, |d| {
                    d.child(Icon::new(IconName::Check).xsmall().text_color(ui::accent()))
                }),
        )
        .child(div().px_1().text_xs().text_color(ui::muted()).child(description))
}

fn visibility_row(
    id: &'static str,
    label: &'static str,
    hint: &'static str,
    visible: bool,
    set: impl Fn(&mut LayoutProfile, bool) + 'static,
    cx: &mut Context<SettingsView>,
) -> Div {
    h_flex()
        .gap_2()
        .py_2()
        .border_b_1()
        .border_color(ui::border().opacity(0.5))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().text_color(ui::fg()).child(label))
                .child(div().text_xs().text_color(ui::muted()).child(hint)),
        )
        .child(ui::segmented(
            id,
            &["Hide", "Show"],
            Some(usize::from(visible)),
            cx.listener(move |this, index: &usize, _, cx| {
                this.edit_presentation(|p| set(p.current_mut(), *index == 1), cx);
            }),
        ))
}

impl SettingsView {
    pub(crate) fn sync_presentation(&mut self, profiles: LayoutProfiles, cx: &mut Context<Self>) {
        self.layout_profiles = profiles;
        cx.notify();
    }

    fn edit_presentation(
        &mut self,
        edit: impl FnOnce(&mut LayoutProfiles),
        cx: &mut Context<Self>,
    ) {
        edit(&mut self.layout_profiles);
        cx.emit(SettingsEvent::PresentationChanged(self.layout_profiles.clone()));
        cx.notify();
    }

    pub(super) fn interface_section(&self, cx: &mut Context<Self>) -> Div {
        let profile = self.layout_profiles.current();
        let active = self.layout_profiles.active;
        let tab_index = if !profile.tabs_visible {
            0
        } else if profile.tab_layout == TabLayout::Horizontal {
            1
        } else {
            2
        };

        let mut settings = section(IconName::PanelLeft, "Workspace layout")
            .gap_2()
            .child(div().text_xs().text_color(ui::muted()).child(
                "Choose a preset. The previews update as you customize it, and each preset remembers its own layout.",
            ))
            .child(
                div().grid().grid_cols(2).gap_2().children(
                    LayoutPreset::ALL.into_iter().map(|preset| {
                        preset_card(preset, self.layout_profiles.profile(preset), active == preset, cx)
                    }),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(ui::muted())
                            .child(format!("Editing {} · changes are saved automatically", active.label())),
                    )
                    .child(
                        Button::new("layout-reset-preset")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::RefreshCw))
                            .label("Reset preset")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_presentation(LayoutProfiles::reset, cx);
                            })),
                    ),
            )
            .child(div().pt_2().child(ui::section_label("Window layout")))
            .child(
                h_flex()
                    .gap_2()
                    .py_2()
                    .border_b_1()
                    .border_color(ui::border().opacity(0.5))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().child("Tabs"))
                            .child(div().text_xs().text_color(ui::muted()).child("Choose where open tabs appear.")),
                    )
                    .child(ui::segmented(
                        "layout-tabs",
                        &["Hidden", "Horizontal", "Vertical"],
                        Some(tab_index),
                        cx.listener(|this, index: &usize, _, cx| {
                            this.edit_presentation(|p| {
                                let p = p.current_mut();
                                p.tabs_visible = *index != 0;
                                if *index != 0 {
                                    p.tab_layout = if *index == 1 { TabLayout::Horizontal } else { TabLayout::Vertical };
                                }
                            }, cx);
                        }),
                    )),
            )
            .child(visibility_row(
                "layout-workspace",
                "Workspace navigation",
                "Show the expanded list of workspaces and agents.",
                profile.workspace_visible,
                |p, visible| p.workspace_visible = visible,
                cx,
            ))
            .child(visibility_row(
                "layout-right-panel",
                "Right panel",
                "Show the active Files, Git, History or Engram view.",
                profile.sidebar_visible,
                |p, visible| p.sidebar_visible = visible,
                cx,
            ))
            .child(visibility_row(
                "layout-title-utilities",
                "Title bar utilities",
                "Pane layout, music, tray and notifications.",
                profile.title_utilities,
                |p, visible| p.title_utilities = visible,
                cx,
            ))
            .child(visibility_row(
                "layout-original-controls",
                "Original controls",
                "Configuration menu and original panel switches.",
                profile.original_controls,
                |p, visible| p.original_controls = visible,
                cx,
            ))
            .child(div().pt_2().child(ui::section_label("Tool placement")))
            .child(div().text_xs().text_color(ui::muted()).child(
                "Choose a location for each shortcut or drag it onto a location below. Drag rows to reorder. Hidden tools remain available in Commands.",
            ));

        // The four destinations are also drop targets. This makes it possible
        // to move a tool into an entirely empty rail without leaving settings.
        settings = settings.child(
            div().grid().grid_cols(4).gap_1().children(
                ToolPlacement::ALL.into_iter().map(|placement| {
                    let count = profile.shortcuts.iter().filter(|s| s.placement == placement).count();
                    v_flex()
                        .id(("layout-destination", placement as usize))
                        .min_w_0()
                        .items_center()
                        .gap_1()
                        .p_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(ui::border())
                        .bg(ui::well())
                        .drag_over::<LayoutDrag>(|style, _, _, _| style.bg(ui::accent().opacity(0.16)))
                        .on_drop(cx.listener(move |this, drag: &LayoutDrag, _, cx| {
                            this.edit_presentation(|p| p.current_mut().place(drag.0, placement), cx);
                        }))
                        .child(Icon::new(placement_icon(placement)).small().text_color(ui::accent()))
                        .child(div().w_full().truncate().text_center().text_xs().child(placement.label()))
                        .child(div().text_xs().text_color(ui::muted()).child(format!("{count}")))
                }),
            ),
        );

        let mut rows = v_flex().gap_0p5();
        for shortcut in &profile.shortcuts {
            let tool = shortcut.tool;
            let selected = shortcut.placement;
            rows = rows.child(
                h_flex()
                    .id(("layout-tool-row", tool as usize))
                    .min_w_0()
                    .gap_2()
                    .py_1p5()
                    .px_1()
                    .rounded_md()
                    .border_b_1()
                    .border_color(ui::border().opacity(0.5))
                    .on_drag(LayoutDrag(tool), |drag, _, _, cx| {
                        cx.new(|_| DragLabel(drag.0.label()))
                    })
                    .drag_over::<LayoutDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
                    .on_drop(cx.listener(move |this, drag: &LayoutDrag, _, cx| {
                        this.edit_presentation(|p| p.current_mut().move_before(drag.0, tool), cx);
                    }))
                    .child(Icon::new(IconName::GripHorizontal).xsmall().text_color(ui::faint()))
                    .child(Icon::new(tool.icon()).small().text_color(ui::muted()))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().text_sm().truncate().child(tool.label()))
                            .child(div().text_xs().text_color(ui::muted()).truncate().child(selected.label())),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_0p5()
                            .p_1()
                            .rounded_lg()
                            .bg(ui::well())
                            .children(ToolPlacement::ALL.into_iter().map(|placement| {
                                Button::new(("layout-placement", (tool as usize) * 4 + placement as usize))
                                    .ghost()
                                    .xsmall()
                                    .selected(selected == placement)
                                    .icon(Icon::new(placement_icon(placement)))
                                    .tooltip(placement.label())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit_presentation(|p| p.current_mut().place(tool, placement), cx);
                                    }))
                            })),
                    ),
            );
        }
        settings.child(rows)
    }
}
