//! Linux AT-SPI2 accessibility adapter.
//!
//! Uses the `atspi` crate (from odilia-app) for pure-Rust AT-SPI2 protocol
//! implementation via D-Bus (zbus). Connects to the accessibility bus, captures
//! element trees, and dispatches native interactions through AT-SPI interfaces.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use async_trait::async_trait;
use atspi::proxy::accessible::AccessibleProxy;
use atspi::proxy::action::ActionProxy;
use atspi::proxy::component::ComponentProxy;
use atspi::proxy::editable_text::EditableTextProxy;
use atspi::proxy::value::ValueProxy;
use atspi::{AccessibilityConnection, CoordType, Interface, InterfaceSet, Role, State};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, trace, warn};
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

        // Connect to the AT-SPI accessibility bus with a timeout.
        let conn = tokio::time::timeout(timeout, AccessibilityConnection::new())
            .await
            .context("Timed out connecting to AT-SPI bus")?
            .context("Failed to connect to AT-SPI accessibility bus")?;

        self.connection = Some(conn);

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
        self.connected.store(false, Ordering::Release);
        self.root_address = None;
        self.handle_table.write().await.clear();
        self.connection = None;
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

    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        let conn = self
            .connection
            .as_ref()
            .context("Not connected to AT-SPI bus")?;

        let (tx, rx) = mpsc::channel::<A11yEvent>(256);
        let zconn = conn.connection().clone();

        // Spawn a task that listens to D-Bus signals for focus changes and
        // structural mutations. We use a raw match rule for AT-SPI event signals.
        tokio::spawn(async move {
            use futures::StreamExt;

            // Subscribe to the AT-SPI event signals on D-Bus.
            // The primary signal interface is org.a11y.atspi.Event.Focus
            // and org.a11y.atspi.Event.Object for state/structure changes.
            let proxy = match zbus::fdo::DBusProxy::builder(&zconn)
                .destination("org.freedesktop.DBus")
                .expect("valid destination")
                .path("/org/freedesktop/DBus")
                .expect("valid path")
                .build()
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    error!("Failed to build DBus proxy for event subscription: {e}");
                    return;
                }
            };

            // Add match rules for AT-SPI events.
            let focus_rule = "type='signal',interface='org.a11y.atspi.Event.Focus'";
            let object_rule =
                "type='signal',interface='org.a11y.atspi.Event.Object',member='StateChanged'";
            let children_rule =
                "type='signal',interface='org.a11y.atspi.Event.Object',member='ChildrenChanged'";

            for rule in &[focus_rule, object_rule, children_rule] {
                match zbus::MatchRule::try_from(*rule) {
                    Ok(match_rule) => {
                        if let Err(e) = proxy.add_match_rule(match_rule).await {
                            warn!("Failed to add match rule {rule}: {e}");
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse match rule {rule}: {e}");
                    }
                }
            }

            // Listen to all messages matching our rules via the message stream.
            let mut stream = zbus::MessageStream::from(&zconn);

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

                let event = match (interface.as_deref(), member.as_deref()) {
                    (Some("org.a11y.atspi.Event.Focus"), _) => {
                        // Focus event — extract sender path as ref placeholder.
                        let path = header
                            .path()
                            .map(|p| p.as_str().to_string())
                            .unwrap_or_default();
                        Some(A11yEvent::FocusChanged {
                            ref_id: path,
                            node_name: None,
                        })
                    }
                    (Some("org.a11y.atspi.Event.Object"), Some("StateChanged")) => {
                        let path = header
                            .path()
                            .map(|p| p.as_str().to_string())
                            .unwrap_or_default();
                        Some(A11yEvent::PropertyChanged {
                            ref_id: path,
                            property: "state".to_string(),
                            old_value: None,
                            new_value: None,
                        })
                    }
                    (Some("org.a11y.atspi.Event.Object"), Some("ChildrenChanged")) => {
                        let path = header
                            .path()
                            .map(|p| p.as_str().to_string())
                            .unwrap_or_default();
                        Some(A11yEvent::StructureChanged {
                            parent_ref: path,
                            change_type: StructureChangeType::Subtree,
                        })
                    }
                    _ => None,
                };

                if let Some(evt) = event {
                    if tx.send(evt).await.is_err() {
                        debug!("AT-SPI event receiver dropped, stopping event loop");
                        break;
                    }
                }
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
