//! Typed-control wrappers for role-narrowed ergonomic access.
//!
//! Parallel to FlaUI's `AsButton` / `AsTextBox` / `AsCheckBox` pattern: each
//! struct borrows a [`UnifiedNode`] and asserts its [`UnifiedRole`] matches.
//! The wrappers expose a few capability-narrowed accessors (e.g.
//! [`Button::invoke`] returning an `InvokeRequest`, [`CheckBox::is_checked`])
//! so callers can write `if let Some(btn) = try_as_button(node) { ... }`
//! instead of sprinkling role checks in calling code.
//!
//! The interaction-dispatching methods do **not** invoke COM / AT-SPI / AX
//! themselves. They return a pattern descriptor (a small, `Send` struct
//! wrapping the `platform_handle`, the chosen [`InteractionPattern`], and
//! any [`InteractionParams`]) that the caller hands to the adapter's
//! `interact(handle, pattern, params)`. This preserves the existing
//! layering: typed wrappers stay on the pure side of the Rust boundary,
//! and the async adapter layer stays the single place COM is touched.

use super::super::model::{
    ControlType, InteractionPattern, TriBool, UnifiedNode, UnifiedRole,
};
use super::super::traits::InteractionParams;

/// Error returned when a typed wrapper is constructed from a node whose
/// role does not match the expected control type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedError {
    pub expected: ControlType,
    pub actual: UnifiedRole,
}

impl std::fmt::Display for TypedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "typed-control mismatch: expected {:?}, got role {:?}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for TypedError {}

/// Descriptor produced by typed-wrapper action methods.
///
/// Carries everything the adapter needs to dispatch an interaction: the
/// element's `platform_handle`, the [`InteractionPattern`] to use, and
/// any [`InteractionParams`]. Returned by methods like [`Button::invoke`]
/// so callers can choose *when* to perform the actual (async, COM) call.
///
/// The `handle` field is `Option<u64>` because in-memory / test
/// [`UnifiedNode`]s may not have a `platform_handle` — callers inspect it
/// and decide how to handle the missing-handle case (usually: fall back to
/// [`InteractionPattern::CoordinateClick`] at element-bounds center).
#[derive(Debug, Clone)]
pub struct InteractionRequest {
    pub handle: Option<u64>,
    pub pattern: InteractionPattern,
    pub params: InteractionParams,
}

impl InteractionRequest {
    fn new(handle: Option<u64>, pattern: InteractionPattern, params: InteractionParams) -> Self {
        Self {
            handle,
            pattern,
            params,
        }
    }
}

/// Assert that `node.role` matches `expected`; produce a [`TypedError`] otherwise.
fn expect_role(node: &UnifiedNode, expected: ControlType) -> Result<(), TypedError> {
    if node.role == expected.to_unified_role() {
        Ok(())
    } else {
        Err(TypedError {
            expected,
            actual: node.role,
        })
    }
}

// ─── Button ─────────────────────────────────────────────────────────────────

/// Typed wrapper around a `Button` [`UnifiedNode`].
#[derive(Debug)]
pub struct Button<'a> {
    node: &'a UnifiedNode,
}

impl<'a> Button<'a> {
    /// Borrow a `Button` from a node, or return a [`TypedError`] if the
    /// role is wrong. Prefer [`try_as_button`] for the `Option` flavour.
    pub fn try_new(node: &'a UnifiedNode) -> Result<Self, TypedError> {
        expect_role(node, ControlType::Button)?;
        Ok(Self { node })
    }

    /// The underlying node (escape hatch for callers needing more than the
    /// typed surface exposes).
    pub fn node(&self) -> &'a UnifiedNode {
        self.node
    }

    /// Button's accessible name.
    pub fn name(&self) -> Option<&str> {
        self.node.name.as_deref()
    }

    /// Produce an [`InteractionRequest`] that invokes the button. The
    /// preferred pattern is [`InteractionPattern::Invoke`] when the node
    /// advertises it, falling back to [`InteractionPattern::CoordinateClick`].
    pub fn invoke(&self) -> InteractionRequest {
        let pattern = if self
            .node
            .supported_patterns
            .contains(&InteractionPattern::Invoke)
        {
            InteractionPattern::Invoke
        } else {
            InteractionPattern::CoordinateClick
        };
        InteractionRequest::new(self.node.platform_handle, pattern, InteractionParams::None)
    }
}

