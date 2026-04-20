//! UIA3 backend — `CUIAutomation` COM class.
//!
//! This file owns the UIA-version-specific pieces: the COM class ID used to
//! create the root `IUIAutomation` interface, and the `find_root` call shapes
//! (which are *themselves* UIA3-specific only in that they use the
//! CreatePropertyCondition + FindFirst surface available on this backend).
//!
//! Everything else — tree walking, handle table, pattern dispatch, focus event
//! handler — lives in [`super::common`] and is shared with any future UIA2
//! backend.

use std::sync::Arc;

use anyhow::{anyhow, Context};
use windows::core::{BSTR, VARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationTreeWalker, TreeScope_Children, UIA_NamePropertyId,
    UIA_ProcessIdPropertyId,
};

use super::common::{SendElement, UiaHandleTable, UiaState};
use super::UiaBackend;
use crate::accessibility::traits::ConnectionTarget;

/// UIA3 backend. Uses the modern `CUIAutomation` COM class (available on
/// Windows 7 SP1+).
pub struct Uia3Backend;

impl Uia3Backend {
    pub const NAME: &'static str = "uia3";

    pub fn new() -> Self {
        Self
    }
}

impl Default for Uia3Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl UiaBackend for Uia3Backend {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn initialize(&self) -> anyhow::Result<(IUIAutomation, IUIAutomationTreeWalker)> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("Failed to initialize COM")?;

            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .context("Failed to create IUIAutomation")?;

            let walker = automation
                .ControlViewWalker()
                .context("Failed to get ControlViewWalker")?;

            Ok((automation, walker))
        }
    }
}

/// Build an initial [`UiaState`] with no root element selected yet.
///
/// Kept in this file (rather than [`super::common`]) because only a backend
/// knows how to produce the underlying `IUIAutomation` / `IUIAutomationTreeWalker`
/// pair — which is exactly what [`UiaBackend::initialize`] returns.
pub(super) fn init_state(backend: &dyn UiaBackend) -> anyhow::Result<Arc<UiaState>> {
    let (automation, walker) = backend.initialize()?;
    #[allow(clippy::arc_with_non_send_sync)] // UiaState has unsafe Send+Sync impls
    Ok(Arc::new(UiaState {
        automation,
        walker,
        root_element: None,
        handles: UiaHandleTable::new(),
    }))
}

/// Find the UIA element that corresponds to the caller-supplied
/// [`ConnectionTarget`].
///
/// Call shapes here (GetRootElement + FindFirst + walker-scan fallback) are
/// shared across UIA versions — they use interfaces present on both UIA2 and
/// UIA3 — so this lives in the UIA3 file purely because UIA3 is the only
/// backend today. A future UIA2 adapter can reuse this function as-is or
/// copy-adapt it if call shapes need to differ.
pub(super) fn find_root(
    state: &Arc<UiaState>,
    target: ConnectionTarget,
) -> anyhow::Result<SendElement> {
    unsafe {
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
    }
}
