//! Unified cross-platform accessibility element model.
//!
//! Defines `UnifiedNode`, `UnifiedRole`, `UnifiedState`, and related types that
//! normalize platform-specific accessibility APIs (Windows UIA, Linux AT-SPI,
//! macOS AXAccessibility, Chrome DevTools Protocol) into a single tree schema.
//!
//! The model is wire-compatible with the Python `AccessibilityNode` schema in
//! `qontinui-schemas`: overlapping fields serialize identically, and extra Rust
//! fields are ignored by the Python side.

use serde::{Deserialize, Serialize};

/// Stable reference ID for an element (e.g., `"@e1"`, `"@e42"`).
pub type Ref = String;

/// Platform-agnostic accessibility role.
///
/// Superset of WAI-ARIA 1.2 roles plus Windows UIA and macOS AX extensions.
/// Matches the Python `AccessibilityRole` enum values for wire compatibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnifiedRole {
    // Document structure
    Application,
    Document,
    Article,
    Banner,
    Complementary,
    Contentinfo,
    Form,
    Main,
    Navigation,
    Region,
    Search,

    // Widget roles
    Button,
    Checkbox,
    Combobox,
    Dialog,
    Gridcell,
    Link,
    Listbox,
    Menu,
    Menubar,
    Menuitem,
    Menuitemcheckbox,
    Menuitemradio,
    Option,
    Progressbar,
    Radio,
    Radiogroup,
    Scrollbar,
    Searchbox,
    Slider,
    Spinbutton,
    Switch,
    Tab,
    Tablist,
    Tabpanel,
    Textbox,
    Toolbar,
    Tooltip,
    Tree,
    Treegrid,
    Treeitem,

    // Structure roles
    Alert,
    Alertdialog,
    Grid,
    Heading,
    Img,
    List,
    Listitem,
    Log,
    Marquee,
    Math,
    Note,
    Separator,
    Status,
    Table,
    Cell,
    Columnheader,
    Row,
    Rowgroup,
    Rowheader,
    Timer,
    Definition,
    Directory,
    Figure,
    Group,
    Paragraph,
    Term,

    // Generic/fallback
    Generic,
    StaticText,
    None,
    #[default]
    Unknown,

    // Windows UIA specific
    Window,
    Pane,
    Titlebar,
    Edit,
    Custom,
    Dataitem,
    Datepicker,
    Calendar,
    Hyperlink,
    Splitbutton,
}

/// UI Automation control type, modelled after FlaUI's `ControlType` enum and
/// Microsoft's well-known UIA `UIA_CONTROLTYPE_ID` constants (50000-50039).
///
/// `ControlType` is a slightly narrower, FlaUI-compatible vocabulary than
/// [`UnifiedRole`]. It exists so that spec authors and tests cribbing from
/// FlaUI.Core.UITests can use the same names. All variants map bidirectionally
/// to `UnifiedRole` via [`ControlType::to_unified_role`] /
/// [`UnifiedRole::to_control_type`]; the mapping is total but lossy (several
/// `UnifiedRole` variants collapse to the same `ControlType`).
///
/// This type is platform-agnostic — it lives here (not behind a `cfg(windows)`
/// gate) so non-Windows backends can still accept control-type queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ControlType {
    Button,
    Calendar,
    CheckBox,
    ComboBox,
    Edit,
    Hyperlink,
    Image,
    List,
    ListItem,
    Menu,
    MenuBar,
    MenuItem,
    ProgressBar,
    RadioButton,
    ScrollBar,
    Slider,
    Spinner,
    StatusBar,
    Tab,
    TabItem,
    Text,
    ToolBar,
    ToolTip,
    Tree,
    TreeItem,
    Custom,
    Group,
    Thumb,
    DataGrid,
    DataItem,
    Document,
    SplitButton,
    Window,
    Pane,
    Header,
    HeaderItem,
    Table,
    TitleBar,
    Separator,
    SemanticZoom,
    AppBar,
}