/// Convenience constructor returning `None` on role mismatch.
pub fn try_as_button(node: &UnifiedNode) -> Option<Button<'_>> {
    Button::try_new(node).ok()
}

// ─── TextBox ────────────────────────────────────────────────────────────────

/// Typed wrapper around an `Edit` / `Textbox` [`UnifiedNode`].
///
/// Accepts any node whose role maps to `ControlType::Edit` — that includes
/// `UnifiedRole::Edit`, `UnifiedRole::Textbox`, and `UnifiedRole::Searchbox`.
#[derive(Debug)]
pub struct TextBox<'a> {
    node: &'a UnifiedNode,
}

impl<'a> TextBox<'a> {
    pub fn try_new(node: &'a UnifiedNode) -> Result<Self, TypedError> {
        // All three roles (Edit, Textbox, Searchbox) share ControlType::Edit.
        match node.role {
            UnifiedRole::Edit | UnifiedRole::Textbox | UnifiedRole::Searchbox => {
                Ok(Self { node })
            }
            other => Err(TypedError {
                expected: ControlType::Edit,
                actual: other,
            }),
        }
    }

    pub fn node(&self) -> &'a UnifiedNode {
        self.node
    }

    /// Current value (what the user sees in the field).
    pub fn get_value(&self) -> Option<&str> {
        self.node.value.as_deref()
    }

    /// Accessible name (typically the field's label).
    pub fn name(&self) -> Option<&str> {
        self.node.name.as_deref()
    }

    /// Produce an [`InteractionRequest`] that sets the text box's value.
    /// Uses [`InteractionPattern::Value`] when available, else falls back
    /// to a coordinate click (to focus) — callers that need keyboard
    /// synthesis should combine this with a subsequent key-input dispatch.
    pub fn set_value(&self, value: &str) -> InteractionRequest {
        let pattern = if self
            .node
            .supported_patterns
            .contains(&InteractionPattern::Value)
        {
            InteractionPattern::Value
        } else {
            InteractionPattern::CoordinateClick
        };
        let params = InteractionParams::Text {
            value: value.to_string(),
            clear_first: true,
        };
        InteractionRequest::new(self.node.platform_handle, pattern, params)
    }
}

pub fn try_as_textbox(node: &UnifiedNode) -> Option<TextBox<'_>> {
    TextBox::try_new(node).ok()
}

// ─── CheckBox ───────────────────────────────────────────────────────────────

/// Typed wrapper around a `CheckBox` [`UnifiedNode`].
#[derive(Debug)]
pub struct CheckBox<'a> {
    node: &'a UnifiedNode,
}

impl<'a> CheckBox<'a> {
    pub fn try_new(node: &'a UnifiedNode) -> Result<Self, TypedError> {
        expect_role(node, ControlType::CheckBox)?;
        Ok(Self { node })
    }

    pub fn node(&self) -> &'a UnifiedNode {
        self.node
    }

    /// Tri-state checked: `Some(true)` / `Some(false)` / `None` for
    /// indeterminate.
    pub fn is_checked(&self) -> Option<bool> {
        match self.node.state.is_checked {
            TriBool::True => Some(true),
            TriBool::False => Some(false),
            TriBool::NotApplicable => None,
        }
    }

    /// Produce an [`InteractionRequest`] that toggles the check state.
    /// Prefers [`InteractionPattern::Toggle`]; falls back to a coordinate
    /// click if toggle isn't advertised.
    pub fn toggle(&self) -> InteractionRequest {
        let pattern = if self
            .node
            .supported_patterns
            .contains(&InteractionPattern::Toggle)
        {
            InteractionPattern::Toggle
        } else {
            InteractionPattern::CoordinateClick
        };
        InteractionRequest::new(self.node.platform_handle, pattern, InteractionParams::None)
    }
}

