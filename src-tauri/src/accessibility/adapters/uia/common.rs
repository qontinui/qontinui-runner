//! Shared helpers used by every UIA backend (UIA3 today, future UIA2).
//!
//! This module factors out the parts of the UIA adapter that do *not* depend
//! on which COM factory class the backend picks. It mirrors the proxy-builder
//! pattern used by `atspi_adapter.rs`: backends supply the raw COM handles via
//! [`UiaBackend`], and the code here drives them.
//!
//! Concretely this module owns:
//!   * UIA pattern ID constants and control-type → [`UnifiedRole`] mapping.
//!   * Handle management ([`UiaHandleTable`]) — the weak u64 ↔ COM element
//!     reverse lookup used by `interact`/`supported_patterns`.
//!   * Tree walking via [`build_node`] — depth-limited recursive conversion
//!     from `IUIAutomationElement` to [`UnifiedNode`].
//!   * Pattern dispatch ([`interact_with_element`]) — the per-pattern match
//!     that resolves `InteractionPattern` to a COM pattern call plus fallback
//!     coordinate clicks.
//!   * The COM focus-changed event handler ([`FocusChangedHandler`]).
//!
//! Backends (e.g. [`super::uia3::Uia3Backend`]) only have to hand back the raw
//! `IUIAutomation` + `IUIAutomationTreeWalker` pair plus pick the right COM
//! class ID. That keeps the per-backend surface narrow and the call shapes
//! consistent across UIA3 / UIA2.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::anyhow;
use dashmap::DashMap;
use tokio::sync::mpsc;
use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::BOOL;
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
    IUIAutomationFocusChangedEventHandler_Impl, IUIAutomationInvokePattern,
    IUIAutomationRangeValuePattern, IUIAutomationScrollPattern, IUIAutomationTogglePattern,
    IUIAutomationTreeWalker, IUIAutomationValuePattern, ScrollAmount_NoAmount, UIA_CONTROLTYPE_ID,
    UIA_PATTERN_ID,
};

use crate::accessibility::events::A11yEvent;
use crate::accessibility::model::{
    InteractionPattern, NodeSource, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use crate::accessibility::traits::{InteractionParams, InteractionResult};

// ---------------------------------------------------------------------------
// Pattern IDs
// ---------------------------------------------------------------------------

pub(super) const INVOKE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10000);
pub(super) const VALUE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10002);
pub(super) const TOGGLE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10015);
pub(super) const EXPAND_COLLAPSE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10005);
pub(super) const SELECTION_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10001);
pub(super) const SCROLL_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10004);
pub(super) const RANGE_VALUE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10003);
pub(super) const TEXT_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10014);

// ---------------------------------------------------------------------------
// Send wrapper
// ---------------------------------------------------------------------------

/// Wrapper to make a single COM element `Send` across the `spawn_blocking`
/// boundary. SAFETY: UIA COM objects are safe to access from any thread when
/// COM is initialized with `COINIT_MULTITHREADED`.
pub(super) struct SendElement(pub(super) IUIAutomationElement);
unsafe impl Send for SendElement {}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Map UIA control type IDs to [`UnifiedRole`].
pub(super) fn control_type_to_role(control_type: UIA_CONTROLTYPE_ID) -> UnifiedRole {
    match control_type.0 {
        50000 => UnifiedRole::Button,
        50001 => UnifiedRole::Calendar,
        50002 => UnifiedRole::Checkbox,
        50003 => UnifiedRole::Combobox,
        50004 => UnifiedRole::Edit,
        50005 => UnifiedRole::Hyperlink,
        50006 => UnifiedRole::Img,
        50007 => UnifiedRole::Listitem,
        50008 => UnifiedRole::List,
        50009 => UnifiedRole::Menu,
        50010 => UnifiedRole::Menubar,
        50011 => UnifiedRole::Menuitem,
        50012 => UnifiedRole::Progressbar,
        50013 => UnifiedRole::Radio,
        50014 => UnifiedRole::Scrollbar,
        50015 => UnifiedRole::Slider,
        50016 => UnifiedRole::Spinbutton,
        50017 => UnifiedRole::Status,
        50018 => UnifiedRole::Tab,
        50019 => UnifiedRole::Tabpanel,
        50020 => UnifiedRole::StaticText,
        50021 => UnifiedRole::Toolbar,
        50022 => UnifiedRole::Tooltip,
        50023 => UnifiedRole::Tree,
        50024 => UnifiedRole::Treeitem,
        50025 => UnifiedRole::Custom,
        50026 => UnifiedRole::Group,
        50027 => UnifiedRole::Scrollbar,
        50028 => UnifiedRole::Dataitem,
        50029 => UnifiedRole::Document,
        50030 => UnifiedRole::Splitbutton,
        50031 => UnifiedRole::Window,
        50032 => UnifiedRole::Pane,
        50033 | 50034 => UnifiedRole::Heading,
        50035 => UnifiedRole::Table,
        50036 => UnifiedRole::Titlebar,
        50037 => UnifiedRole::Separator,
        _ => UnifiedRole::Unknown,
    }
}

