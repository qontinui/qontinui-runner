//! Windows UI Automation adapter.
//!
//! Uses the `windows` crate COM bindings to access the Windows UIA API.
//! All UIA COM calls are dispatched onto a dedicated OS thread via
//! `tokio::task::spawn_blocking` since they are blocking.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use windows::core::{Interface, BSTR, VARIANT};
use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
    IUIAutomationFocusChangedEventHandler, IUIAutomationFocusChangedEventHandler_Impl,
    IUIAutomationInvokePattern, IUIAutomationRangeValuePattern, IUIAutomationScrollPattern,
    IUIAutomationTogglePattern, IUIAutomationTreeWalker, IUIAutomationValuePattern,
    ScrollAmount_NoAmount, TreeScope_Children, UIA_CONTROLTYPE_ID, UIA_NamePropertyId,
    UIA_PATTERN_ID, UIA_ProcessIdPropertyId,
};

use super::super::events::A11yEvent;
use super::super::model::{
    InteractionPattern, NodeSource, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use super::super::traits::{
    ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter,
};

// Pattern IDs as UIA_PATTERN_ID constants.
const INVOKE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10000);
const VALUE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10002);
const TOGGLE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10015);
const EXPAND_COLLAPSE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10005);
const SELECTION_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10001);
const SCROLL_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10004);
const RANGE_VALUE_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10003);
const TEXT_PATTERN: UIA_PATTERN_ID = UIA_PATTERN_ID(10014);

