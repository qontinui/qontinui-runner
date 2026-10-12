//! Windows UI Automation adapter.
//!
//! Previously a single 822-line `uia.rs` monolith; split into a `uia/`
//! directory so a second UIA backend (UIA2) can share the common helpers
//! without duplicating tree-walk / handle / dispatch logic.
//!
//! Module layout:
//!   * [`common`] — backend-agnostic helpers: tree walking, handle table,
//!     pattern dispatch, focus-event handler, pattern-ID constants.
//!   * [`uia3`] — the default backend, using `CUIAutomation` (Windows 7 SP1+).
//!   * This file — public [`UiaAdapter`] entry point, [`UiaBackend`] trait,
//!     and the backend selector.
//!
//! External code talks to [`UiaAdapter`] only; the `UiaBackend` trait and the
//! backend modules are `pub` so other code in the crate (e.g. a hypothetical
//! selector tuning knob) can reach them, but nothing outside the `accessibility`
//! module currently does.
//!
//! All UIA COM calls are dispatched onto a dedicated OS thread via
//! `tokio::task::spawn_blocking` since COM calls are blocking.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use windows::Win32::UI::Accessibility::{
    IUIAutomation, IUIAutomationFocusChangedEventHandler, IUIAutomationTreeWalker,
};

use super::super::events::A11yEvent;
use super::super::model::{InteractionPattern, UnifiedNode};
use super::super::traits::{
    ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter,
};

pub mod common;
pub mod uia3;

use crate::wedge_diagnostics::spawn_blocking_tracked;
use common::{detect_patterns, interact_with_element, FocusChangedHandler, UiaState};

// ---------------------------------------------------------------------------
// Backend trait
// ---------------------------------------------------------------------------

/// A swap-in UIA backend.
///
/// Narrowed to just the surface that actually differs between UIA3 (today)
/// and a future UIA2 binding: picking a COM class ID and building the
/// `IUIAutomation` + `IUIAutomationTreeWalker` pair. Everything else — tree
/// walking, handle management, pattern dispatch — is shared via
/// [`common`] and receives the pair through [`UiaState`].
///
/// Why this shape: UIA2 and UIA3 expose the same interface types
/// (`IUIAutomation`, `IUIAutomationElement`, …); the only real difference is
/// *which* COM class you `CoCreateInstance` and which `windows` crate feature
/// exposes it. Moving that one choice behind a trait keeps the bulk of the
/// code version-agnostic.
///
/// # Selection
///
/// Currently the selector in [`select_backend`] always picks [`uia3::Uia3Backend`].
/// When a UIA2 backend lands it will join this trait and the selector will
/// grow an `Auto` mode that falls through on empty trees or WinForms detection
/// — but that's deferred.
pub trait UiaBackend: Send + Sync {
    /// Short backend identifier, e.g. `"uia3"` or `"uia2"`. Used for logs.
    fn name(&self) -> &'static str;

    /// Initialize COM, create the root automation object, and build the
    /// control-view tree walker.
    ///
    /// Called inside a blocking task — implementations may use synchronous
    /// COM calls freely.
    fn initialize(&self) -> anyhow::Result<(IUIAutomation, IUIAutomationTreeWalker)>;
}

/// Which UIA backend to use.
///
/// Defaults to [`BackendChoice::Uia3`]. A future UIA2 backend will add
/// `Uia2`, and `Auto` mode will attempt UIA3 first and fall through on known
/// problem cases (empty trees, WinForms processes). None of those variants
/// exist yet — this enum is the hook for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackendChoice {
    /// Modern UIA3 (`CUIAutomation`). Default.
    #[default]
    Uia3,
}