impl ControlType {
    /// Map a FlaUI-style control type to the qontinui [`UnifiedRole`].
    ///
    /// The mapping is total: every `ControlType` resolves to a concrete
    /// `UnifiedRole`. Variants without a perfect match fall back to the
    /// closest semantic role (e.g. `Thumb` → `Slider`, `SemanticZoom` →
    /// `Group`, `AppBar` → `Toolbar`).
    pub const fn to_unified_role(self) -> UnifiedRole {
        match self {
            Self::Button => UnifiedRole::Button,
            Self::Calendar => UnifiedRole::Calendar,
            Self::CheckBox => UnifiedRole::Checkbox,
            Self::ComboBox => UnifiedRole::Combobox,
            Self::Edit => UnifiedRole::Edit,
            Self::Hyperlink => UnifiedRole::Hyperlink,
            Self::Image => UnifiedRole::Img,
            Self::List => UnifiedRole::List,
            Self::ListItem => UnifiedRole::Listitem,
            Self::Menu => UnifiedRole::Menu,
            Self::MenuBar => UnifiedRole::Menubar,
            Self::MenuItem => UnifiedRole::Menuitem,
            Self::ProgressBar => UnifiedRole::Progressbar,
            Self::RadioButton => UnifiedRole::Radio,
            Self::ScrollBar => UnifiedRole::Scrollbar,
            Self::Slider => UnifiedRole::Slider,
            Self::Spinner => UnifiedRole::Spinbutton,
            Self::StatusBar => UnifiedRole::Status,
            Self::Tab => UnifiedRole::Tab,
            Self::TabItem => UnifiedRole::Tabpanel,
            Self::Text => UnifiedRole::StaticText,
            Self::ToolBar => UnifiedRole::Toolbar,
            Self::ToolTip => UnifiedRole::Tooltip,
            Self::Tree => UnifiedRole::Tree,
            Self::TreeItem => UnifiedRole::Treeitem,
            Self::Custom => UnifiedRole::Custom,
            Self::Group => UnifiedRole::Group,
            // Thumb (drag handle on a scrollbar/slider) has no dedicated
            // UnifiedRole; Slider is the closest interactive match.
            Self::Thumb => UnifiedRole::Slider,
            Self::DataGrid => UnifiedRole::Grid,
            Self::DataItem => UnifiedRole::Dataitem,
            Self::Document => UnifiedRole::Document,
            Self::SplitButton => UnifiedRole::Splitbutton,
            Self::Window => UnifiedRole::Window,
            Self::Pane => UnifiedRole::Pane,
            Self::Header => UnifiedRole::Rowgroup,
            Self::HeaderItem => UnifiedRole::Columnheader,
            Self::Table => UnifiedRole::Table,
            Self::TitleBar => UnifiedRole::Titlebar,
            Self::Separator => UnifiedRole::Separator,
            // SemanticZoom is a Windows 8+ zoom control with no ARIA peer;
            // Group is the least-wrong fallback.
            Self::SemanticZoom => UnifiedRole::Group,
            // AppBar is the Windows 8+ edge-command surface; Toolbar is the
            // closest existing role.
            Self::AppBar => UnifiedRole::Toolbar,
        }
    }

