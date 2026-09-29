//! Tests for `AccessibilityManager` forwarding the native adapter's own event
//! stream (`PlatformAdapter::subscribe_events`) into `subscribe()`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use super::events::{A11yEvent, EventReceiver};
use super::model::{InteractionPattern, UnifiedNode};
use super::traits::{ConnectionTarget, InteractionParams, InteractionResult, PlatformAdapter};
use super::AccessibilityManager;

/// Adapter whose event stream is whatever receivers the test queued; each
/// `subscribe_events` call hands out the next one, or `None` when none is left.
struct FakeAdapter {
    connected: bool,
    streams: Mutex<Vec<mpsc::Receiver<A11yEvent>>>,
    /// While set, `connect` fails (the test keeps a clone to flip it).
    fail_connect: Arc<AtomicBool>,
}

impl FakeAdapter {
    /// An adapter plus the senders feeding its successive event streams.
    fn with_streams(count: usize) -> (Box<dyn PlatformAdapter>, Vec<mpsc::Sender<A11yEvent>>) {
        let (adapter, senders, _) = Self::with_failing_connect(count);
        (adapter, senders)
    }

    /// Like `with_streams`, plus a switch that makes `connect` fail.
    fn with_failing_connect(
        count: usize,
    ) -> (
        Box<dyn PlatformAdapter>,
        Vec<mpsc::Sender<A11yEvent>>,
        Arc<AtomicBool>,
    ) {
        let mut senders = Vec::new();
        let mut receivers = Vec::new();
        for _ in 0..count {
            let (tx, rx) = mpsc::channel(16);
            senders.push(tx);
            receivers.push(rx);
        }
        // Handed out by `pop`, so reverse to give them out in creation order.
        receivers.reverse();
        let fail = Arc::new(AtomicBool::new(false));
        let adapter = FakeAdapter {
            connected: false,
            streams: Mutex::new(receivers),
            fail_connect: fail.clone(),
        };
        (Box::new(adapter), senders, fail)
    }
}

#[async_trait::async_trait]
impl PlatformAdapter for FakeAdapter {
    fn backend_name(&self) -> &'static str {
        "fake"
    }

    async fn connect(&mut self, _target: ConnectionTarget, _timeout_ms: u64) -> anyhow::Result<()> {
        if self.fail_connect.load(Ordering::SeqCst) {
            self.connected = false;
            anyhow::bail!("fake connect failure");
        }
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn capture_tree(
        &self,
        _max_depth: Option<u32>,
        _include_hidden: bool,
    ) -> anyhow::Result<UnifiedNode> {
        anyhow::bail!("fake adapter has no tree")
    }

    async fn subscribe_events(&self) -> anyhow::Result<Option<mpsc::Receiver<A11yEvent>>> {
        Ok(self.streams.lock().unwrap().pop())
    }

    async fn interact(
        &self,
        _platform_handle: u64,
        pattern: InteractionPattern,
        _params: InteractionParams,
    ) -> anyhow::Result<InteractionResult> {
        Ok(InteractionResult::err(pattern, "fake adapter"))
    }

    async fn supported_patterns(&self, _platform_handle: u64) -> Vec<InteractionPattern> {
        vec![]
    }
}

fn focus(name: &str) -> A11yEvent {
    A11yEvent::FocusChanged {
        ref_id: String::new(),
        node_name: Some(name.to_string()),
    }
}

/// Next `FocusChanged` name on `rx`, skipping manager-originated events;
/// `None` if none arrives within `wait`.
async fn next_focus(rx: &mut EventReceiver, wait: Duration) -> Option<String> {
    tokio::time::timeout(wait, async {
        loop {
            match rx.recv().await {
                Ok(A11yEvent::FocusChanged { node_name, .. }) => return node_name,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

const WAIT: Duration = Duration::from_secs(2);
const QUIET: Duration = Duration::from_millis(150);

#[tokio::test]
async fn adapter_events_reach_subscribers() {
    let (adapter, senders) = FakeAdapter::with_streams(1);
    let mut mgr = AccessibilityManager::with_adapter(adapter);
    let mut rx = mgr.subscribe();

    assert!(!mgr.has_native_events(), "no stream before connecting");
    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();
    assert!(mgr.has_native_events());

    senders[0].send(focus("OK button")).await.unwrap();
    assert_eq!(
        next_focus(&mut rx, WAIT).await.as_deref(),
        Some("OK button")
    );
}

#[tokio::test]
async fn adapter_without_stream_reports_no_native_events() {
    let (adapter, _senders) = FakeAdapter::with_streams(0);
    let mut mgr = AccessibilityManager::with_adapter(adapter);

    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();
    assert!(mgr.is_connected());
    assert!(!mgr.has_native_events());
}

#[tokio::test]
async fn ended_stream_reports_no_native_events() {
    let (adapter, mut senders) = FakeAdapter::with_streams(1);
    let mut mgr = AccessibilityManager::with_adapter(adapter);
    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();

    drop(senders.pop());
    tokio::time::timeout(WAIT, async {
        while mgr.has_native_events() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("forwarder should finish once the adapter's stream ends");
}

#[tokio::test]
async fn disconnect_stops_forwarding() {
    let (adapter, senders) = FakeAdapter::with_streams(1);
    let mut mgr = AccessibilityManager::with_adapter(adapter);
    let mut rx = mgr.subscribe();
    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();

    mgr.disconnect().await.unwrap();
    assert!(!mgr.has_native_events());

    // The aborted forwarder drops the adapter's receiver.
    tokio::time::timeout(WAIT, senders[0].closed())
        .await
        .expect("forwarder should release the adapter stream on disconnect");
    assert!(senders[0].send(focus("late")).await.is_err());
    assert_eq!(next_focus(&mut rx, QUIET).await, None);
}

#[tokio::test]
async fn reconnect_replaces_the_forwarder() {
    let (adapter, senders) = FakeAdapter::with_streams(2);
    let mut mgr = AccessibilityManager::with_adapter(adapter);
    let mut rx = mgr.subscribe();

    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();
    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();
    assert!(mgr.has_native_events());

    tokio::time::timeout(WAIT, senders[0].closed())
        .await
        .expect("the first forwarder should be aborted on reconnect");
    senders[1].send(focus("second")).await.unwrap();
    assert_eq!(next_focus(&mut rx, WAIT).await.as_deref(), Some("second"));
    // Exactly one forwarder: the event is not delivered twice.
    assert_eq!(next_focus(&mut rx, QUIET).await, None);
}

#[tokio::test]
async fn failed_reconnect_leaves_no_stale_forwarder() {
    let (adapter, senders, fail) = FakeAdapter::with_failing_connect(1);
    let mut mgr = AccessibilityManager::with_adapter(adapter);
    let mut rx = mgr.subscribe();

    mgr.connect(ConnectionTarget::Desktop, 1000).await.unwrap();
    assert!(mgr.has_native_events());

    fail.store(true, Ordering::SeqCst);
    assert!(mgr.connect(ConnectionTarget::Desktop, 1000).await.is_err());
    assert!(
        !mgr.has_native_events(),
        "a failed reconnect must not leave the old forwarder reporting a live stream"
    );
    tokio::time::timeout(WAIT, senders[0].closed())
        .await
        .expect("the old forwarder should be aborted before reconnecting");
    assert_eq!(next_focus(&mut rx, QUIET).await, None);
}
