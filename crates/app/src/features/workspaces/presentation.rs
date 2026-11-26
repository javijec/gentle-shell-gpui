//! Independent, serializable presentation presets. They never own panes,
//! sessions, or workspace data.
use gpui_kit::assets::IconName;
use serde::{Deserialize, Serialize};

use super::{SavedState, TabLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutTool {
    Files, Git, History, Engram, Agents, Docs, Devices, Usage, Tower,
    Profiles, Sessions, Providers, Configuration,
}

impl LayoutTool {
    pub const ALL: [Self; 13] = [
        Self::Files, Self::Git, Self::History, Self::Engram, Self::Agents,
        Self::Docs, Self::Devices, Self::Usage, Self::Tower, Self::Profiles,
        Self::Sessions, Self::Providers, Self::Configuration,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "Files", Self::Git => "Git",
            Self::History => "Git History", Self::Engram => "Engram",
            Self::Agents => "Agents", Self::Docs => "Architecture Docs",
            Self::Devices => "Devices", Self::Usage => "Usage & Stats",
            Self::Tower => "Gentle Tower", Self::Profiles => "Profiles",
            Self::Sessions => "Sessions", Self::Providers => "Providers",
            Self::Configuration => "Configuration",
        }
    }
    pub fn icon(self) -> IconName {
        match self {
            Self::Files => IconName::Files, Self::Git => IconName::GitBranch,
            Self::History => IconName::GitGraph, Self::Engram => IconName::Brain,
            Self::Agents => IconName::Users, Self::Docs => IconName::BookOpen,
            Self::Devices => IconName::Smartphone, Self::Usage => IconName::Gauge,
            Self::Tower => IconName::Building2, Self::Profiles => IconName::Route,
            Self::Sessions => IconName::RotateCcwClock, Self::Providers => IconName::KeyRound,
            Self::Configuration => IconName::Settings,
        }
    }
    pub fn docked(self) -> bool {
        matches!(self, Self::Files | Self::Git | Self::History | Self::Engram)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPlacement { Title, Left, Right, Hidden }

impl ToolPlacement {
    pub const ALL: [Self; 4] = [Self::Title, Self::Left, Self::Right, Self::Hidden];
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title bar", Self::Left => "Left rail",
            Self::Right => "Right rail", Self::Hidden => "Commands only",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutPreset { Minimal, Expanded }

impl LayoutPreset {
    pub const ALL: [Self; 2] = [Self::Minimal, Self::Expanded];
    pub fn label(self) -> &'static str {
        match self { Self::Minimal => "Minimal", Self::Expanded => "Expanded" }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolShortcut {
    pub tool: LayoutTool,
    pub placement: ToolPlacement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfile {
    pub shortcuts: Vec<ToolShortcut>,
    pub tabs_visible: bool,
    pub tab_layout: TabLayout,
    pub workspace_visible: bool,
    pub sidebar_visible: bool,
    pub dock: LayoutTool,
    pub tabs_width: f32,
    pub files_width: f32,
}

impl LayoutProfile {
    pub fn preset(preset: LayoutPreset) -> Self {
        let expanded = preset == LayoutPreset::Expanded;
        let width = gentle_core::workspaces::SIDEBAR_DEFAULT_WIDTH;
        Self {
            shortcuts: LayoutTool::ALL.into_iter().map(|tool| ToolShortcut {
                tool,
                placement: if expanded { ToolPlacement::Right }
                    else if matches!(tool, LayoutTool::Files | LayoutTool::Git | LayoutTool::History | LayoutTool::Configuration) {
                        ToolPlacement::Title
                    } else { ToolPlacement::Hidden },
            }).collect(),
            tabs_visible: expanded,
            tab_layout: if expanded { TabLayout::Vertical } else { TabLayout::Horizontal },
            workspace_visible: expanded,
            sidebar_visible: expanded,
            dock: LayoutTool::Files,
            tabs_width: width,
            files_width: width,
        }
    }
    pub fn has(&self, placement: ToolPlacement) -> bool {
        self.shortcuts.iter().any(|s| s.placement == placement)
    }
    pub fn place(&mut self, tool: LayoutTool, placement: ToolPlacement) {
        if let Some(s) = self.shortcuts.iter_mut().find(|s| s.tool == tool) {
            s.placement = placement;
        }
    }
    /// Reorder without introducing duplicate shortcuts.
    pub fn move_before(&mut self, tool: LayoutTool, target: LayoutTool) {
        if tool == target { return; }
        let Some(from) = self.shortcuts.iter().position(|s| s.tool == tool) else { return; };
        if !self.shortcuts.iter().any(|s| s.tool == target) { return; }
        let item = self.shortcuts.remove(from);
        if let Some(to) = self.shortcuts.iter().position(|s| s.tool == target) {
            self.shortcuts.insert(to, item);
        }
    }
    pub fn normalize(&mut self, preset: LayoutPreset) {
        let mut seen = Vec::new();
        self.shortcuts.retain(|s| {
            if seen.contains(&s.tool) { false } else { seen.push(s.tool); true }
        });
        for s in Self::preset(preset).shortcuts {
            if !seen.contains(&s.tool) { self.shortcuts.push(s); }
        }
        if !self.dock.docked() { self.dock = LayoutTool::Files; }
        self.tabs_width = gentle_core::workspaces::saved_sidebar_width(self.tabs_width);
        self.files_width = gentle_core::workspaces::saved_sidebar_width(self.files_width);
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfiles {
    pub active: LayoutPreset,
    pub minimal: LayoutProfile,
    pub expanded: LayoutProfile,
}

impl Default for LayoutProfiles {
    fn default() -> Self {
        Self {
            active: LayoutPreset::Minimal,
            minimal: LayoutProfile::preset(LayoutPreset::Minimal),
            expanded: LayoutProfile::preset(LayoutPreset::Expanded),
        }
    }
}

impl LayoutProfiles {
    pub fn current(&self) -> &LayoutProfile {
        match self.active { LayoutPreset::Minimal => &self.minimal, LayoutPreset::Expanded => &self.expanded }
    }
    pub fn current_mut(&mut self) -> &mut LayoutProfile {
        match self.active { LayoutPreset::Minimal => &mut self.minimal, LayoutPreset::Expanded => &mut self.expanded }
    }
    pub fn reset(&mut self) {
        *self.current_mut() = LayoutProfile::preset(self.active);
    }
    pub fn normalize(&mut self) {
        self.minimal.normalize(LayoutPreset::Minimal);
        self.expanded.normalize(LayoutPreset::Expanded);
    }
    /// Existing installations retain their tab placement, widths and panel
    /// visibility. Only a new installation starts with the Minimal preset.
    pub fn from_saved(saved: &SavedState) -> Self {
        if let Some(mut profiles) = saved.presentation.clone() {
            profiles.normalize();
            return profiles;
        }
        let mut profiles = Self::default();
        if saved.version != 0 || !saved.workspaces.is_empty() {
            profiles.active = LayoutPreset::Expanded;
            let p = profiles.current_mut();
            p.tab_layout = saved.tab_layout;
            p.workspace_visible = saved.tab_layout == TabLayout::Vertical && !saved.sidebar_compact;
            p.sidebar_visible = saved.sidebar;
            p.tabs_width = saved.tabs_sidebar_width;
            p.files_width = saved.files_sidebar_width;
        }
        profiles.normalize();
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_has_only_essential_title_icons_and_no_panels() {
        let p = LayoutProfiles::default();
        let current = p.current();
        assert!(!current.tabs_visible && !current.workspace_visible && !current.sidebar_visible);
        let title: Vec<_> = current.shortcuts.iter().filter(|s| s.placement == ToolPlacement::Title).map(|s| s.tool).collect();
        assert_eq!(title, [LayoutTool::Files, LayoutTool::Git, LayoutTool::History, LayoutTool::Configuration]);
    }

    #[test]
    fn presets_keep_independent_changes_and_round_trip() {
        let mut p = LayoutProfiles::default();
        p.current_mut().place(LayoutTool::Engram, ToolPlacement::Left);
        p.current_mut().move_before(LayoutTool::Engram, LayoutTool::Files);
        p.active = LayoutPreset::Expanded;
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Files);
        p.current_mut().sidebar_visible = false;
        p.active = LayoutPreset::Minimal;
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Engram);
        assert_eq!(serde_json::from_str::<LayoutProfiles>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
        p.reset();
        assert_eq!(p.current().shortcuts[0].tool, LayoutTool::Files);
        assert!(!p.expanded.sidebar_visible);
    }

    #[test]
    fn normalization_removes_duplicates_and_restores_missing_tools() {
        let mut p = LayoutProfile::preset(LayoutPreset::Minimal);
        p.shortcuts.push(p.shortcuts[0]);
        p.shortcuts.remove(1);
        p.dock = LayoutTool::Devices;
        p.normalize(LayoutPreset::Minimal);
        assert_eq!(p.shortcuts.len(), LayoutTool::ALL.len());
        assert_eq!(p.dock, LayoutTool::Files);
    }

    #[test]
    fn legacy_layout_preserves_visibility_and_width() {
        let saved = SavedState {
            version: 1, tab_layout: TabLayout::Vertical, sidebar_compact: true,
            sidebar: false, files_sidebar_width: 420., ..SavedState::default()
        };
        let p = LayoutProfiles::from_saved(&saved);
        assert_eq!(p.active, LayoutPreset::Expanded);
        assert_eq!(p.current().tab_layout, TabLayout::Vertical);
        assert!(!p.current().workspace_visible && !p.current().sidebar_visible);
        assert_eq!(p.current().files_width, 420.);
    }
}
