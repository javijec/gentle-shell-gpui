//! Presentation settings for Configuration → Appearance. All changes are
//! applied to the existing workspace, never to a second set of views.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use super::layout::section;
use super::{SettingsEvent, SettingsView};
use crate::features::workspaces::{LayoutPreset, LayoutProfiles, LayoutTool, TabLayout, ToolPlacement};
use crate::shared::ui;

#[derive(Clone, Copy)]
struct LayoutDrag(LayoutTool);

struct DragLabel(&'static str);
impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px_2().py_1().rounded_md().bg(ui::raised()).text_sm().child(self.0)
    }
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
        let mut settings = section(IconName::PanelLeft, "Workspace layout")
            .child(
                div().text_xs().text_color(ui::muted()).child(
                    "Choose a starting layout. Each preset remembers its own customization. Hidden shortcuts remain in Commands.",
                ),
            )
            .child(h_flex().gap_2().pt_2().children(
                LayoutPreset::ALL.into_iter().map(|preset| {
                    Button::new(("layout-preset", preset as usize))
                        .ghost().small()
                        .selected(self.layout_profiles.active == preset)
                        .label(preset.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_presentation(|p| p.active = preset, cx);
                        }))
                }),
            ))
            .child(h_flex().gap_2().pt_2()
                .child(div().w(rems(100. / 16.)).flex_none().text_sm().child("Tabs"))
                .child(Button::new("layout-tabs-hidden").ghost().small()
                    .selected(!profile.tabs_visible).label("Hidden")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_presentation(|p| p.current_mut().tabs_visible = false, cx);
                    })))
                .children([TabLayout::Horizontal, TabLayout::Vertical].into_iter().map(|layout| {
                    Button::new(("layout-tabs", layout as usize))
                        .ghost().small()
                        .selected(profile.tabs_visible && profile.tab_layout == layout)
                        .label(if layout == TabLayout::Horizontal { "Horizontal" } else { "Vertical" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.edit_presentation(|p| {
                                let p = p.current_mut();
                                p.tabs_visible = true;
                                p.tab_layout = layout;
                            }, cx);
                        }))
                })))
            .child(h_flex().gap_2().pt_2()
                .child(div().flex_1().text_sm().child("Workspace navigation"))
                .child(Button::new("layout-workspace").ghost().small()
                    .selected(profile.workspace_visible)
                    .label(if profile.workspace_visible { "Visible" } else { "Hidden" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_presentation(|p| {
                            let p = p.current_mut();
                            p.workspace_visible = !p.workspace_visible;
                        }, cx);
                    }))))
            .child(h_flex().gap_2().pt_2()
                .child(div().flex_1().text_sm().child("Original title utilities"))
                .child(Button::new("layout-title-utilities").ghost().small()
                    .selected(profile.title_utilities)
                    .label(if profile.title_utilities { "Visible" } else { "Hidden" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_presentation(|p| {
                            let p = p.current_mut();
                            p.title_utilities = !p.title_utilities;
                        }, cx);
                    }))))
            .child(h_flex().gap_2().pt_2()
                .child(div().flex_1().text_sm().child("Original screen menu and panel controls"))
                .child(Button::new("layout-original-controls").ghost().small()
                    .selected(profile.original_controls)
                    .label(if profile.original_controls { "Visible" } else { "Hidden" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_presentation(|p| {
                            let p = p.current_mut();
                            p.original_controls = !p.original_controls;
                        }, cx);
                    }))))
            .child(h_flex().gap_2().pt_2()
                .child(div().flex_1().text_sm().child("Right panel"))
                .child(Button::new("layout-right-panel").ghost().small()
                    .selected(profile.sidebar_visible)
                    .label(if profile.sidebar_visible { "Visible" } else { "Hidden" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.edit_presentation(|p| {
                            let p = p.current_mut();
                            p.sidebar_visible = !p.sidebar_visible;
                        }, cx);
                    }))))
            .child(div().pt_3().child(ui::section_label("Tool placement")))
            .child(div().text_xs().text_color(ui::muted()).child(
                "Drag a row to reorder tools. Use the placement buttons or right-click a shortcut in the workspace to move it.",
            ));

        let mut rows = v_flex().gap_1();
        for shortcut in &profile.shortcuts {
            let tool = shortcut.tool;
            let selected = shortcut.placement;
            rows = rows.child(
                v_flex()
                    .id(("layout-tool-row", tool as usize))
                    .rounded_md().p_2().gap_1()
                    .border_b_1().border_color(ui::border())
                    .on_drag(LayoutDrag(tool), |drag, _, _, cx| cx.new(|_| DragLabel(drag.0.label())))
                    .drag_over::<LayoutDrag>(|style, _, _, _| style.bg(ui::petal().opacity(0.18)))
                    .on_drop(cx.listener(move |this, drag: &LayoutDrag, _, cx| {
                        this.edit_presentation(|p| p.current_mut().move_before(drag.0, tool), cx);
                    }))
                    .child(h_flex().gap_2()
                        .child(Icon::new(IconName::GripHorizontal).xsmall().text_color(ui::muted()))
                        .child(Icon::new(tool.icon()).small())
                        .child(div().flex_1().text_sm().child(tool.label())))
                    .child(h_flex().gap_1().children(ToolPlacement::ALL.into_iter().map(|placement| {
                        Button::new(("layout-placement", (tool as usize) * 4 + placement as usize))
                            .ghost().xsmall()
                            .selected(selected == placement)
                            .label(placement.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_presentation(|p| p.current_mut().place(tool, placement), cx);
                            }))
                    }))),
            );
        }
        settings = settings.child(rows).child(
            Button::new("layout-reset-preset").ghost().small().label("Reset current preset")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edit_presentation(LayoutProfiles::reset, cx);
                })),
        );
        settings
    }
}