    /// Construct a `ControlType` from a Microsoft UIA `UIA_CONTROLTYPE_ID`
    /// integer (50000-50039). Unknown IDs return `None`.
    ///
    /// Reference:
    /// <https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-controltype-ids>
    pub const fn from_uia_control_type_id(id: i32) -> Option<Self> {
        match id {
            50000 => Some(Self::Button),
            50001 => Some(Self::Calendar),
            50002 => Some(Self::CheckBox),
            50003 => Some(Self::ComboBox),
            50004 => Some(Self::Edit),
            50005 => Some(Self::Hyperlink),
            50006 => Some(Self::Image),
            50007 => Some(Self::ListItem),
            50008 => Some(Self::List),
            50009 => Some(Self::Menu),
            50010 => Some(Self::MenuBar),
            50011 => Some(Self::MenuItem),
            50012 => Some(Self::ProgressBar),
            50013 => Some(Self::RadioButton),
            50014 => Some(Self::ScrollBar),
            50015 => Some(Self::Slider),
            50016 => Some(Self::Spinner),
            50017 => Some(Self::StatusBar),
            50018 => Some(Self::Tab),
            50019 => Some(Self::TabItem),
            50020 => Some(Self::Text),
            50021 => Some(Self::ToolBar),
            50022 => Some(Self::ToolTip),
            50023 => Some(Self::Tree),
            50024 => Some(Self::TreeItem),
            50025 => Some(Self::Custom),
            50026 => Some(Self::Group),
            50027 => Some(Self::Thumb),
            50028 => Some(Self::DataGrid),
            50029 => Some(Self::DataItem),
            50030 => Some(Self::Document),
            50031 => Some(Self::SplitButton),
            50032 => Some(Self::Window),
            50033 => Some(Self::Pane),
            50034 => Some(Self::Header),
            50035 => Some(Self::HeaderItem),
            50036 => Some(Self::Table),
            50037 => Some(Self::TitleBar),
            50038 => Some(Self::Separator),
            50039 => Some(Self::SemanticZoom),
            50040 => Some(Self::AppBar),
            _ => None,
        }
    }

    /// The corresponding `UIA_CONTROLTYPE_ID` integer. Inverse of
    /// [`Self::from_uia_control_type_id`] for known IDs.
    pub const fn to_uia_control_type_id(self) -> i32 {
        match self {
            Self::Button => 50000,
            Self::Calendar => 50001,
            Self::CheckBox => 50002,
            Self::ComboBox => 50003,
            Self::Edit => 50004,
            Self::Hyperlink => 50005,
            Self::Image => 50006,
            Self::ListItem => 50007,
            Self::List => 50008,
            Self::Menu => 50009,
            Self::MenuBar => 50010,
            Self::MenuItem => 50011,
            Self::ProgressBar => 50012,
            Self::RadioButton => 50013,
            Self::ScrollBar => 50014,
            Self::Slider => 50015,
            Self::Spinner => 50016,
            Self::StatusBar => 50017,
            Self::Tab => 50018,
            Self::TabItem => 50019,
            Self::Text => 50020,
            Self::ToolBar => 50021,
            Self::ToolTip => 50022,
            Self::Tree => 50023,
            Self::TreeItem => 50024,
            Self::Custom => 50025,
            Self::Group => 50026,
            Self::Thumb => 50027,
            Self::DataGrid => 50028,
            Self::DataItem => 50029,
            Self::Document => 50030,
            Self::SplitButton => 50031,
            Self::Window => 50032,
            Self::Pane => 50033,
            Self::Header => 50034,
            Self::HeaderItem => 50035,
            Self::Table => 50036,
            Self::TitleBar => 50037,
            Self::Separator => 50038,
            Self::SemanticZoom => 50039,
            Self::AppBar => 50040,
        }
    }
}

impl From<ControlType> for UnifiedRole {
    fn from(ct: ControlType) -> Self {
        ct.to_unified_role()
    }
}

