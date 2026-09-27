//! Linux AT-SPI2 accessibility adapter.
//!
//! Uses the `atspi` crate (from odilia-app) for pure-Rust AT-SPI2 protocol
//! implementation via D-Bus (zbus). Connects to the accessibility bus, captures
//! element trees, and dispatches native interactions through AT-SPI interfaces.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context};
use async_trait::async_trait;
use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::action::ActionProxy;
use atspi::proxy::component::ComponentProxy;
use atspi::proxy::editable_text::EditableTextProxy;
use atspi::proxy::value::ValueProxy;
use atspi::{AccessibilityConnection, CoordType, Interface, InterfaceSet, Role, State};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, trace, warn};
use zbus::proxy::CacheProperties;

use crate::accessibility::events::{A11yEvent, StructureChangeType};
use crate::accessibility::model::{
    InteractionPattern, NodeSource, TriBool, UnifiedBounds, UnifiedNode, UnifiedRole, UnifiedState,
};
use crate::accessibility::traits::{
    ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter,
};

/// AT-SPI D-Bus address for a remote accessible element.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct AtspiAddress {
    bus_name: String,
    object_path: String,
}

/// Linux AT-SPI2 adapter.
pub struct AtspiAdapter {
    connection: Option<AccessibilityConnection>,
    /// Root accessible of the target application (or the desktop root).
    root_address: Option<AtspiAddress>,
    /// Maps u64 handles to (bus_name, object_path) for proxy reconstruction.
    handle_table: Arc<RwLock<HashMap<u64, AtspiAddress>>>,
    /// Monotonic handle counter.
    next_handle: AtomicU64,
    /// Monotonic ref counter for node ref IDs.
    next_ref: AtomicU64,
    connected: AtomicBool,
    /// The D-Bus event listener spawned by `subscribe_events`. It owns the
    /// match-rule streams, so aborting it drops them and zbus deregisters the
    /// rules. At most one exists: stopped before every re-subscribe, on
    /// `connect`/`disconnect`, and on drop. A `std` mutex because
    /// `subscribe_events` takes `&self`; never held across an `.await`.
    event_listener: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Whether this connection has asked the AT-SPI registry to have
    /// application bridges emit the event classes the listener needs (see
    /// `register_registry_events`). Reset whenever the connection is replaced:
    /// the registry drops a client's registrations when its bus name vanishes.
    registry_events_registered: AtomicBool,
}

impl Drop for AtspiAdapter {
    fn drop(&mut self) {
        self.stop_event_listener();
    }
}

impl Default for AtspiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AtspiAdapter {
    pub fn new() -> Self {
        Self {
            connection: None,
            root_address: None,
            handle_table: Arc::new(RwLock::new(HashMap::new())),
            next_handle: AtomicU64::new(1),
            next_ref: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            event_listener: Mutex::new(None),
            registry_events_registered: AtomicBool::new(false),
        }
    }

    /// Abort the event listener task, if any. Dropping its match-rule streams
    /// queues their removal from the bus.
    fn stop_event_listener(&self) {
        self.replace_event_listener(None);
    }

    /// Put `next` in the listener slot and abort whatever it displaces, so at
    /// most one listener (and one set of match-rule streams) exists. The one
    /// place the slot is written — `stop_event_listener` and
    /// `subscribe_events` both go through it.
    fn replace_event_listener(&self, next: Option<tokio::task::JoinHandle<()>>) {
        let displaced = std::mem::replace(
            &mut *self
                .event_listener
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            next,
        );
        if let Some(old) = displaced {
            old.abort();
        }
    }

    /// Ask the AT-SPI registry to have application bridges emit focus and
    /// object events, once per connection.
    ///
    /// A match rule only filters what reaches this client; whether a GTK / Qt
    /// bridge emits an event at all depends on some client having registered
    /// for it with the registry (`org.a11y.atspi.Registry.RegisterEvent`).
    /// With no screen reader running, nothing else has, and the listener would
    /// hear nothing. Only the registry half of the crate's `register_event` is
    /// used: its match-rule half adds a bus-wide rule that is never removed,
    /// while the listener's own per-stream rules are removed with the streams.
    /// A refusal is logged, not returned: the streams still receive whatever
    /// the bridges emit for other clients.
    async fn register_registry_events(&self, conn: &AccessibilityConnection) {
        use atspi::events::focus::FocusEvents;
        use atspi::events::object::ObjectEvents;

        if self.registry_events_registered.load(Ordering::Acquire) {
            return;
        }
        let focus = conn.add_registry_event::<FocusEvents>().await;
        let object = conn.add_registry_event::<ObjectEvents>().await;
        match (focus, object) {
            (Ok(()), Ok(())) => {
                self.registry_events_registered
                    .store(true, Ordering::Release);
            }
            (focus, object) => {
                for e in [focus.err(), object.err()].into_iter().flatten() {
                    warn!("AT-SPI registry event registration failed: {e}");
                }
            }
        }
    }

