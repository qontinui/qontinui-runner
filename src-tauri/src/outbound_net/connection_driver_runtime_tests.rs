//! Which runtime drives a pooled HTTP connection? (plan
//! `2026-10-19-runner-ui-wedge-signal-fires-when-the-app-runtime-is-starved-by-process-spawns`,
//! Phase 0 item 3, decision D4.)
//!
//! hyper-util 0.1.20's `TokioExecutor::execute` is `tokio::spawn(fut)`
//! (`src/rt/tokio.rs`), called for each new connection's driver task. So a
//! pooled connection is driven by whichever runtime OPENED it, and one
//! `reqwest::Client` shared across runtimes (`coord_http::coord_client`) can
//! have its requests from a healthy runtime stall behind a starved one. This
//! file tests that mechanism directly rather than inferring it from logs:
//!
//! * open a keep-alive connection from runtime A, park every A worker, and
//!   send from runtime B on the same client — the hypothesis says it times out;
//! * control (i): the same request from B BEFORE A is parked succeeds;
//! * control (ii): a client used only from B succeeds while A is parked.
//!
//! The `deadpool-postgres` twin is static evidence, not a test: 0.14.1's
//! connector `tokio::spawn`s the connection task (`src/lib.rs:223`) on the
//! runtime running `Manager::create`, the same shape.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Barrier};
use std::time::Duration;

/// A loopback HTTP/1.1 server on plain threads (no tokio, so no runtime under
/// test can starve it). Answers every request on a connection with `ok` and
/// keeps the connection alive.
fn keep_alive_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::Builder::new()
        .name("test-http-server".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        let Ok(n) = stream.read(&mut chunk) else { return };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            buf.drain(..end + 4);
                            let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
                            if stream.write_all(resp).is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        })
        .expect("server thread");
    format!("http://{addr}/")
}

fn runtime(name: &str) -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name(name)
        .enable_all()
        .build()
        .expect("test runtime")
}

/// Park every worker of a 2-worker runtime; returns once both are parked.
fn park_both_workers(rt: &tokio::runtime::Runtime, for_: Duration) {
    let barrier = Arc::new(Barrier::new(3));
    for _ in 0..2 {
        let b = barrier.clone();
        rt.spawn(async move {
            b.wait();
            std::thread::sleep(for_);
        });
    }
    barrier.wait();
}

async fn get(client: &reqwest::Client, url: &str) -> Result<String, String> {
    match tokio::time::timeout(Duration::from_secs(2), async {
        client.get(url).send().await?.text().await
    })
    .await
    {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(e)) => Err(format!("request error: {e}")),
        Err(_) => Err("timed out after 2s".into()),
    }
}

#[test]
fn a_pooled_connection_is_driven_by_the_runtime_that_opened_it() {
    let url = keep_alive_server();
    let rt_a = runtime("conn-test-a");
    let rt_b = runtime("conn-test-b");
    let shared = super::os_dns::client_builder()
        .pool_max_idle_per_host(1)
        .build()
        .expect("client");

    // Open the connection from A, read the body so it returns to the pool.
    assert_eq!(rt_a.block_on(get(&shared, &url)), Ok("ok".into()));
    // Control (i): from B on the pooled connection, A healthy.
    assert_eq!(
        rt_b.block_on(get(&shared, &url)),
        Ok("ok".into()),
        "control (i): a request from B on A's pooled connection succeeds while A is healthy"
    );

    park_both_workers(&rt_a, Duration::from_secs(6));

    // Control (ii): a client used only from B is unaffected by A.
    let b_only = super::os_dns::client_builder().build().expect("client");
    assert_eq!(
        rt_b.block_on(get(&b_only, &url)),
        Ok("ok".into()),
        "control (ii): a B-only client succeeds while A is parked"
    );

    // The hypothesis: the shared client's pooled connection is driven by A.
    let shared_from_b = rt_b.block_on(get(&shared, &url));
    assert_eq!(
        shared_from_b,
        Err("timed out after 2s".into()),
        "a request from healthy runtime B on a connection opened by parked runtime A \
         must stall — if this now succeeds, D4's premise (the connection driver lives \
         on the opener's runtime) has changed and the net-rt routing can be revisited"
    );

    rt_a.shutdown_background();
    rt_b.shutdown_background();
}