pub fn try_as_checkbox(node: &UnifiedNode) -> Option<CheckBox<'_>> {
    CheckBox::try_new(node).ok()
}

// ─── ComboBox ───────────────────────────────────────────────────────────────

/// Typed wrapper around a `ComboBox` [`UnifiedNode`].
#[derive(Debug)]
pub struct ComboBox<'a> {
    node: &'a UnifiedNode,
}

impl<'a> ComboBox<'a> {
    pub fn try_new(node: &'a UnifiedNode) -> Result<Self, TypedError> {
        expect_role(node, ControlType::ComboBox)?;
        Ok(Self { node })
    }

    pub fn node(&self) -> &'a UnifiedNode {
        self.node
    }

    /// Current selection (the combo's value field).
    pub fn selected(&self) -> Option<&str> {
        self.node.value.as_deref()
    }

    /// Names of all option children (searches one level deep — works for
    /// `UnifiedRole::Option` and `UnifiedRole::Listitem`).
    pub fn option_names(&self) -> Vec<&str> {
        self.node
            .children
            .iter()
            .filter(|c| {
                matches!(
                    c.role,
                    UnifiedRole::Option | UnifiedRole::Listitem | UnifiedRole::Menuitem
                )
            })
            .filter_map(|c| c.name.as_deref())
            .collect()
    }

    /// Produce an [`InteractionRequest`] that selects an item by its
    /// visible name. Uses [`InteractionPattern::Value`] with the desired
    /// selection string; adapters that only expose a selection pattern
    /// can re-route internally.
    pub fn select(&self, item: &str) -> InteractionRequest {
        let pattern = if self
            .node
            .supported_patterns
            .contains(&InteractionPattern::Value)
        {
            InteractionPattern::Value
        } else if self
            .node
            .supported_patterns
            .contains(&InteractionPattern::Selection)
        {
            InteractionPattern::Selection
        } else {
            InteractionPattern::CoordinateClick
        };
        let params = InteractionParams::Text {
            value: item.to_string(),
            clear_first: true,
        };
        InteractionRequest::new(self.node.platform_handle, pattern, params)
    }
}

pub fn try_as_combobox(node: &UnifiedNode) -> Option<ComboBox<'_>> {
    ComboBox::try_new(node).ok()
}

// ─── Window ─────────────────────────────────────────────────────────────────

/// Typed wrapper around a `Window` [`UnifiedNode`].
#[derive(Debug)]
pub struct Window<'a> {
    node: &'a UnifiedNode,
}

impl<'a> Window<'a> {
    pub fn try_new(node: &'a UnifiedNode) -> Result<Self, TypedError> {
        expect_role(node, ControlType::Window)?;
        Ok(Self { node })
    }

    pub fn node(&self) -> &'a UnifiedNode {
        self.node
    }

    /// Window title (the accessible name).
    pub fn title(&self) -> Option<&str> {
        self.node.name.as_deref()
    }

    /// Window class name, when the adapter exposes it (UIA `ClassName`,
    /// AT-SPI `WM_CLASS`, AX `AXSubrole`).
    pub fn class_name(&self) -> Option<&str> {
        self.node.class_name.as_deref()
    }

    /// Whether the window is modal (UIA `IsModal` property / AT-SPI
    /// `MODAL` state / AX `AXModal`).
    pub fn is_modal(&self) -> bool {
        self.node.state.is_modal
    }
}

pub fn try_as_window(node: &UnifiedNode) -> Option<Window<'_>> {
    Window::try_new(node).ok()
}

#[cfg(test)]
mod tests {
    use super::super::super::model::{
        InteractionPattern, NodeSource, TriBool, UnifiedBounds, UnifiedNode, UnifiedRole,
        UnifiedState,
    };
    use super::*;