impl UnifiedRole {
    /// Map this role to the nearest FlaUI-compatible [`ControlType`].
    ///
    /// Returns `None` for web/ARIA-only roles that have no UIA peer
    /// (e.g. `Main`, `Navigation`, `Searchbox`), and for sentinel variants
    /// (`None`, `Unknown`, `Generic`).
    pub const fn to_control_type(self) -> Option<ControlType> {
        match self {
            Self::Button => Some(ControlType::Button),
            Self::Calendar => Some(ControlType::Calendar),
            Self::Checkbox => Some(ControlType::CheckBox),
            Self::Combobox => Some(ControlType::ComboBox),
            Self::Edit => Some(ControlType::Edit),
            // Textbox and Searchbox are ARIA flavours of Edit.
            Self::Textbox => Some(ControlType::Edit),
            Self::Searchbox => Some(ControlType::Edit),
            Self::Hyperlink => Some(ControlType::Hyperlink),
            Self::Link => Some(ControlType::Hyperlink),
            Self::Img => Some(ControlType::Image),
            Self::Figure => Some(ControlType::Image),
            Self::List => Some(ControlType::List),
            Self::Listbox => Some(ControlType::List),
            Self::Directory => Some(ControlType::List),
            Self::Listitem => Some(ControlType::ListItem),
            Self::Option => Some(ControlType::ListItem),
            Self::Menu => Some(ControlType::Menu),
            Self::Menubar => Some(ControlType::MenuBar),
            Self::Menuitem => Some(ControlType::MenuItem),
            Self::Menuitemcheckbox => Some(ControlType::MenuItem),
            Self::Menuitemradio => Some(ControlType::MenuItem),
            Self::Progressbar => Some(ControlType::ProgressBar),
            Self::Radio => Some(ControlType::RadioButton),
            Self::Radiogroup => Some(ControlType::Group),
            Self::Scrollbar => Some(ControlType::ScrollBar),
            Self::Slider => Some(ControlType::Slider),
            Self::Spinbutton => Some(ControlType::Spinner),
            Self::Status => Some(ControlType::StatusBar),
            Self::Tab => Some(ControlType::Tab),
            Self::Tablist => Some(ControlType::Tab),
            Self::Tabpanel => Some(ControlType::TabItem),
            Self::StaticText => Some(ControlType::Text),
            Self::Paragraph => Some(ControlType::Text),
            Self::Heading => Some(ControlType::Text),
            Self::Toolbar => Some(ControlType::ToolBar),
            Self::Tooltip => Some(ControlType::ToolTip),
            Self::Tree => Some(ControlType::Tree),
            Self::Treeitem => Some(ControlType::TreeItem),
            Self::Treegrid => Some(ControlType::DataGrid),
            Self::Custom => Some(ControlType::Custom),
            Self::Group => Some(ControlType::Group),
            Self::Region => Some(ControlType::Group),
            Self::Form => Some(ControlType::Group),
            Self::Grid => Some(ControlType::DataGrid),
            Self::Gridcell => Some(ControlType::DataItem),
            Self::Dataitem => Some(ControlType::DataItem),
            Self::Cell => Some(ControlType::DataItem),
            Self::Document => Some(ControlType::Document),
            Self::Article => Some(ControlType::Document),
            Self::Splitbutton => Some(ControlType::SplitButton),
            Self::Window => Some(ControlType::Window),
            Self::Dialog => Some(ControlType::Window),
            Self::Alertdialog => Some(ControlType::Window),
            Self::Application => Some(ControlType::Window),
            Self::Pane => Some(ControlType::Pane),
            Self::Rowgroup => Some(ControlType::Header),
            Self::Columnheader => Some(ControlType::HeaderItem),
            Self::Rowheader => Some(ControlType::HeaderItem),
            Self::Row => Some(ControlType::DataItem),
            Self::Table => Some(ControlType::Table),
            Self::Titlebar => Some(ControlType::TitleBar),
            Self::Separator => Some(ControlType::Separator),
            Self::Datepicker => Some(ControlType::Calendar),
            // Announcement/ambient roles map to static text on UIA.
            Self::Alert => Some(ControlType::Text),
            Self::Log => Some(ControlType::Text),
            Self::Marquee => Some(ControlType::Text),
            Self::Math => Some(ControlType::Text),
            Self::Note => Some(ControlType::Text),
            Self::Timer => Some(ControlType::Text),
            Self::Definition => Some(ControlType::Text),
            Self::Term => Some(ControlType::Text),
            Self::Banner => Some(ControlType::Pane),
            Self::Complementary => Some(ControlType::Pane),
            Self::Contentinfo => Some(ControlType::Pane),
            Self::Main => Some(ControlType::Pane),
            Self::Navigation => Some(ControlType::Pane),
            Self::Search => Some(ControlType::Group),
            Self::Switch => Some(ControlType::CheckBox),
            // No sensible UIA ControlType for these sentinels.
            Self::Generic | Self::None | Self::Unknown => None,
        }
    }
}