/// Convert a Windows `BOOL` to a Rust `bool`.
pub(super) fn bool_from_win(b: BOOL) -> bool {
    b.0 != 0
}

// ---------------------------------------------------------------------------
// Handle management
// ---------------------------------------------------------------------------

/// `u64` handle ↔ `IUIAutomationElement` reverse-lookup table.
///
/// Handles are allocated monotonically and stored in a `DashMap` keyed by
/// `u64`. Every [`UnifiedNode`] built via [`build_node`] carries a
/// `platform_handle` from this table so later `interact`/`supported_patterns`
/// calls can reconstruct the COM pointer without walking the tree again.
pub(super) struct UiaHandleTable {
    counter: AtomicU64,
    handles: Arc<DashMap<u64, IUIAutomationElement>>,
}

// SAFETY: IUIAutomationElement pointers inside the DashMap are COM pointers
// that are thread-safe when COM is initialized with COINIT_MULTITHREADED.
unsafe impl Send for UiaHandleTable {}
unsafe impl Sync for UiaHandleTable {}

impl UiaHandleTable {
    pub(super) fn new() -> Self {
        // Clippy flags Arc<DashMap<_, IUIAutomationElement>> as "non-Send/Sync
        // inner". The COM pointers inside are thread-safe under
        // COINIT_MULTITHREADED — the unsafe `Send`/`Sync` impls above cover
        // the whole `UiaHandleTable` and the cloned `Arc` returned by
        // `shared_map`.
        #[allow(clippy::arc_with_non_send_sync)]
        let handles = Arc::new(DashMap::new());
        Self {
            counter: AtomicU64::new(1),
            handles,
        }
    }