    /// Allocate a new platform handle and store the address mapping.
    async fn register_handle(&self, addr: AtspiAddress) -> u64 {
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handle_table.write().await.insert(handle, addr);
        handle
    }

    /// Allocate a new ref ID like "@e1", "@e2", etc.
    fn alloc_ref(&self) -> String {
        let id = self.next_ref.fetch_add(1, Ordering::Relaxed);
        format!("@e{id}")
    }

    /// Look up a stored address by handle.
    async fn lookup_handle(&self, handle: u64) -> anyhow::Result<AtspiAddress> {
        self.handle_table
            .read()
            .await
            .get(&handle)
            .cloned()
            .context(format!("No AT-SPI element registered for handle {handle}"))
    }

    /// Get the zbus connection from the accessibility connection.
    fn zbus_conn(&self) -> anyhow::Result<&zbus::Connection> {
        self.connection
            .as_ref()
            .map(|c| c.connection())
            .context("Not connected to AT-SPI bus")
    }

    /// Build an AccessibleProxy for a given address. Uses `CacheProperties::No`
    /// to avoid stale D-Bus property caches for dynamic UI elements.
    async fn accessible_proxy<'a>(
        &self,
        addr: &'a AtspiAddress,
    ) -> anyhow::Result<AccessibleProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = AccessibleProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build AccessibleProxy")?;
        Ok(proxy)
    }

    /// Build an ActionProxy for the given address.
    async fn action_proxy<'a>(&self, addr: &'a AtspiAddress) -> anyhow::Result<ActionProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = ActionProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build ActionProxy")?;
        Ok(proxy)
    }

    /// Build a ComponentProxy for the given address.
    async fn component_proxy<'a>(
        &self,
        addr: &'a AtspiAddress,
    ) -> anyhow::Result<ComponentProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = ComponentProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build ComponentProxy")?;
        Ok(proxy)
    }

    /// Build an EditableTextProxy for the given address.
    async fn editable_text_proxy<'a>(
        &self,
        addr: &'a AtspiAddress,
    ) -> anyhow::Result<EditableTextProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = EditableTextProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build EditableTextProxy")?;
        Ok(proxy)
    }

    /// Build a ValueProxy for the given address.
    async fn value_proxy<'a>(&self, addr: &'a AtspiAddress) -> anyhow::Result<ValueProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = ValueProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build ValueProxy")?;
        Ok(proxy)
    }

    /// Build an ApplicationProxy for the given address (used for PID lookup).
    async fn application_proxy<'a>(
        &self,
        addr: &'a AtspiAddress,
    ) -> anyhow::Result<atspi::proxy::application::ApplicationProxy<'a>> {
        let conn = self.zbus_conn()?;
        let proxy = atspi::proxy::application::ApplicationProxy::builder(conn)
            .destination(addr.bus_name.as_str())?
            .path(addr.object_path.as_str())?
            .cache_properties(CacheProperties::No)
            .build()
            .await
            .context("Failed to build ApplicationProxy")?;
        Ok(proxy)
    }

    /// Recursively capture the accessibility tree starting from an address.
    ///
    /// Returns a `BoxFuture` rather than `async fn` because the function calls
    /// itself recursively for child nodes, and Rust requires explicit boxing
    /// to give the resulting future a known size (E0733).
    fn capture_node<'a>(
        &'a self,
        addr: &'a AtspiAddress,
        current_depth: u32,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> futures::future::BoxFuture<'a, anyhow::Result<Option<UnifiedNode>>> {
        Box::pin(async move {
            // Depth limit check.
            if let Some(max) = max_depth {
                if current_depth > max {
                    return Ok(None);
                }
            }

            let proxy = match self.accessible_proxy(addr).await {
                Ok(p) => p,
                Err(e) => {
                    trace!("Skipping inaccessible node {}: {e}", addr.object_path);
                    return Ok(None);
                }
            };

            // Fetch properties concurrently where possible.
            let name = proxy.name().await.unwrap_or_default();
            let description = proxy.description().await.ok().filter(|d| !d.is_empty());
            let role = proxy.get_role().await.unwrap_or(Role::Invalid);
            let state_set = proxy.get_state().await.unwrap_or_default();
            let interfaces = proxy
                .get_interfaces()
                .await
                .unwrap_or_else(|_| InterfaceSet::empty());

            // Convert AT-SPI states.
            let is_hidden = state_set.contains(State::Defunct)
                || (!state_set.contains(State::Showing) && !state_set.contains(State::Visible));

            // Skip hidden nodes unless requested.
            if is_hidden && !include_hidden {
                return Ok(None);
            }

            let unified_role = map_atspi_role(role);
            let state = convert_atspi_state(&state_set);

            // Fetch bounds via Component interface (if available).
            let bounds = if interfaces.contains(Interface::Component) {
                match self.component_proxy(addr).await {
                    Ok(comp) => match comp.get_extents(CoordType::Screen).await {
                        Ok((x, y, w, h)) => Some(UnifiedBounds {
                            x,
                            y,
                            width: w,
                            height: h,
                        }),
                        Err(_) => None,
                    },
                    Err(_) => None,
                }
            } else {
                None
            };

            // Determine supported interaction patterns from interfaces.
            let supported_patterns = determine_patterns(&interfaces);
            let is_interactive =
                unified_role.is_interactive_role() || !supported_patterns.is_empty();

            // Register handle for this element.
            let handle = self.register_handle(addr.clone()).await;

            // Recurse into children.
            let children_addrs = match proxy.get_children().await {
                Ok(children) => children,
                Err(e) => {
                    trace!("Could not get children for {}: {e}", addr.object_path);
                    vec![]
                }
            };

            let mut children_nodes = Vec::new();
            for child_obj_ref in &children_addrs {
                let child_addr = AtspiAddress {
                    bus_name: child_obj_ref.name.to_string(),
                    object_path: child_obj_ref.path.to_string(),
                };
                match self
                    .capture_node(&child_addr, current_depth + 1, max_depth, include_hidden)
                    .await
                {
                    Ok(Some(node)) => children_nodes.push(node),
                    Ok(None) => {} // hidden or depth-limited
                    Err(e) => {
                        trace!("Error capturing child {}: {e}", child_addr.object_path);
                    }
                }
            }

            let node = UnifiedNode {
                ref_id: self.alloc_ref(),
                role: unified_role,
                name: if name.is_empty() { None } else { Some(name) },
                value: None,
                description,
                bounds,
                state,
                is_interactive,
                level: None,
                automation_id: None,
                class_name: None,
                html_tag: None,
                url: None,
                source: NodeSource::Atspi,
                platform_handle: Some(handle),

                supported_patterns,
                generation: 0,
                children: children_nodes,
            };

            Ok(Some(node))
        })
    }

    /// Find a child application on the desktop by window title (partial, case-insensitive).
    async fn find_by_title(
        &self,
        desktop_proxy: &AccessibleProxy<'_>,
        title: &str,
    ) -> anyhow::Result<AtspiAddress> {
        let title_lower = title.to_lowercase();
        let children = desktop_proxy
            .get_children()
            .await
            .context("Failed to enumerate desktop children")?;

        for child_ref in &children {
            let child_addr = AtspiAddress {
                bus_name: child_ref.name.to_string(),
                object_path: child_ref.path.to_string(),
            };
            if let Ok(proxy) = self.accessible_proxy(&child_addr).await {
                // Check the application itself.
                if let Ok(name) = proxy.name().await {
                    if name.to_lowercase().contains(&title_lower) {
                        return Ok(child_addr);
                    }
                }
                // Also check the application's child windows.
                if let Ok(app_children) = proxy.get_children().await {
                    for win_ref in &app_children {
                        let win_addr = AtspiAddress {
                            bus_name: win_ref.name.to_string(),
                            object_path: win_ref.path.to_string(),
                        };
                        if let Ok(win_proxy) = self.accessible_proxy(&win_addr).await {
                            if let Ok(win_name) = win_proxy.name().await {
                                if win_name.to_lowercase().contains(&title_lower) {
                                    return Ok(child_addr);
                                }
                            }
                        }
                    }
                }
            }
        }

        bail!("No application found with title containing \"{title}\"")
    }

    /// Find a child application on the desktop by process ID.
    async fn find_by_pid(
        &self,
        desktop_proxy: &AccessibleProxy<'_>,
        pid: u32,
    ) -> anyhow::Result<AtspiAddress> {
        let children = desktop_proxy
            .get_children()
            .await
            .context("Failed to enumerate desktop children")?;

        for child_ref in &children {
            let child_addr = AtspiAddress {
                bus_name: child_ref.name.to_string(),
                object_path: child_ref.path.to_string(),
            };
            if let Ok(_proxy) = self.accessible_proxy(&child_addr).await {
                // AT-SPI applications expose get_id() or we can check the Application interface.
                // The accessible application proxy has a method to get the PID.
                if let Ok(app) = self.application_proxy(&child_addr).await {
                    // The `id` property on Application interface is the PID.
                    if let Ok(app_id) = app.id().await {
                        if app_id as u32 == pid {
                            return Ok(child_addr);
                        }
                    }
                }
            }
        }

        bail!("No application found with PID {pid}")
    }
}