impl UnifiedRole {
    /// Convert to the wire-format string matching Python `AccessibilityRole.value`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Document => "document",
            Self::Article => "article",
            Self::Banner => "banner",
            Self::Complementary => "complementary",
            Self::Contentinfo => "contentinfo",
            Self::Form => "form",
            Self::Main => "main",
            Self::Navigation => "navigation",
            Self::Region => "region",
            Self::Search => "search",
            Self::Button => "button",
            Self::Checkbox => "checkbox",
            Self::Combobox => "combobox",
            Self::Dialog => "dialog",
            Self::Gridcell => "gridcell",
            Self::Link => "link",
            Self::Listbox => "listbox",
            Self::Menu => "menu",
            Self::Menubar => "menubar",
            Self::Menuitem => "menuitem",
            Self::Menuitemcheckbox => "menuitemcheckbox",
            Self::Menuitemradio => "menuitemradio",
            Self::Option => "option",
            Self::Progressbar => "progressbar",
            Self::Radio => "radio",
            Self::Radiogroup => "radiogroup",
            Self::Scrollbar => "scrollbar",
            Self::Searchbox => "searchbox",
            Self::Slider => "slider",
            Self::Spinbutton => "spinbutton",
            Self::Switch => "switch",
            Self::Tab => "tab",
            Self::Tablist => "tablist",
            Self::Tabpanel => "tabpanel",
            Self::Textbox => "textbox",
            Self::Toolbar => "toolbar",
            Self::Tooltip => "tooltip",
            Self::Tree => "tree",
            Self::Treegrid => "treegrid",
            Self::Treeitem => "treeitem",
            Self::Alert => "alert",
            Self::Alertdialog => "alertdialog",
            Self::Grid => "grid",
            Self::Heading => "heading",
            Self::Img => "img",
            Self::List => "list",
            Self::Listitem => "listitem",
            Self::Log => "log",
            Self::Marquee => "marquee",
            Self::Math => "math",
            Self::Note => "note",
            Self::Separator => "separator",
            Self::Status => "status",
            Self::Table => "table",
            Self::Cell => "cell",
            Self::Columnheader => "columnheader",
            Self::Row => "row",
            Self::Rowgroup => "rowgroup",
            Self::Rowheader => "rowheader",
            Self::Timer => "timer",
            Self::Definition => "definition",
            Self::Directory => "directory",
            Self::Figure => "figure",
            Self::Group => "group",
            Self::Paragraph => "paragraph",
            Self::Term => "term",
            Self::Generic => "generic",
            Self::StaticText => "static_text",
            Self::None => "none",
            Self::Unknown => "unknown",
            Self::Window => "window",
            Self::Pane => "pane",
            Self::Titlebar => "titlebar",
            Self::Edit => "edit",
            Self::Custom => "custom",
            Self::Dataitem => "dataitem",
            Self::Datepicker => "datepicker",
            Self::Calendar => "calendar",
            Self::Hyperlink => "hyperlink",
            Self::Splitbutton => "splitbutton",
        }
    }

    /// Whether this role is typically interactive.
    pub fn is_interactive_role(&self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::Link
                | Self::Hyperlink
                | Self::Checkbox
                | Self::Radio
                | Self::Combobox
                | Self::Edit
                | Self::Textbox
                | Self::Searchbox
                | Self::Slider
                | Self::Spinbutton
                | Self::Switch
                | Self::Tab
                | Self::Menuitem
                | Self::Menuitemcheckbox
                | Self::Menuitemradio
                | Self::Treeitem
                | Self::Listitem
                | Self::Option
                | Self::Gridcell
                | Self::Splitbutton
        )
    }
}

/// Tri-state boolean for accessibility states that may not apply to an element.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriBool {
    True,
    False,
    /// State is not applicable to this element type.
    #[serde(rename = "n/a")]
    #[default]
    NotApplicable,
}

impl TriBool {
    pub fn from_option(opt: Option<bool>) -> Self {
        match opt {
            Some(true) => Self::True,
            Some(false) => Self::False,
            None => Self::NotApplicable,
        }
    }