/// Map UIA control type IDs to `UnifiedRole`.
fn control_type_to_role(control_type: UIA_CONTROLTYPE_ID) -> UnifiedRole {
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
fn bool_from_win(b: BOOL) -> bool {
    b.0 != 0
}

/// Wrapper to make a single COM element Send across the spawn_blocking boundary.
/// SAFETY: UIA COM objects are safe to access from any thread when COM is
/// initialized with COINIT_MULTITHREADED.
struct SendElement(IUIAutomationElement);
unsafe impl Send for SendElement {}

/// Shared state between the adapter and the COM thread.
struct UiaState {
    automation: IUIAutomation,
    walker: IUIAutomationTreeWalker,
    root_element: Option<IUIAutomationElement>,
    handle_counter: AtomicU64,
    handles: Arc<DashMap<u64, IUIAutomationElement>>,
}

// SAFETY: IUIAutomation and IUIAutomationElement are COM pointers that are
// thread-safe when COM is initialized with COINIT_MULTITHREADED.
unsafe impl Send for UiaState {}
unsafe impl Sync for UiaState {}

impl UiaState {
    fn next_handle(&self) -> u64 {
        self.handle_counter.fetch_add(1, Ordering::Relaxed)
    }

    fn store_element(&self, element: &IUIAutomationElement) -> u64 {
        let handle = self.next_handle();
        self.handles.insert(handle, element.clone());
        handle
    }

    fn get_element(&self, handle: u64) -> Option<IUIAutomationElement> {
        self.handles.get(&handle).map(|e| e.value().clone())
    }

    /// Check whether an element supports a given UIA pattern.
    fn has_pattern(&self, element: &IUIAutomationElement, pattern_id: UIA_PATTERN_ID) -> bool {
        unsafe {
            element
                .GetCurrentPattern(pattern_id)
                .is_ok()
        }
    }

    /// Detect which interaction patterns an element supports.
    fn detect_patterns(&self, element: &IUIAutomationElement) -> Vec<InteractionPattern> {
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
            if self.has_pattern(element, pattern_id) {
                patterns.push(pattern_enum);
            }
        }

        // All elements support coordinate click as a fallback.
        patterns.push(InteractionPattern::CoordinateClick);
        patterns
    }

    /// Build a `UnifiedNode` from a UIA element, recursing into children.
    fn build_node(
        &self,
        element: &IUIAutomationElement,
        current_depth: u32,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> Option<UnifiedNode> {
        // Depth limit check.
        if let Some(max) = max_depth {
            if current_depth > max {
                return None;
            }
        }

        unsafe {
            // Read properties, gracefully handling failures.
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

            // Bounding rectangle.
            let bounds = element.CurrentBoundingRectangle().ok().and_then(|r| {
                let width = r.right - r.left;
                let height = r.bottom - r.top;
                // Skip zero-size elements unless including hidden.
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

            // State flags.
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

            let supported_patterns = self.detect_patterns(element);
            let is_interactive = role.is_interactive_role();
            let handle = self.store_element(element);

            // Recurse into children using the control view walker.
            let mut children = Vec::new();
            if max_depth.map_or(true, |m| current_depth < m) {
                if let Ok(child) = self.walker.GetFirstChildElement(element) {
                    let mut current_child = Some(child);
                    while let Some(ref child_elem) = current_child {
                        if let Some(child_node) = self.build_node(
                            child_elem,
                            current_depth + 1,
                            max_depth,
                            include_hidden,
                        ) {
                            children.push(child_node);
                        }
                        current_child = self.walker.GetNextSiblingElement(child_elem).ok();
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
}

/// Windows UI Automation adapter.
///
/// All COM calls are dispatched through `tokio::task::spawn_blocking`.
pub struct UiaAdapter {
    state: Option<Arc<UiaState>>,
    connected: Arc<AtomicBool>,
}

impl UiaAdapter {
    pub fn new() -> Self {
        Self {
            state: None,
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Initialize COM and create UIA objects on a blocking thread.
    async fn init_uia(&self) -> anyhow::Result<Arc<UiaState>> {
        let result = tokio::task::spawn_blocking(|| unsafe {
            // Initialize COM for multithreaded apartment.
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("Failed to initialize COM")?;

            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .context("Failed to create IUIAutomation")?;

            let walker = automation
                .ControlViewWalker()
                .context("Failed to get ControlViewWalker")?;

            let state = UiaState {
                automation,
                walker,
                root_element: None,
                handle_counter: AtomicU64::new(1),
                handles: Arc::new(DashMap::new()),
            };

            // Wrap in SendElement-style trick: we return an Arc<UiaState> which
            // has unsafe Send/Sync impls.
            Ok::<_, anyhow::Error>(Arc::new(state))
        })
        .await??;

        Ok(result)
    }

    /// Find a UIA element matching the connection target.
    async fn find_root(
        state: &Arc<UiaState>,
        target: ConnectionTarget,
        _timeout_ms: u64,
    ) -> anyhow::Result<SendElement> {
        let state = state.clone();
        tokio::task::spawn_blocking(move || unsafe {
            let elem = match target {
                ConnectionTarget::Desktop => state
                    .automation
                    .GetRootElement()
                    .context("Failed to get desktop root element")?,

                ConnectionTarget::ProcessId(pid) => {
                    let desktop = state
                        .automation
                        .GetRootElement()
                        .context("Failed to get desktop root")?;

                    let variant = VARIANT::from(pid as i32);
                    let condition = state
                        .automation
                        .CreatePropertyCondition(UIA_ProcessIdPropertyId, &variant)
                        .context("Failed to create process ID condition")?;

                    desktop
                        .FindFirst(TreeScope_Children, &condition)
                        .context(format!("No window found for PID {}", pid))?
                }

                ConnectionTarget::WindowTitle(title) => {
                    let desktop = state
                        .automation
                        .GetRootElement()
                        .context("Failed to get desktop root")?;

                    // Try exact match via property condition first.
                    let variant = VARIANT::from(BSTR::from(&title));
                    let condition = state
                        .automation
                        .CreatePropertyCondition(UIA_NamePropertyId, &variant)
                        .context("Failed to create name condition")?;

                    if let Ok(elem) = desktop.FindFirst(TreeScope_Children, &condition) {
                        return Ok(SendElement(elem));
                    }

                    // Fall back to substring search by walking children.
                    let title_lower = title.to_lowercase();
                    let walker = &state.walker;
                    let mut child = walker.GetFirstChildElement(&desktop).ok();
                    while let Some(ref elem) = child {
                        if let Ok(name) = elem.CurrentName() {
                            let name_str = name.to_string();
                            if name_str.to_lowercase().contains(&title_lower) {
                                return Ok(SendElement(elem.clone()));
                            }
                        }
                        child = walker.GetNextSiblingElement(elem).ok();
                    }

                    return Err(anyhow!("No window found matching title '{}'", title));
                }

            };

            Ok(SendElement(elem))
        })
        .await?
    }
}

#[async_trait]
impl PlatformAdapter for UiaAdapter {
    fn backend_name(&self) -> &'static str {
        "uia"
    }

    async fn connect(
        &mut self,
        target: ConnectionTarget,
        timeout_ms: u64,
    ) -> anyhow::Result<()> {
        if self.connected.load(Ordering::Relaxed) {
            self.disconnect().await?;
        }

        let state = self.init_uia().await?;
        let root = Self::find_root(&state, target, timeout_ms).await?;

        // Store root element. Re-create the Arc with root_element set,
        // transferring the existing handle table.
        let new_state = Arc::new(UiaState {
            automation: state.automation.clone(),
            walker: state.walker.clone(),
            root_element: Some(root.0),
            handle_counter: AtomicU64::new(state.handle_counter.load(Ordering::Relaxed)),
            handles: state.handles.clone(),
        });

        self.state = Some(new_state);
        self.connected.store(true, Ordering::Relaxed);
        debug!("UIA adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        if let Some(state) = self.state.take() {
            state.handles.clear();
        }
        self.connected.store(false, Ordering::Relaxed);
        debug!("UIA adapter disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    async fn capture_tree(
        &self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| anyhow!("UIA adapter not connected"))?
            .clone();

        tokio::task::spawn_blocking(move || {
            let root_element = state
                .root_element
                .as_ref()
                .ok_or_else(|| anyhow!("No root element set"))?;

            // Clear old handles before re-capture.
            state.handles.clear();
            state.handle_counter.store(1, Ordering::Relaxed);

            state
                .build_node(root_element, 0, max_depth, include_hidden)
                .ok_or_else(|| anyhow!("Root element could not be read (possibly destroyed)"))
        })
        .await?
    }

    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        let state = match self.state.as_ref() {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        let (tx, rx) = mpsc::channel::<A11yEvent>(256);

        // Register a focus-changed event handler on a blocking thread.
        tokio::task::spawn_blocking(move || unsafe {
            let handler = FocusChangedHandler { tx };
            let handler: IUIAutomationFocusChangedEventHandler = handler.into();
            if let Err(e) = state.automation.AddFocusChangedEventHandler(None, &handler) {
                warn!("Failed to register UIA focus event handler: {}", e);
            }
        });

        Ok(Some(rx))
    }

    async fn interact(
        &self,
        platform_handle: u64,
        pattern: InteractionPattern,
        params: InteractionParams,
    ) -> anyhow::Result<InteractionResult> {
        let state = self
            .state
            .as_ref()
            .ok_or_else(|| anyhow!("UIA adapter not connected"))?
            .clone();

        // Look up the element inside the blocking closure to avoid sending
        // non-Send COM pointers across the spawn_blocking boundary.
        tokio::task::spawn_blocking(move || {
            let element = state
                .get_element(platform_handle)
                .ok_or_else(|| anyhow!("No element found for handle {}", platform_handle))?;

            unsafe { interact_with_element(&element, pattern, params) }
        })
        .await?
    }

    async fn supported_patterns(&self, platform_handle: u64) -> Vec<InteractionPattern> {
        let state = match self.state.as_ref() {
            Some(s) => s.clone(),
            None => return vec![],
        };

        tokio::task::spawn_blocking(move || {
            match state.get_element(platform_handle) {
                Some(element) => state.detect_patterns(&element),
                None => vec![],
            }
        })
        .await
        .unwrap_or_default()
    }
}

/// Perform a UIA interaction on a specific element.
///
/// # Safety
/// Caller must ensure COM is initialized on the current thread.
unsafe fn interact_with_element(
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

        InteractionPattern::Text
        | InteractionPattern::Selection => Ok(InteractionResult::err(
            pattern,
            format!("{:?} not supported by UIA adapter", pattern),
        )),
    }
}

/// COM event handler for UIA focus-changed events.
#[windows::core::implement(IUIAutomationFocusChangedEventHandler)]
struct FocusChangedHandler {
    tx: mpsc::Sender<A11yEvent>,
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
            ref_id: String::new(), // Will be resolved by RefManager later.
            node_name,
        };

        // Non-blocking send; drop event if channel is full.
        let _ = self.tx.try_send(event);

        Ok(())
    }
}