    fn next_handle(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    /// Insert an element and return the new handle.
    pub(super) fn store(&self, element: &IUIAutomationElement) -> u64 {
        let handle = self.next_handle();
        self.handles.insert(handle, element.clone());
        handle
    }

    /// Reverse-lookup a handle to its element, if still present.
    pub(super) fn get(&self, handle: u64) -> Option<IUIAutomationElement> {
        self.handles.get(&handle).map(|e| e.value().clone())
    }

    /// Snapshot the current counter (for transferring across re-connections).
    pub(super) fn counter_snapshot(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Install a previously-snapshotted counter value.
    pub(super) fn set_counter(&self, value: u64) {
        self.counter.store(value, Ordering::Relaxed);
    }

    /// Forget every stored element and reset the counter.
    pub(super) fn clear(&self) {
        self.handles.clear();
        self.counter.store(1, Ordering::Relaxed);
    }

    /// Clone the inner `Arc<DashMap>` so a rebuilt state can reuse it.
    pub(super) fn shared_map(&self) -> Arc<DashMap<u64, IUIAutomationElement>> {
        self.handles.clone()
    }

    /// Adopt an existing `Arc<DashMap>` (used when rebuilding after `connect`).
    pub(super) fn from_existing(
        handles: Arc<DashMap<u64, IUIAutomationElement>>,
        counter: u64,
    ) -> Self {
        Self {
            counter: AtomicU64::new(counter),
            handles,
        }
    }
}

// ---------------------------------------------------------------------------
// Pattern detection
// ---------------------------------------------------------------------------

/// Check whether an element supports a given UIA pattern.
pub(super) fn has_pattern(element: &IUIAutomationElement, pattern_id: UIA_PATTERN_ID) -> bool {
    unsafe { element.GetCurrentPattern(pattern_id).is_ok() }
}

/// Detect which [`InteractionPattern`]s an element supports.
///
/// Always appends [`InteractionPattern::CoordinateClick`] as a universal
/// fallback.
pub(super) fn detect_patterns(element: &IUIAutomationElement) -> Vec<InteractionPattern> {
    let mut patterns = Vec::new();
    let pattern_checks: &[(UIA_PATTERN_ID, InteractionPattern)] = &[
        (INVOKE_PATTERN, InteractionPattern::Invoke),
        (VALUE_PATTERN, InteractionPattern::Value),
        (TOGGLE_PATTERN, InteractionPattern::Toggle),
        (EXPAND_COLLAPSE_PATTERN, InteractionPattern::ExpandCollapse),
        (SELECTION_PATTERN, InteractionPattern::Selection),
        (SCROLL_PATTERN, InteractionPattern::Scroll),
        (RANGE_VALUE_PATTERN, InteractionPattern::RangeValue),
        (TEXT_PATTERN, InteractionPattern::Text),
    ];

    for &(pattern_id, pattern_enum) in pattern_checks {
        if has_pattern(element, pattern_id) {
            patterns.push(pattern_enum);
        }
    }

    patterns.push(InteractionPattern::CoordinateClick);
    patterns
}

// ---------------------------------------------------------------------------
// Tree walking
// ---------------------------------------------------------------------------

/// Recursively build a [`UnifiedNode`] subtree rooted at `element`.
///
/// The tree-walk strategy (depth handling, sibling iteration, child filtering)
/// is identical across UIA3 and any future UIA2 backend — only the underlying
/// `IUIAutomation` / `IUIAutomationTreeWalker` factories differ.
pub(super) fn build_node(
    element: &IUIAutomationElement,
    walker: &IUIAutomationTreeWalker,
    handles: &UiaHandleTable,
    current_depth: u32,
    max_depth: Option<u32>,
    include_hidden: bool,
) -> Option<UnifiedNode> {
    if let Some(max) = max_depth {
        if current_depth > max {
            return None;
        }
    }

    unsafe {
        let control_type = element
            .CurrentControlType()
            .unwrap_or(UIA_CONTROLTYPE_ID(0));
        let role = control_type_to_role(control_type);

        let name = element.CurrentName().ok().and_then(|b| {
            let s = b.to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });

        let automation_id = element.CurrentAutomationId().ok().and_then(|b| {
            let s = b.to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });

        let class_name = element.CurrentClassName().ok().and_then(|b| {
            let s = b.to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        });

        let bounds = element.CurrentBoundingRectangle().ok().and_then(|r| {
            let width = r.right - r.left;
            let height = r.bottom - r.top;
            if width <= 0 && height <= 0 && !include_hidden {
                return None;
            }
            Some(UnifiedBounds {
                x: r.left,
                y: r.top,
                width,
                height,
            })
        });

        let is_enabled = element
            .CurrentIsEnabled()
            .map(bool_from_win)
            .unwrap_or(true);
        let has_focus = element
            .CurrentHasKeyboardFocus()
            .map(bool_from_win)
            .unwrap_or(false);
        let is_offscreen = element
            .CurrentIsOffscreen()
            .map(bool_from_win)
            .unwrap_or(false);

        if is_offscreen && !include_hidden {
            return None;
        }

        let state = UnifiedState {
            is_focused: has_focus,
            is_disabled: !is_enabled,
            is_hidden: is_offscreen,
            ..Default::default()
        };

        let supported_patterns = detect_patterns(element);
        let is_interactive = role.is_interactive_role();
        let handle = handles.store(element);

        let mut children = Vec::new();
        if max_depth.is_none_or(|m| current_depth < m) {
            if let Ok(child) = walker.GetFirstChildElement(element) {
                let mut current_child = Some(child);
                while let Some(ref child_elem) = current_child {
                    if let Some(child_node) = build_node(
                        child_elem,
                        walker,
                        handles,
                        current_depth + 1,
                        max_depth,
                        include_hidden,
                    ) {
                        children.push(child_node);
                    }
                    current_child = walker.GetNextSiblingElement(child_elem).ok();
                }
            }
        }

        Some(UnifiedNode {
            ref_id: String::new(),
            role,
            name,
            value: None,
            description: None,
            bounds,
            state,
            is_interactive,
            level: None,
            automation_id,
            class_name,
            html_tag: None,
            url: None,
            children,
            source: NodeSource::Uia,
            platform_handle: Some(handle),

            supported_patterns,
            generation: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Pattern dispatch
// ---------------------------------------------------------------------------

/// Perform a UIA interaction on a specific element.
///
/// # Safety
/// Caller must ensure COM is initialized on the current thread.
pub(super) unsafe fn interact_with_element(
    element: &IUIAutomationElement,
    pattern: InteractionPattern,
    params: InteractionParams,
) -> anyhow::Result<InteractionResult> {
    match pattern {
        InteractionPattern::Invoke => match element.GetCurrentPattern(INVOKE_PATTERN) {
            Ok(pat) => {
                let invoke: IUIAutomationInvokePattern = pat.cast()?;
                match invoke.Invoke() {
                    Ok(()) => Ok(InteractionResult::ok(pattern)),
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Invoke failed: {}", e),
                    )),
                }
            }
            Err(_) => Ok(InteractionResult::err(
                pattern,
                "Element does not support InvokePattern",
            )),
        },

        InteractionPattern::Value => {
            let text = match params {
                InteractionParams::Text { value, .. } => value,
                _ => {
                    return Ok(InteractionResult::err(
                        pattern,
                        "Value pattern requires Text params",
                    ))
                }
            };

            match element.GetCurrentPattern(VALUE_PATTERN) {
                Ok(pat) => {
                    let value_pat: IUIAutomationValuePattern = pat.cast()?;
                    let bstr = BSTR::from(&text);
                    match value_pat.SetValue(&bstr) {
                        Ok(()) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("SetValue failed: {}", e),
                        )),
                    }
                }
                Err(_) => Ok(InteractionResult::err(
                    pattern,
                    "Element does not support ValuePattern",
                )),
            }
        }

        InteractionPattern::Toggle => match element.GetCurrentPattern(TOGGLE_PATTERN) {
            Ok(pat) => {
                let toggle: IUIAutomationTogglePattern = pat.cast()?;
                match toggle.Toggle() {
                    Ok(()) => Ok(InteractionResult::ok(pattern)),
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Toggle failed: {}", e),
                    )),
                }
            }
            Err(_) => Ok(InteractionResult::err(
                pattern,
                "Element does not support TogglePattern",
            )),
        },

        InteractionPattern::ExpandCollapse => {
            let expand = match params {
                InteractionParams::ExpandCollapse { expand } => expand,
                _ => true,
            };

            match element.GetCurrentPattern(EXPAND_COLLAPSE_PATTERN) {
                Ok(pat) => {
                    let ec: IUIAutomationExpandCollapsePattern = pat.cast()?;
                    let result = if expand { ec.Expand() } else { ec.Collapse() };
                    match result {
                        Ok(()) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("ExpandCollapse failed: {}", e),
                        )),
                    }
                }
                Err(_) => Ok(InteractionResult::err(
                    pattern,
                    "Element does not support ExpandCollapsePattern",
                )),
            }
        }

        InteractionPattern::Scroll => {
            let (h, v) = match params {
                InteractionParams::ScrollDirection {
                    horizontal,
                    vertical,
                } => (horizontal, vertical),
                _ => (0, 0),
            };

            match element.GetCurrentPattern(SCROLL_PATTERN) {
                Ok(pat) => {
                    let scroll: IUIAutomationScrollPattern = pat.cast()?;
                    let h_amount = if h != 0 {
                        windows::Win32::UI::Accessibility::ScrollAmount(h)
                    } else {
                        ScrollAmount_NoAmount
                    };
                    let v_amount = if v != 0 {
                        windows::Win32::UI::Accessibility::ScrollAmount(v)
                    } else {
                        ScrollAmount_NoAmount
                    };
                    match scroll.Scroll(h_amount, v_amount) {
                        Ok(()) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("Scroll failed: {}", e),
                        )),
                    }
                }
                Err(_) => Ok(InteractionResult::err(
                    pattern,
                    "Element does not support ScrollPattern",
                )),
            }
        }

        InteractionPattern::RangeValue => {
            let val = match params {
                InteractionParams::RangeValue { value } => value,
                _ => {
                    return Ok(InteractionResult::err(
                        pattern,
                        "RangeValue pattern requires RangeValue params",
                    ))
                }
            };

            match element.GetCurrentPattern(RANGE_VALUE_PATTERN) {
                Ok(pat) => {
                    let rv: IUIAutomationRangeValuePattern = pat.cast()?;
                    match rv.SetValue(val) {
                        Ok(()) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("SetValue (range) failed: {}", e),
                        )),
                    }
                }
                Err(_) => Ok(InteractionResult::err(
                    pattern,
                    "Element does not support RangeValuePattern",
                )),
            }
        }

        InteractionPattern::CoordinateClick => match element.CurrentBoundingRectangle() {
            Ok(rect) => {
                let cx = rect.left + (rect.right - rect.left) / 2;
                let cy = rect.top + (rect.bottom - rect.top) / 2;

                use windows::Win32::UI::Input::KeyboardAndMouse::{
                    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE,
                    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT,
                };
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN,
                };

                let screen_w = GetSystemMetrics(SM_CXSCREEN) as f64;
                let screen_h = GetSystemMetrics(SM_CYSCREEN) as f64;

                let abs_x = ((cx as f64 / screen_w) * 65535.0) as i32;
                let abs_y = ((cy as f64 / screen_h) * 65535.0) as i32;

                let inputs = [
                    INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: abs_x,
                                dy: abs_y,
                                mouseData: 0,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTDOWN,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    },
                    INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: abs_x,
                                dy: abs_y,
                                mouseData: 0,
                                dwFlags: MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_LEFTUP,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    },
                ];

                let sent = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
                if sent == 2 {
                    Ok(InteractionResult::ok(pattern))
                } else {
                    Ok(InteractionResult::err(
                        pattern,
                        "SendInput failed for coordinate click",
                    ))
                }
            }
            Err(e) => Ok(InteractionResult::err(
                pattern,
                format!("Cannot get bounding rect for click: {}", e),
            )),
        },

        InteractionPattern::Text | InteractionPattern::Selection => Ok(InteractionResult::err(
            pattern,
            format!("{:?} not supported by UIA adapter", pattern),
        )),
    }
}