    pub fn to_option(self) -> Option<bool> {
        match self {
            Self::True => Some(true),
            Self::False => Some(false),
            Self::NotApplicable => None,
        }
    }

    pub fn is_true(self) -> bool {
        self == Self::True
    }
}

/// Accessibility state flags for a node.
///
/// Wire-compatible with Python `AccessibilityState`: boolean fields serialize
/// the same way, and `TriBool` fields serialize to `true`/`false`/`null` matching
/// the Python `bool | None` pattern.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnifiedState {
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub is_disabled: bool,
    #[serde(default)]
    pub is_hidden: bool,
    #[serde(default)]
    pub is_expanded: TriBool,
    #[serde(default)]
    pub is_selected: TriBool,
    #[serde(default)]
    pub is_checked: TriBool,
    #[serde(default)]
    pub is_pressed: TriBool,
    #[serde(default)]
    pub is_readonly: bool,
    #[serde(default)]
    pub is_required: bool,
    #[serde(default)]
    pub is_multiselectable: bool,
    #[serde(default)]
    pub is_editable: bool,
    #[serde(default)]
    pub is_focusable: bool,
    #[serde(default)]
    pub is_modal: bool,
}

/// Bounding rectangle in screen coordinates (pixels).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl UnifiedBounds {
    pub fn center_x(&self) -> i32 {
        self.x + self.width / 2
    }

    pub fn center_y(&self) -> i32 {
        self.y + self.height / 2
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// Which backend produced this node (for fusion routing and provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeSource {
    Uia,
    Atspi,
    Ax,
    /// Merged from two sources (e.g., native window + UI Bridge data).
    Fused,
}

/// Native interaction patterns supported by platform accessibility APIs.
///
/// The interaction engine uses these to choose the best method for each action,
/// falling back to coordinate-based clicking only when no native pattern is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionPattern {
    /// UIA InvokePattern / AT-SPI "click" action / AX AXPress
    Invoke,
    /// UIA ValuePattern / AT-SPI EditableText / AX AXValue
    Value,
    /// UIA TogglePattern
    Toggle,
    /// UIA ExpandCollapsePattern
    ExpandCollapse,
    /// UIA SelectionPattern / AT-SPI Selection
    Selection,
    /// UIA ScrollPattern / AT-SPI scroll action
    Scroll,
    /// UIA RangeValuePattern (sliders, spinners)
    RangeValue,
    /// UIA TextPattern (rich text manipulation)
    Text,
    /// Fallback: move cursor and click at element center coordinates
    CoordinateClick,
}

/// A single node in the unified accessibility tree.
///
/// Wire-compatible with Python `AccessibilityNode`: core fields (`ref`, `role`,
/// `name`, `value`, `bounds`, `state`, `is_interactive`, `children`) match exactly.
/// Extra fields (`source`, `platform_handle`, `supported_patterns`, `generation`)
/// are serialized but ignored by the Python side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedNode {
    /// Stable reference ID (e.g., `"@e1"`, `"@e5"`).
    #[serde(rename = "ref")]
    pub ref_id: Ref,

    /// Accessibility role.
    pub role: UnifiedRole,

    /// Accessible name (label text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Current value (for inputs, sliders, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,

    /// Accessible description (additional context).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Bounding rectangle in screen coordinates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<UnifiedBounds>,

    /// Current state flags.
    #[serde(default)]
    pub state: UnifiedState,

    /// Whether this element accepts user interaction.
    #[serde(default)]
    pub is_interactive: bool,

    /// Hierarchical level (for headings, tree items).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,

    /// Automation ID / test ID attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,

    /// CSS class name or native control class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,

    /// HTML tag name (for web elements).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_tag: Option<String>,

    /// URL for link elements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Child nodes in the tree.
    #[serde(default)]
    pub children: Vec<UnifiedNode>,

    // --- Fusion & cache metadata (ignored by Python) ---
    /// Which backend produced this node.
    #[serde(default = "default_node_source")]
    pub source: NodeSource,

    /// Opaque handle to the platform element for interaction dispatch.
    /// Indexes into the adapter's handle table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_handle: Option<u64>,

    /// Native interaction patterns this element supports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_patterns: Vec<InteractionPattern>,

    /// Cache generation (for incremental diff tracking).
    #[serde(default)]
    pub generation: u64,
}