    fn node(role: UnifiedRole) -> UnifiedNode {
        UnifiedNode {
            ref_id: "@e1".into(),
            role,
            name: Some("Test".into()),
            value: None,
            description: None,
            bounds: Some(UnifiedBounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            }),
            state: UnifiedState::default(),
            is_interactive: true,
            level: None,
            automation_id: None,
            class_name: None,
            html_tag: None,
            url: None,
            source: NodeSource::Uia,
            platform_handle: Some(1),
            supported_patterns: vec![InteractionPattern::Invoke],
            generation: 0,
            children: vec![],
        }
    }

    #[test]
    fn button_wrap_success() {
        let n = node(UnifiedRole::Button);
        let btn = try_as_button(&n).expect("should wrap button");
        assert_eq!(btn.name(), Some("Test"));
        let req = btn.invoke();
        assert_eq!(req.pattern, InteractionPattern::Invoke);
        assert_eq!(req.handle, Some(1));
    }

    #[test]
    fn button_wrap_wrong_role() {
        let n = node(UnifiedRole::Textbox);
        assert!(try_as_button(&n).is_none());
        let err = Button::try_new(&n).unwrap_err();
        assert_eq!(err.expected, ControlType::Button);
        assert_eq!(err.actual, UnifiedRole::Textbox);
    }

    #[test]
    fn textbox_accepts_edit_and_textbox_and_searchbox() {
        assert!(try_as_textbox(&node(UnifiedRole::Edit)).is_some());
        assert!(try_as_textbox(&node(UnifiedRole::Textbox)).is_some());
        assert!(try_as_textbox(&node(UnifiedRole::Searchbox)).is_some());
        assert!(try_as_textbox(&node(UnifiedRole::Button)).is_none());
    }

    #[test]
    fn textbox_set_value_falls_back_without_value_pattern() {
        let mut n = node(UnifiedRole::Textbox);
        n.supported_patterns = vec![]; // no Value support
        let tb = try_as_textbox(&n).unwrap();
        let req = tb.set_value("hello");
        assert_eq!(req.pattern, InteractionPattern::CoordinateClick);
    }

    #[test]
    fn checkbox_is_checked_tristate() {
        let mut n = node(UnifiedRole::Checkbox);
        n.state.is_checked = TriBool::True;
        assert_eq!(try_as_checkbox(&n).unwrap().is_checked(), Some(true));

        n.state.is_checked = TriBool::False;
        assert_eq!(try_as_checkbox(&n).unwrap().is_checked(), Some(false));

        n.state.is_checked = TriBool::NotApplicable;
        assert_eq!(try_as_checkbox(&n).unwrap().is_checked(), None);
    }

    #[test]
    fn checkbox_toggle_prefers_toggle_pattern() {
        let mut n = node(UnifiedRole::Checkbox);
        n.supported_patterns = vec![InteractionPattern::Toggle];
        let cb = try_as_checkbox(&n).unwrap();
        assert_eq!(cb.toggle().pattern, InteractionPattern::Toggle);
    }

    #[test]
    fn combobox_option_names_walks_children() {
        let mut n = node(UnifiedRole::Combobox);
        n.children = vec![
            UnifiedNode {
                ref_id: "@e2".into(),
                role: UnifiedRole::Listitem,
                name: Some("Red".into()),
                ..Default::default()
            },
            UnifiedNode {
                ref_id: "@e3".into(),
                role: UnifiedRole::Listitem,
                name: Some("Green".into()),
                ..Default::default()
            },
        ];
        let cb = try_as_combobox(&n).unwrap();
        assert_eq!(cb.option_names(), vec!["Red", "Green"]);
    }

    #[test]
    fn window_title_and_modal() {
        let mut n = node(UnifiedRole::Window);
        n.name = Some("Settings".into());
        n.state.is_modal = true;
        let w = try_as_window(&n).unwrap();
        assert_eq!(w.title(), Some("Settings"));
        assert!(w.is_modal());
    }

    #[test]
    fn typed_error_displays_expected_and_actual() {
        let err = TypedError {
            expected: ControlType::Button,
            actual: UnifiedRole::Textbox,
        };
        let msg = err.to_string();
        assert!(msg.contains("Button"));
        assert!(msg.contains("Textbox"));
    }
}