#[async_trait]
impl PlatformAdapter for AtspiAdapter {
    fn backend_name(&self) -> &'static str {
        "atspi"
    }

    async fn connect(&mut self, target: ConnectionTarget, timeout_ms: u64) -> anyhow::Result<()> {
        let timeout = std::time::Duration::from_millis(timeout_ms);

        // A listener from a previous connection must not outlive it.
        self.stop_event_listener();

        // Connect to the AT-SPI accessibility bus with a timeout.
        let conn = tokio::time::timeout(timeout, AccessibilityConnection::new())
            .await
            .context("Timed out connecting to AT-SPI bus")?
            .context("Failed to connect to AT-SPI accessibility bus")?;

        self.connection = Some(conn);
        self.registry_events_registered
            .store(false, Ordering::Release);

        // Get the desktop root (registry).
        let registry_addr = AtspiAddress {
            bus_name: "org.a11y.atspi.Registry".to_string(),
            object_path: "/org/a11y/atspi/accessible/root".to_string(),
        };

        let desktop_proxy = self
            .accessible_proxy(&registry_addr)
            .await
            .context("Failed to access AT-SPI registry root")?;

        let root_addr = match target {
            ConnectionTarget::Desktop => registry_addr,
            ConnectionTarget::WindowTitle(ref title) => {
                self.find_by_title(&desktop_proxy, title).await?
            }
            ConnectionTarget::ProcessId(pid) => self.find_by_pid(&desktop_proxy, pid).await?,
        };

        debug!(
            "AT-SPI connected to {} ({})",
            root_addr.bus_name, root_addr.object_path
        );

        self.root_address = Some(root_addr);
        self.connected.store(true, Ordering::Release);

        // Reset handle and ref counters for a fresh session.
        self.handle_table.write().await.clear();
        self.next_handle.store(1, Ordering::Relaxed);
        self.next_ref.store(1, Ordering::Relaxed);

        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop_event_listener();
        self.connected.store(false, Ordering::Release);
        self.root_address = None;
        self.handle_table.write().await.clear();
        self.connection = None;
        self.registry_events_registered
            .store(false, Ordering::Release);
        debug!("AT-SPI adapter disconnected");
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    async fn capture_tree(
        &self,
        max_depth: Option<u32>,
        include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode> {
        let root_addr = self
            .root_address
            .as_ref()
            .context("Not connected — call connect() first")?
            .clone();

        let node = self
            .capture_node(&root_addr, 0, max_depth, include_hidden)
            .await?
            .context("Root node was hidden or inaccessible")?;

        Ok(node)
    }

    /// Idempotent per connection: the previous listener (and its match rules)
    /// is stopped first. The match rules are registered before this returns,
    /// so a bus refusal is an `Err`, not a silently empty stream.
    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        use futures::StreamExt;

        let conn = self
            .connection
            .as_ref()
            .context("Not connected to AT-SPI bus")?;
        let zconn = conn.connection().clone();

        self.stop_event_listener();
        self.register_registry_events(conn).await;

        // One stream per AT-SPI event match rule: focus changes, state changes
        // and structural (children) mutations. `for_match_rule` registers the
        // rule with the bus now, and removes it when the stream is dropped.
        const RULES: [&str; 3] = [
            "type='signal',interface='org.a11y.atspi.Event.Focus'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='StateChanged'",
            "type='signal',interface='org.a11y.atspi.Event.Object',member='ChildrenChanged'",
        ];
        let mut streams = Vec::with_capacity(RULES.len());
        for rule in RULES {
            let stream = zbus::MessageStream::for_match_rule(rule, &zconn, None)
                .await
                .with_context(|| format!("Failed to add AT-SPI match rule {rule}"))?;
            streams.push(stream);
        }
        let mut stream = futures::stream::select_all(streams);

        let (tx, rx) = mpsc::channel::<A11yEvent>(256);

        let listener = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                // zbus 4 yields `Result<Message, Error>` from MessageStream rather
                // than the bare `Arc<Message>` of zbus 3. Drop transient errors and
                // continue listening.
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        warn!("AT-SPI message stream error: {e}");
                        continue;
                    }
                };
                let header = msg.header();
                let interface = header.interface().map(|i| i.as_str().to_string());
                let member = header.member().map(|m| m.as_str().to_string());
                let path = header
                    .path()
                    .map(|p| p.as_str().to_string())
                    .unwrap_or_default();
                // Only a state change's detail decides its mapping; skip
                // decoding every other body.
                let detail = if member.as_deref() == Some("StateChanged") {
                    event_detail(&msg)
                } else {
                    None
                };

                let event = map_atspi_signal(
                    interface.as_deref(),
                    member.as_deref(),
                    path,
                    detail
                        .as_ref()
                        .map(|(kind, detail1)| (kind.as_str(), *detail1)),
                );

                if let Some(evt) = event {
                    if tx.send(evt).await.is_err() {
                        debug!("AT-SPI event receiver dropped, stopping event loop");
                        break;
                    }
                }
            }
        });

        self.replace_event_listener(Some(listener));

        Ok(Some(rx))
    }

    async fn interact(
        &self,
        platform_handle: u64,
        pattern: InteractionPattern,
        params: InteractionParams,
    ) -> anyhow::Result<InteractionResult> {
        let addr = self.lookup_handle(platform_handle).await?;

        match pattern {
            InteractionPattern::Invoke => match self.action_proxy(&addr).await {
                Ok(action) => match action.do_action(0).await {
                    Ok(success) => {
                        if success {
                            Ok(InteractionResult::ok(pattern))
                        } else {
                            Ok(InteractionResult::err(
                                pattern,
                                "AT-SPI action returned false",
                            ))
                        }
                    }
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("AT-SPI do_action failed: {e}"),
                    )),
                },
                Err(e) => Ok(InteractionResult::err(
                    pattern,
                    format!("Failed to create ActionProxy: {e}"),
                )),
            },

            InteractionPattern::Value => {
                let text = match &params {
                    InteractionParams::Text { value, .. } => value.clone(),
                    _ => bail!("Value pattern requires Text params"),
                };

                match self.editable_text_proxy(&addr).await {
                    Ok(et) => match et.set_text_contents(&text).await {
                        Ok(_) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("set_text_contents failed: {e}"),
                        )),
                    },
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create EditableTextProxy: {e}"),
                    )),
                }
            }

            InteractionPattern::RangeValue => {
                let val = match &params {
                    InteractionParams::RangeValue { value } => *value,
                    _ => bail!("RangeValue pattern requires RangeValue params"),
                };

                match self.value_proxy(&addr).await {
                    Ok(vp) => match vp.set_current_value(val).await {
                        Ok(_) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("set_current_value failed: {e}"),
                        )),
                    },
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ValueProxy: {e}"),
                    )),
                }
            }

            InteractionPattern::Toggle => {
                // Toggle is implemented as a click/invoke action in AT-SPI.
                match self.action_proxy(&addr).await {
                    Ok(action) => match action.do_action(0).await {
                        Ok(_) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("Toggle via do_action failed: {e}"),
                        )),
                    },
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ActionProxy for toggle: {e}"),
                    )),
                }
            }

            InteractionPattern::CoordinateClick => {
                // Return the center of the element's bounding box for fallback clicking.
                match self.component_proxy(&addr).await {
                    Ok(comp) => match comp.get_extents(CoordType::Screen).await {
                        Ok((x, y, w, h)) => {
                            let cx = x + w / 2;
                            let cy = y + h / 2;
                            debug!("CoordinateClick target: ({cx}, {cy})");
                            Ok(InteractionResult::ok(pattern))
                        }
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("get_extents failed: {e}"),
                        )),
                    },
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ComponentProxy: {e}"),
                    )),
                }
            }

            InteractionPattern::Selection => {
                // Selection via Action interface (select action, usually index 0 or 1).
                match self.action_proxy(&addr).await {
                    Ok(action) => {
                        // Try to find a "select" action by name.
                        let n_actions = action.nactions().await.unwrap_or(0);
                        let mut select_idx: Option<i32> = None;
                        for i in 0..n_actions {
                            if let Ok(name) = action.get_name(i).await {
                                if name.to_lowercase().contains("select") {
                                    select_idx = Some(i);
                                    break;
                                }
                            }
                        }
                        let idx = select_idx.unwrap_or(0);
                        match action.do_action(idx).await {
                            Ok(_) => Ok(InteractionResult::ok(pattern)),
                            Err(e) => Ok(InteractionResult::err(
                                pattern,
                                format!("Selection do_action({idx}) failed: {e}"),
                            )),
                        }
                    }
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ActionProxy for selection: {e}"),
                    )),
                }
            }

            InteractionPattern::ExpandCollapse => {
                // Expand/collapse via Action interface.
                match self.action_proxy(&addr).await {
                    Ok(action) => match action.do_action(0).await {
                        Ok(_) => Ok(InteractionResult::ok(pattern)),
                        Err(e) => Ok(InteractionResult::err(
                            pattern,
                            format!("ExpandCollapse do_action failed: {e}"),
                        )),
                    },
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ActionProxy for expand/collapse: {e}"),
                    )),
                }
            }

            InteractionPattern::Scroll => {
                // AT-SPI doesn't have a dedicated scroll interface; use Action "scroll".
                match self.action_proxy(&addr).await {
                    Ok(action) => {
                        let n_actions = action.nactions().await.unwrap_or(0);
                        let mut scroll_idx: Option<i32> = None;
                        for i in 0..n_actions {
                            if let Ok(name) = action.get_name(i).await {
                                if name.to_lowercase().contains("scroll") {
                                    scroll_idx = Some(i);
                                    break;
                                }
                            }
                        }
                        match scroll_idx {
                            Some(idx) => match action.do_action(idx).await {
                                Ok(_) => Ok(InteractionResult::ok(pattern)),
                                Err(e) => Ok(InteractionResult::err(
                                    pattern,
                                    format!("Scroll do_action failed: {e}"),
                                )),
                            },
                            None => Ok(InteractionResult::err(
                                pattern,
                                "No scroll action available on this element",
                            )),
                        }
                    }
                    Err(e) => Ok(InteractionResult::err(
                        pattern,
                        format!("Failed to create ActionProxy for scroll: {e}"),
                    )),
                }
            }

            InteractionPattern::Text => Ok(InteractionResult::err(
                pattern,
                format!("{pattern:?} is not supported by the AT-SPI adapter"),
            )),
        }
    }

    async fn supported_patterns(&self, platform_handle: u64) -> Vec<InteractionPattern> {
        let addr = match self.lookup_handle(platform_handle).await {
            Ok(a) => a,
            Err(_) => return vec![],
        };

        let proxy = match self.accessible_proxy(&addr).await {
            Ok(p) => p,
            Err(_) => return vec![],
        };

        let interfaces = match proxy.get_interfaces().await {
            Ok(i) => i,
            Err(_) => return vec![],
        };

        determine_patterns(&interfaces)
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// The `(kind, detail1)` of an AT-SPI event body — e.g. `("focused", 1)` for
/// `object:state-changed:focused` turning on. GTK and most toolkits send the
/// `siiva{sv}` body; Qt sends `siiv(so)`. `None` when neither decodes.
fn event_detail(msg: &zbus::Message) -> Option<(String, i32)> {
    use atspi::events::{EventBodyOwned, EventBodyQT};

    let body = msg.body();
    if let Ok(b) = body.deserialize::<EventBodyOwned>() {
        return Some((b.kind, b.detail1));
    }
    body.deserialize::<EventBodyQT>()
        .ok()
        .map(|b| (b.kind, b.detail1))
}

/// Map one AT-SPI signal to an [`A11yEvent`]. Pure, so the mapping is tested
/// without a bus.
///
/// `path` is the emitting object's D-Bus path (carried as the event's ref:
/// it is not a ref-manager ref, and consumers resolve it themselves).
/// `detail` is the state-change body's `(kind, detail1)`, when decoded.
///
/// Focus arrives two ways. The legacy `Event.Focus:Focus` signal, and — what
/// current GTK and Qt actually emit — `Event.Object:StateChanged` with kind
/// `focused` and `detail1 == 1`. Both become `FocusChanged`. A `focused`
/// state turning OFF (`detail1 == 0`) is the old element losing focus; the
/// gaining element sends its own `focused`/1, so that one stays an ordinary
/// state `PropertyChanged`.
fn map_atspi_signal(
    interface: Option<&str>,
    member: Option<&str>,
    path: String,
    detail: Option<(&str, i32)>,
) -> Option<A11yEvent> {
    match (interface, member) {
        (Some("org.a11y.atspi.Event.Focus"), _) => Some(A11yEvent::FocusChanged {
            ref_id: path,
            node_name: None,
        }),
        (Some("org.a11y.atspi.Event.Object"), Some("StateChanged")) => match detail {
            Some(("focused", 1)) => Some(A11yEvent::FocusChanged {
                ref_id: path,
                node_name: None,
            }),
            _ => Some(A11yEvent::PropertyChanged {
                ref_id: path,
                property: "state".to_string(),
                old_value: None,
                new_value: None,
            }),
        },
        (Some("org.a11y.atspi.Event.Object"), Some("ChildrenChanged")) => {
            Some(A11yEvent::StructureChanged {
                parent_ref: path,
                change_type: StructureChangeType::Subtree,
            })
        }
        _ => None,
    }
}

/// Map AT-SPI `Role` enum to `UnifiedRole`.
fn map_atspi_role(role: Role) -> UnifiedRole {
    match role {
        Role::PushButton => UnifiedRole::Button,
        Role::CheckBox => UnifiedRole::Checkbox,
        Role::ComboBox => UnifiedRole::Combobox,
        Role::Dialog => UnifiedRole::Dialog,
        Role::Text => UnifiedRole::StaticText,
        Role::Entry => UnifiedRole::Textbox,
        Role::Menu => UnifiedRole::Menu,
        Role::MenuBar => UnifiedRole::Menubar,
        Role::MenuItem => UnifiedRole::Menuitem,
        Role::CheckMenuItem => UnifiedRole::Menuitemcheckbox,
        Role::RadioMenuItem => UnifiedRole::Menuitemradio,
        Role::PageTab => UnifiedRole::Tab,
        Role::PageTabList => UnifiedRole::Tablist,
        Role::Tree => UnifiedRole::Tree,
        Role::TreeItem => UnifiedRole::Treeitem,
        Role::TreeTable => UnifiedRole::Treegrid,
        Role::Table => UnifiedRole::Table,
        Role::TableCell => UnifiedRole::Cell,
        Role::TableColumnHeader => UnifiedRole::Columnheader,
        Role::TableRowHeader => UnifiedRole::Rowheader,
        Role::TableRow => UnifiedRole::Row,
        Role::List => UnifiedRole::List,
        Role::ListItem => UnifiedRole::Listitem,
        Role::Slider => UnifiedRole::Slider,
        Role::SpinButton => UnifiedRole::Spinbutton,
        Role::Link => UnifiedRole::Link,
        Role::Heading => UnifiedRole::Heading,
        Role::Panel => UnifiedRole::Pane,
        Role::Frame => UnifiedRole::Window,
        Role::ScrollBar => UnifiedRole::Scrollbar,
        Role::Separator => UnifiedRole::Separator,
        Role::ToolBar => UnifiedRole::Toolbar,
        Role::ToolTip => UnifiedRole::Tooltip,
        Role::ProgressBar => UnifiedRole::Progressbar,
        Role::RadioButton => UnifiedRole::Radio,
        Role::Label => UnifiedRole::StaticText,
        Role::Image => UnifiedRole::Img,
        Role::DocumentFrame => UnifiedRole::Document,
        Role::DocumentWeb => UnifiedRole::Document,
        Role::Application => UnifiedRole::Application,
        Role::StatusBar => UnifiedRole::Status,
        Role::Filler => UnifiedRole::Group,
        Role::Form => UnifiedRole::Form,
        Role::Paragraph => UnifiedRole::Paragraph,
        Role::Alert => UnifiedRole::Alert,
        Role::ListBox => UnifiedRole::Listbox,
        Role::ToggleButton => UnifiedRole::Button,
        Role::ScrollPane => UnifiedRole::Pane,
        Role::Viewport => UnifiedRole::Region,
        Role::PasswordText => UnifiedRole::Textbox,
        // NOTE: atspi 0.22 renamed these variants — `EditBar` → `Editbar`
        // (lowercase b) and `Glass` → `GlassPane`. The original mappings
        // (Edit / Pane) are preserved.
        Role::Editbar => UnifiedRole::Edit,
        Role::GlassPane => UnifiedRole::Pane,
        Role::Extended => UnifiedRole::Custom,
        _ => UnifiedRole::Unknown,
    }
}

/// Convert AT-SPI `StateSet` flags to `UnifiedState`.
fn convert_atspi_state(state_set: &atspi::StateSet) -> UnifiedState {
    UnifiedState {
        is_focused: state_set.contains(State::Focused),
        is_disabled: !state_set.contains(State::Sensitive),
        is_hidden: !state_set.contains(State::Showing),
        is_expanded: if state_set.contains(State::Expandable) {
            TriBool::from_option(Some(state_set.contains(State::Expanded)))
        } else {
            TriBool::NotApplicable
        },
        is_selected: if state_set.contains(State::Selectable) {
            TriBool::from_option(Some(state_set.contains(State::Selected)))
        } else {
            TriBool::NotApplicable
        },
        is_checked: if state_set.contains(State::Checkable) {
            TriBool::from_option(Some(state_set.contains(State::Checked)))
        } else {
            TriBool::NotApplicable
        },
        is_pressed: if state_set.contains(State::Pressed) {
            TriBool::True
        } else {
            TriBool::NotApplicable
        },
        is_readonly: !state_set.contains(State::Editable),
        is_required: state_set.contains(State::Required),
        is_multiselectable: state_set.contains(State::Multiselectable),
        is_editable: state_set.contains(State::Editable),
        is_focusable: state_set.contains(State::Focusable),
        is_modal: state_set.contains(State::Modal),
    }
}

/// Determine which interaction patterns are supported based on AT-SPI interfaces.
///
/// Takes `&InterfaceSet` (atspi's bitflag-based interface bag) rather than a slice.
/// `InterfaceSet::contains` accepts the `Interface` enum by value.
fn determine_patterns(interfaces: &InterfaceSet) -> Vec<InteractionPattern> {
    let mut patterns = Vec::new();

    if interfaces.contains(Interface::Action) {
        patterns.push(InteractionPattern::Invoke);
    }
    if interfaces.contains(Interface::EditableText) {
        patterns.push(InteractionPattern::Value);
    }
    if interfaces.contains(Interface::Value) {
        patterns.push(InteractionPattern::RangeValue);
    }
    if interfaces.contains(Interface::Selection) {
        patterns.push(InteractionPattern::Selection);
    }
    if interfaces.contains(Interface::Component) {
        patterns.push(InteractionPattern::CoordinateClick);
    }

    patterns
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECT: Option<&str> = Some("org.a11y.atspi.Event.Object");
    const PATH: &str = "/org/a11y/atspi/accessible/42";

    fn focus_changed(event: Option<A11yEvent>) -> Option<(String, Option<String>)> {
        match event {
            Some(A11yEvent::FocusChanged { ref_id, node_name }) => Some((ref_id, node_name)),
            _ => None,
        }
    }

    #[test]
    fn legacy_focus_signal_maps_to_focus_changed() {
        let ev = map_atspi_signal(
            Some("org.a11y.atspi.Event.Focus"),
            Some("Focus"),
            PATH.into(),
            None,
        );
        assert_eq!(focus_changed(ev), Some((PATH.to_string(), None)));
    }

    #[test]
    fn state_changed_focused_on_maps_to_focus_changed() {
        let ev = map_atspi_signal(
            OBJECT,
            Some("StateChanged"),
            PATH.into(),
            Some(("focused", 1)),
        );
        assert_eq!(focus_changed(ev), Some((PATH.to_string(), None)));
    }

    #[test]
    fn state_changed_focused_off_stays_a_property_change() {
        let ev = map_atspi_signal(
            OBJECT,
            Some("StateChanged"),
            PATH.into(),
            Some(("focused", 0)),
        );
        assert!(matches!(
            ev,
            Some(A11yEvent::PropertyChanged { ref ref_id, ref property, .. })
                if ref_id == PATH && property == "state"
        ));
    }

    #[test]
    fn other_state_changes_and_undecoded_bodies_stay_property_changes() {
        for detail in [Some(("checked", 1)), Some(("selected", 0)), None] {
            let ev = map_atspi_signal(OBJECT, Some("StateChanged"), PATH.into(), detail);
            assert!(
                matches!(ev, Some(A11yEvent::PropertyChanged { .. })),
                "{detail:?} -> {ev:?}"
            );
        }
    }

    #[test]
    fn children_changed_maps_to_structure_changed() {
        let ev = map_atspi_signal(OBJECT, Some("ChildrenChanged"), PATH.into(), None);
        assert!(matches!(
            ev,
            Some(A11yEvent::StructureChanged { ref parent_ref, .. }) if parent_ref == PATH
        ));
    }

    #[test]
    fn unrelated_signals_are_dropped() {
        assert!(map_atspi_signal(OBJECT, Some("BoundsChanged"), PATH.into(), None).is_none());
        assert!(map_atspi_signal(None, None, PATH.into(), None).is_none());
    }

    /// A stand-in listener: pending forever, holding a sender whose drop
    /// signals that the task was torn down (aborted and dropped).
    fn parked_listener() -> (
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let (alive_tx, alive_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let _alive = alive_tx;
            std::future::pending::<()>().await;
        });
        (handle, alive_rx)
    }

    async fn torn_down(alive: tokio::sync::oneshot::Receiver<()>) -> bool {
        // The sender is only ever dropped, never sent on: Err == torn down.
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), alive).await,
            Ok(Err(_))
        )
    }

    #[tokio::test]
    async fn replacing_the_listener_tears_the_previous_one_down() {
        let adapter = AtspiAdapter::new();
        let (first, first_alive) = parked_listener();
        adapter.replace_event_listener(Some(first));
        let (second, mut second_alive) = parked_listener();
        adapter.replace_event_listener(Some(second));

        assert!(
            torn_down(first_alive).await,
            "displaced listener still running"
        );
        assert!(
            matches!(
                second_alive.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ),
            "the new listener must keep running"
        );
    }

    #[tokio::test]
    async fn disconnect_tears_the_listener_down_without_a_bus() {
        let mut adapter = AtspiAdapter::new();
        let (listener, alive) = parked_listener();
        adapter.replace_event_listener(Some(listener));

        adapter.disconnect().await.unwrap();

        assert!(torn_down(alive).await, "listener outlived disconnect");
        assert!(adapter.event_listener.lock().unwrap().is_none());
    }
}