// ---------------------------------------------------------------------------
// Focus-changed event handler
// ---------------------------------------------------------------------------

/// COM event handler for UIA focus-changed events.
///
/// Shared between backends — every UIA version exposes the same
/// `AddFocusChangedEventHandler` surface, so the handler itself is backend-
/// agnostic.
#[windows::core::implement(
    windows::Win32::UI::Accessibility::IUIAutomationFocusChangedEventHandler
)]
pub(super) struct FocusChangedHandler {
    pub(super) tx: mpsc::Sender<A11yEvent>,
}

impl IUIAutomationFocusChangedEventHandler_Impl for FocusChangedHandler_Impl {
    fn HandleFocusChangedEvent(
        &self,
        sender: Option<&IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        let node_name = sender.and_then(|elem| unsafe {
            elem.CurrentName()
                .ok()
                .map(|b| b.to_string())
                .filter(|s| !s.is_empty())
        });

        let event = A11yEvent::FocusChanged {
            ref_id: String::new(),
            node_name,
        };

        let _ = self.tx.try_send(event);

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Backend-owned shared state
// ---------------------------------------------------------------------------

/// Per-connection UIA state shared between the adapter and blocking tasks.
///
/// Backend-agnostic: the `automation` and `walker` fields come from whichever
/// [`super::UiaBackend`] the selector picked. Kept in an `Arc` so spawn_blocking
/// closures can capture and move it safely despite the non-Send-looking COM
/// types (see the `unsafe impl Send/Sync` below).
pub(super) struct UiaState {
    pub(super) automation: IUIAutomation,
    pub(super) walker: IUIAutomationTreeWalker,
    pub(super) root_element: Option<IUIAutomationElement>,
    pub(super) handles: UiaHandleTable,
}

// SAFETY: IUIAutomation and IUIAutomationElement are COM pointers that are
// thread-safe when COM is initialized with COINIT_MULTITHREADED.
unsafe impl Send for UiaState {}
unsafe impl Sync for UiaState {}

impl UiaState {
    /// Rebuild the state with a root element set, preserving the existing
    /// handle table and counter.
    pub(super) fn with_root(state: &Arc<UiaState>, root: IUIAutomationElement) -> Arc<UiaState> {
        #[allow(clippy::arc_with_non_send_sync)] // UiaState has unsafe Send+Sync impls
        Arc::new(UiaState {
            automation: state.automation.clone(),
            walker: state.walker.clone(),
            root_element: Some(root),
            handles: UiaHandleTable::from_existing(
                state.handles.shared_map(),
                state.handles.counter_snapshot(),
            ),
        })
    }

    /// Capture the tree starting from the stored root element. Clears and
    /// reseeds the handle table on every call so stale handles from an earlier
    /// capture cannot be re-used.
    pub(super) fn capture_tree(
        &self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode> {
        let root_element = self
            .root_element
            .as_ref()
            .ok_or_else(|| anyhow!("No root element set"))?;

        self.handles.clear();

        build_node(
            root_element,
            &self.walker,
            &self.handles,
            0,
            max_depth,
            include_hidden,
        )
        .ok_or_else(|| anyhow!("Root element could not be read (possibly destroyed)"))
    }
}