/// Instantiate the boxed backend for a given [`BackendChoice`].
///
/// Kept as a separate function so it's obvious where new backends hook in.
fn select_backend(choice: BackendChoice) -> Box<dyn UiaBackend> {
    match choice {
        BackendChoice::Uia3 => Box::new(uia3::Uia3Backend::new()),
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Windows UI Automation adapter.
///
/// Public surface has not changed from the pre-split monolith: `new()` plus
/// the [`PlatformAdapter`] trait. A future constructor (e.g. `with_backend`)
/// can opt into UIA2 once that backend exists.
pub struct UiaAdapter {
    state: Option<Arc<UiaState>>,
    connected: Arc<AtomicBool>,
    backend: Box<dyn UiaBackend>,
    /// The focus-changed handler registered by `subscribe_events`, with the
    /// exact `IUIAutomation` it was registered on (every connect builds a
    /// fresh one, so the handler must be removed from the instance that owns
    /// it). At most one exists: it is removed before every re-subscribe, on
    /// `disconnect`/`connect`, and on drop. A `std` mutex because
    /// `subscribe_events` takes `&self`; it is never held across an `.await`.
    focus_subscription: Mutex<Option<FocusSubscription>>,
}

/// A live `AddFocusChangedEventHandler` registration, removed exactly once:
/// by [`FocusSubscription::remove`], or else when it is dropped.
///
/// The drop arm is what makes a cancelled `subscribe_events` safe: the
/// registration is made inside a blocking task, and if the future awaiting it
/// is dropped the task still completes and drops its `FocusSubscription`, which
/// then unregisters the handler instead of leaking it in UIA.
struct FocusSubscription {
    /// `None` once removed, which disarms the drop.
    registration: Option<FocusRegistration>,
}

/// The handler plus the exact `IUIAutomation` it was registered on.
struct FocusRegistration {
    automation: IUIAutomation,
    handler: IUIAutomationFocusChangedEventHandler,
}

// SAFETY: `automation` is an interface pointer created in the MTA (`uia3`
// initializes COM with COINIT_MULTITHREADED), so it may be used from any
// thread that is itself in the MTA — and the only use after the move,
// `FocusRegistration::remove`, joins the MTA before calling it. `handler` is
// our own `FocusChangedHandler`, whose only state is an `mpsc::Sender`
// (`Send`). The same argument as `UiaState`'s `unsafe impl Send`.
unsafe impl Send for FocusRegistration {}

impl FocusSubscription {
    fn new(automation: IUIAutomation, handler: IUIAutomationFocusChangedEventHandler) -> Self {
        Self {
            registration: Some(FocusRegistration {
                automation,
                handler,
            }),
        }
    }

    /// Unregister the handler now, on this thread. Blocking COM call — run it
    /// off the async runtime. Once removed, UIA drops its reference to the
    /// handler, which drops the channel sender and ends the receiver's stream.
    fn remove(mut self) {
        if let Some(registration) = self.registration.take() {
            registration.remove();
        }
    }
}

impl Drop for FocusSubscription {
    /// Remove a registration that was never explicitly removed, on a plain OS
    /// thread: `drop` may run on an async worker (or inside a blocking task
    /// whose awaiting future was cancelled), where it must neither block on
    /// COM nor wait for anything.
    fn drop(&mut self) {
        if let Some(registration) = self.registration.take() {
            std::thread::spawn(move || registration.remove());
        }
    }
}

impl FocusRegistration {
    fn remove(self) {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

        // The calling thread may never have touched COM (a fresh thread from
        // `Drop`, or a pooled blocking thread). Join the MTA first. S_OK and
        // S_FALSE ("already initialized in the MTA") each take a reference
        // that must be paired with CoUninitialize. RPC_E_CHANGED_MODE means
        // the thread is already in an STA (not expected: only our own threads
        // and tokio's blocking pool run this). COM is initialized there and
        // the removal is still attempted — its HRESULT is logged if it fails —
        // but that call took no reference, so none is released.
        //
        // SAFETY: plain COM initialization of the current thread.
        let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let paired = init.is_ok(); // S_OK or S_FALSE
        if !paired && init != RPC_E_CHANGED_MODE {
            warn!(
                "CoInitializeEx before removing the UIA focus handler failed: {:?}",
                init
            );
        }

        // SAFETY: `handler` was registered on this `automation` instance, and
        // COM is initialized on this thread (see above) — or, when
        // initialization failed outright, the call reports that as an error
        // instead of touching uninitialized COM state.
        if let Err(e) = unsafe {
            self.automation
                .RemoveFocusChangedEventHandler(&self.handler)
        } {
            warn!("Failed to remove UIA focus event handler: {}", e);
        }
        // Release the COM pointers before the apartment reference.
        drop(self);

        if paired {
            // SAFETY: pairs the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

impl Default for UiaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl UiaAdapter {
    /// Construct an adapter using the default ([`BackendChoice::Uia3`])
    /// backend.
    pub fn new() -> Self {
        Self::with_choice(BackendChoice::default())
    }

    /// Construct an adapter using a specific [`BackendChoice`].
    ///
    /// Currently only `Uia3` is meaningful; other variants will appear when
    /// the UIA2 backend is added.
    pub fn with_choice(choice: BackendChoice) -> Self {
        Self {
            state: None,
            connected: Arc::new(AtomicBool::new(false)),
            backend: select_backend(choice),
            focus_subscription: Mutex::new(None),
        }
    }

    /// Take the current focus subscription out of its slot, if any.
    fn take_focus_subscription(&self) -> Option<FocusSubscription> {
        self.focus_subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Remove the registered focus handler (if any) and wait for UIA to
    /// confirm, so no second registration can coexist with it.
    async fn release_focus_subscription(&self) {
        if let Some(sub) = self.take_focus_subscription() {
            if let Err(e) = spawn_blocking_tracked(move || sub.remove()).await {
                warn!("UIA focus handler removal task failed: {}", e);
            }
        }
    }

    /// Initialize COM + UIA on a blocking thread, delegating the COM-version
    /// choice to the configured [`UiaBackend`].
    async fn init_uia(&self) -> anyhow::Result<Arc<UiaState>> {
        // Spawn_blocking can't borrow `&self.backend`, so pick the backend
        // type up-front. Today that's always UIA3 — when UIA2 lands this
        // becomes a small match.
        let choice = match self.backend.name() {
            uia3::Uia3Backend::NAME => BackendChoice::Uia3,
            other => {
                return Err(anyhow!(
                    "Unknown UIA backend name '{}' (refactor bug)",
                    other
                ))
            }
        };

        spawn_blocking_tracked(move || {
            let backend = select_backend(choice);
            uia3::init_state(backend.as_ref())
        })
        .await?
    }
}

#[async_trait]
impl PlatformAdapter for UiaAdapter {
    fn backend_name(&self) -> &'static str {
        "uia"
    }

    async fn connect(&mut self, target: ConnectionTarget, _timeout_ms: u64) -> anyhow::Result<()> {
        if self.connected.load(Ordering::Relaxed) {
            self.disconnect().await?;
        }
        // A subscription can outlive a failed connect; never carry one over.
        self.release_focus_subscription().await;

        let state = self.init_uia().await?;

        // Resolve the root element on a blocking thread.
        let find_state = state.clone();
        let send_root =
            spawn_blocking_tracked(move || uia3::find_root(&find_state, target)).await??;

        // Rebuild the Arc<UiaState> with the root element set, keeping the
        // existing handle table.
        let new_state = UiaState::with_root(&state, send_root.0);

        self.state = Some(new_state);
        self.connected.store(true, Ordering::Relaxed);
        debug!(backend = self.backend.name(), "UIA adapter connected");
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.release_focus_subscription().await;
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

        spawn_blocking_tracked(move || state.capture_tree(max_depth, include_hidden)).await?
    }

    /// Idempotent per connection: any handler a previous call registered is
    /// removed first, and the registration is awaited — a failure is returned
    /// as `Err`, never a receiver for a stream that will stay silent.
    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        let state = match self.state.as_ref() {
            Some(s) => s.clone(),
            None => return Ok(None),
        };

        self.release_focus_subscription().await;

        let (tx, rx) = mpsc::channel::<A11yEvent>(256);

        let subscription = spawn_blocking_tracked(move || -> anyhow::Result<FocusSubscription> {
            let handler: IUIAutomationFocusChangedEventHandler = FocusChangedHandler { tx }.into();
            // SAFETY: COM call on an MTA-initialized `IUIAutomation`.
            unsafe { state.automation.AddFocusChangedEventHandler(None, &handler) }
                .map_err(|e| anyhow!("Failed to register UIA focus event handler: {}", e))?;
            Ok(FocusSubscription::new(state.automation.clone(), handler))
        })
        .await??;

        // Should a concurrent subscribe have filled the slot meanwhile, keep
        // the newest registration and remove the one it displaced.
        let displaced = self
            .focus_subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(subscription);
        if let Some(old) = displaced {
            let _ = spawn_blocking_tracked(move || old.remove()).await;
        }

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

        spawn_blocking_tracked(move || {
            let element = state
                .handles
                .get(platform_handle)
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

        spawn_blocking_tracked(move || match state.handles.get(platform_handle) {
            Some(element) => detect_patterns(&element),
            None => vec![],
        })
        .await
        .unwrap_or_default()
    }
}

impl Drop for UiaAdapter {
    /// Backstop for an adapter dropped without `disconnect` (e.g. when the
    /// manager swaps in the JAB adapter): dropping the subscription removes
    /// the handler on a plain OS thread (see `FocusSubscription`'s `Drop`),
    /// since `drop` cannot await and must not block the runtime.
    fn drop(&mut self) {
        drop(self.take_focus_subscription());
    }
}