fn default_node_source() -> NodeSource {
    NodeSource::Uia
}

/// Test-only `Default` implementation for `UnifiedNode`.
///
/// Lives at module level (not inside a `mod tests` block) so every sibling
/// test module — `query::tests`, `query::typed::tests`,
/// `flaui_conformance_test`, etc. — can call `UnifiedNode::default()`
/// without each duplicating the impl (which would collide at link time).
#[cfg(test)]
impl Default for UnifiedNode {
    fn default() -> Self {
        Self {
            ref_id: String::new(),
            role: UnifiedRole::Unknown,
            name: None,
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: false,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: None,
            supported_patterns: vec![],
            generation: 0,
            children: vec![],
        }
    }
}

impl UnifiedNode {
    /// Count total nodes in the subtree (including self).
    pub fn total_node_count(&self) -> u32 {
        1 + self
            .children
            .iter()
            .map(|c| c.total_node_count())
            .sum::<u32>()
    }

    /// Count interactive nodes in the subtree (including self).
    pub fn interactive_node_count(&self) -> u32 {
        let self_count = if self.is_interactive { 1 } else { 0 };
        self_count
            + self
                .children
                .iter()
                .map(|c| c.interactive_node_count())
                .sum::<u32>()
    }

    /// Find a node by ref ID in the subtree.
    pub fn find_by_ref(&self, ref_id: &str) -> Option<&UnifiedNode> {
        if self.ref_id == ref_id {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find_by_ref(ref_id) {
                return Some(found);
            }
        }
        None
    }

    /// Find a node by ref ID in the subtree (mutable).
    pub fn find_by_ref_mut(&mut self, ref_id: &str) -> Option<&mut UnifiedNode> {
        if self.ref_id == ref_id {
            return Some(self);
        }
        for child in &mut self.children {
            if let Some(found) = child.find_by_ref_mut(ref_id) {
                return Some(found);
            }
        }
        None
    }

    /// Find a node by platform handle in the subtree.
    pub fn find_by_handle(&self, handle: u64) -> Option<&UnifiedNode> {
        if self.platform_handle == Some(handle) {
            return Some(self);
        }
        for child in &self.children {
            if let Some(found) = child.find_by_handle(handle) {
                return Some(found);
            }
        }
        None
    }

    /// Collect all interactive nodes in the subtree.
    pub fn collect_interactive(&self) -> Vec<&UnifiedNode> {
        let mut result = Vec::new();
        self.collect_interactive_inner(&mut result);
        result
    }

    fn collect_interactive_inner<'a>(&'a self, result: &mut Vec<&'a UnifiedNode>) {
        if self.is_interactive {
            result.push(self);
        }
        for child in &self.children {
            child.collect_interactive_inner(result);
        }
    }
}

/// Complete accessibility tree snapshot at a point in time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedSnapshot {
    /// Root node of the accessibility tree.
    pub root: UnifiedNode,

    /// Unix timestamp in milliseconds.
    pub timestamp_ms: u64,

    /// Primary backend used for this snapshot.
    pub source: NodeSource,

    /// Page URL (for web targets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Page or window title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// Total number of nodes in the tree.
    pub total_nodes: u32,

    /// Number of interactive nodes.
    pub interactive_nodes: u32,

    /// Cache generation for incremental diff tracking.
    pub generation: u64,
}

impl UnifiedSnapshot {
    /// Create a snapshot from a root node, computing counts.
    pub fn from_root(
        root: UnifiedNode,
        source: NodeSource,
        url: Option<String>,
        title: Option<String>,
        generation: u64,
    ) -> Self {
        let total_nodes = root.total_node_count();
        let interactive_nodes = root.interactive_node_count();
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            root,
            timestamp_ms,
            source,
            url,
            title,
            total_nodes,
            interactive_nodes,
            generation,
        }
    }

    /// Find a node by ref ID.
    pub fn node_by_ref(&self, ref_id: &str) -> Option<&UnifiedNode> {
        self.root.find_by_ref(ref_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_role_serde_round_trip() {
        let role = UnifiedRole::Button;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"button\"");
        let deserialized: UnifiedRole = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, role);
    }

    #[test]
    fn test_tribool_from_option() {
        assert_eq!(TriBool::from_option(Some(true)), TriBool::True);
        assert_eq!(TriBool::from_option(Some(false)), TriBool::False);
        assert_eq!(TriBool::from_option(None), TriBool::NotApplicable);
    }

    #[test]
    fn test_unified_node_counts() {
        let root = UnifiedNode {
            ref_id: "@e0".into(),
            role: UnifiedRole::Window,
            name: Some("Test".into()),
            value: None,
            description: None,
            bounds: None,
            state: UnifiedState::default(),
            is_interactive: false,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: None,

            supported_patterns: vec![],
            generation: 0,
            children: vec![
                UnifiedNode {
                    ref_id: "@e1".into(),
                    role: UnifiedRole::Button,
                    name: Some("OK".into()),
                    value: None,
                    description: None,
                    bounds: Some(UnifiedBounds {
                        x: 10,
                        y: 20,
                        width: 80,
                        height: 30,
                    }),
                    state: UnifiedState::default(),
                    is_interactive: true,
                    level: None,
                    automation_id: None,
                    class_name: None,
                    html_tag: None,
                    url: None,
                    source: NodeSource::Uia,
                    platform_handle: None,

                    supported_patterns: vec![InteractionPattern::Invoke],
                    generation: 0,
                    children: vec![],
                },
                UnifiedNode {
                    ref_id: "@e2".into(),
                    role: UnifiedRole::StaticText,
                    name: Some("Hello".into()),
                    value: None,
                    description: None,
                    bounds: None,
                    state: UnifiedState::default(),
                    is_interactive: false,
                    level: None,
                    automation_id: None,
                    class_name: None,
                    html_tag: None,
                    url: None,
                    source: NodeSource::Uia,
                    platform_handle: None,

                    supported_patterns: vec![],
                    generation: 0,
                    children: vec![],
                },
            ],
        };

        assert_eq!(root.total_node_count(), 3);
        assert_eq!(root.interactive_node_count(), 1);
        assert!(root.find_by_ref("@e1").is_some());
        assert!(root.find_by_ref("@e99").is_none());
    }

    #[test]
    fn test_unified_bounds_center() {
        let bounds = UnifiedBounds {
            x: 100,
            y: 200,
            width: 80,
            height: 40,
        };
        assert_eq!(bounds.center_x(), 140);
        assert_eq!(bounds.center_y(), 220);
        assert!(!bounds.is_empty());

        let empty = UnifiedBounds {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
        assert!(empty.is_empty());
    }

    #[test]
    fn test_node_json_wire_compatibility() {
        // Verify the JSON field names match Python AccessibilityNode
        let node = UnifiedNode {
            ref_id: "@e1".into(),
            role: UnifiedRole::Button,
            name: Some("Submit".into()),
            value: None,
            description: None,
            bounds: Some(UnifiedBounds {
                x: 10,
                y: 20,
                width: 100,
                height: 30,
            }),
            state: UnifiedState {
                is_focused: true,
                is_disabled: false,
                ..Default::default()
            },
            is_interactive: true,
            level: None,
            automation_id: Some("btn_submit".into()),
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: None,

            supported_patterns: vec![],
            generation: 0,
            children: vec![],
        };

        let json = serde_json::to_value(&node).unwrap();
        // Check Python-compatible field names
        assert_eq!(json["ref"], "@e1");
        assert_eq!(json["role"], "button");
        assert_eq!(json["name"], "Submit");
        assert_eq!(json["is_interactive"], true);
        assert_eq!(json["state"]["is_focused"], true);
        assert_eq!(json["bounds"]["x"], 10);
        assert_eq!(json["automation_id"], "btn_submit");
    }
}
