//! Tests for the per-local-user connection guard.
//!
//! Coverage enumerated (testing policy `coverage-is-enumerated-not-salient`):
//! the decision on every arm (same user, other user, each unresolved reason,
//! unknown own user); the kill-switch parser on unset/empty/`0`/` 0 `/`1`/
//! `off`; the `/proc/net/tcp{,6}` parser on header, IPv4, IPv4-mapped IPv6,
//! malformed lines; owner matching on the peer row vs the server row,
//! TIME_WAIT and inode-0 rows, a missing row; the Windows row decoders and the
//! FILETIME conversion; and the listener end to end over real TCP with an
//! injected resolver (admit, other user, unresolved, kill switch off, two
//! connections). On Linux the real `/proc` resolver is exercised against a
//! live connection, and an `#[ignore]`d test covers a genuine second uid.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::decode::*;
use super::*;

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

// --- decide ----------------------------------------------------------------

#[test]
fn same_user_is_admitted() {
    let own = LocalUser::Uid(1000);
    let r = Resolution {
        owner: Ok(LocalUser::Uid(1000)),
        pid: None,
    };
    assert_eq!(decide(Some(&own), &r), Verdict::Admit);
}

#[test]
fn other_user_is_refused_and_named() {
    let own = LocalUser::Uid(1000);
    let r = Resolution {
        owner: Ok(LocalUser::Uid(1001)),
        pid: None,
    };
    assert_eq!(
        decide(Some(&own), &r),
        Verdict::RefuseOtherUser(LocalUser::Uid(1001))
    );
}

#[test]
fn sids_compare_by_string() {
    let own = LocalUser::Sid("S-1-5-21-1-2-3-1001".into());
    let same = Resolution {
        owner: Ok(LocalUser::Sid("S-1-5-21-1-2-3-1001".into())),
        pid: Some(4242),
    };
    let other = Resolution {
        owner: Ok(LocalUser::Sid("S-1-5-21-1-2-3-1002".into())),
        pid: Some(4243),
    };
    assert_eq!(decide(Some(&own), &same), Verdict::Admit);
    assert!(matches!(
        decide(Some(&own), &other),
        Verdict::RefuseOtherUser(_)
    ));
}

#[test]
fn every_unresolved_reason_is_refused() {
    let own = LocalUser::Uid(1000);
    for why in [
        Unresolved::NoMatchingSocket,
        Unresolved::SocketClosing,
        Unresolved::ProcessUnopenable,
        Unresolved::TokenUnreadable,
        Unresolved::TokenAccessDenied,
        Unresolved::PidRecycled,
        Unresolved::TableUnreadable,
        Unresolved::OwnUserUnknown,
    ] {
        let r = Resolution {
            owner: Err(why),
            pid: None,
        };
        assert_eq!(
            decide(Some(&own), &r),
            Verdict::RefuseUnresolved(why),
            "{why:?}"
        );
    }
}

#[test]
fn unknown_own_user_refuses_even_a_resolved_peer() {
    let r = Resolution {
        owner: Ok(LocalUser::Uid(1000)),
        pid: None,
    };
    assert_eq!(
        decide(None, &r),
        Verdict::RefuseUnresolved(Unresolved::OwnUserUnknown)
    );
}

#[test]
fn mode_parser_covers_every_spelling() {
    let d = platform_default_mode();
    assert_eq!(mode_from_env_value(None), (d, true));
    assert_eq!(mode_from_env_value(Some("")), (d, true));
    assert_eq!(mode_from_env_value(Some("0")), (Mode::Off, true));
    assert_eq!(mode_from_env_value(Some(" 0 ")), (Mode::Off, true));
    assert_eq!(mode_from_env_value(Some("OFF")), (Mode::Off, true));
    assert_eq!(mode_from_env_value(Some("shadow")), (Mode::Shadow, true));
    assert_eq!(mode_from_env_value(Some("1")), (Mode::Enforce, true));
    assert_eq!(mode_from_env_value(Some("enforce")), (Mode::Enforce, true));
    assert_eq!(
        mode_from_env_value(Some("false")),
        (d, false),
        "garbage takes the default, flagged"
    );
}

#[test]
fn platform_default_is_enforce_on_linux_and_shadow_on_windows() {
    if cfg!(windows) {
        assert_eq!(platform_default_mode(), Mode::Shadow);
    } else {
        assert_eq!(platform_default_mode(), Mode::Enforce);
    }
}

// --- /proc/net/tcp parsing ---------------------------------------------------

const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

#[test]
fn proc_header_and_garbage_are_skipped() {
    assert_eq!(parse_proc_line(HEADER), None);
    assert_eq!(parse_proc_line(""), None);
    assert_eq!(parse_proc_line("   0: zz:zz 0100007F:2694 01"), None);
}

#[test]
fn proc_ipv4_row_parses() {
    // 127.0.0.1:41234 -> 127.0.0.1:9876, ESTABLISHED, uid 1001, inode 555.
    let line = "   3: 0100007F:A112 0100007F:2694 01 00000000:00000000 00:00000000 00000000  1001        0 555 1 0000000000000000 20 4 30 10 -1";
    let row = parse_proc_line(line).expect("parses");
    assert_eq!(row.local, sa("127.0.0.1:41234"));
    assert_eq!(row.remote, sa("127.0.0.1:9876"));
    assert_eq!(row.state, 0x01);
    assert_eq!(row.uid, 1001);
    assert_eq!(row.inode, 555);
}

#[test]
fn proc_ipv4_mapped_ipv6_row_parses_and_canonicalises() {
    // ::ffff:127.0.0.1:41235 -> ::ffff:127.0.0.1:9876 in tcp6.
    let line = "   0: 0000000000000000FFFF00000100007F:A113 0000000000000000FFFF00000100007F:2694 01 00000000:00000000 00:00000000 00000000  1002        0 777 1 0000000000000000 20 4 30 10 -1";
    let row = parse_proc_line(line).expect("parses");
    assert_eq!(
        row.local.ip(),
        IpAddr::V6(Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped())
    );
    assert_eq!(canonical(row.local), sa("127.0.0.1:41235"));
    assert_eq!(
        owner_uid([row], sa("127.0.0.1:9876"), sa("127.0.0.1:41235")),
        Ok(1002)
    );
}

#[test]
fn proc_ipv6_loopback_parses() {
    let line = "   0: 00000000000000000000000001000000:A114 00000000000000000000000001000000:2694 01 00000000:00000000 00:00000000 00000000  1003        0 888 1 0000000000000000 20 4 30 10 -1";
    let row = parse_proc_line(line).expect("parses");
    assert_eq!(
        row.local,
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 41236)
    );
}

fn row(local: &str, remote: &str, state: u8, uid: u32, inode: u64) -> ProcRow {
    ProcRow {
        local: sa(local),
        remote: sa(remote),
        state,
        uid,
        inode,
    }
}

#[test]
fn owner_is_the_peer_row_not_the_server_row() {
    let server = row("127.0.0.1:9876", "127.0.0.1:41234", 0x01, 1000, 10);
    let client = row("127.0.0.1:41234", "127.0.0.1:9876", 0x01, 1001, 11);
    assert_eq!(
        owner_uid(
            [server, client],
            sa("127.0.0.1:9876"),
            sa("127.0.0.1:41234")
        ),
        Ok(1001)
    );
    // Order must not matter.
    assert_eq!(
        owner_uid(
            [client, server],
            sa("127.0.0.1:9876"),
            sa("127.0.0.1:41234")
        ),
        Ok(1001)
    );
}

#[test]
fn a_different_port_does_not_match() {
    let client = row("127.0.0.1:41299", "127.0.0.1:9876", 0x01, 1001, 11);
    assert_eq!(
        owner_uid([client], sa("127.0.0.1:9876"), sa("127.0.0.1:41234")),
        Err(Unresolved::NoMatchingSocket)
    );
}

#[test]
fn time_wait_and_orphan_rows_are_unresolved_not_root() {
    let tw = row(
        "127.0.0.1:41234",
        "127.0.0.1:9876",
        PROC_STATE_TIME_WAIT,
        0,
        0,
    );
    assert_eq!(
        owner_uid([tw], sa("127.0.0.1:9876"), sa("127.0.0.1:41234")),
        Err(Unresolved::SocketClosing)
    );
    let orphan = row("127.0.0.1:41234", "127.0.0.1:9876", 0x08, 0, 0);
    assert_eq!(
        owner_uid([orphan], sa("127.0.0.1:9876"), sa("127.0.0.1:41234")),
        Err(Unresolved::SocketClosing)
    );
}

#[test]
fn a_stale_row_before_the_live_one_does_not_hide_it() {
    let tw = row(
        "127.0.0.1:41234",
        "127.0.0.1:9876",
        PROC_STATE_TIME_WAIT,
        0,
        0,
    );
    let live = row("127.0.0.1:41234", "127.0.0.1:9876", 0x01, 1001, 12);
    assert_eq!(
        owner_uid([tw, live], sa("127.0.0.1:9876"), sa("127.0.0.1:41234")),
        Ok(1001)
    );
}

#[test]
fn empty_table_is_no_matching_socket() {
    assert_eq!(
        owner_uid(
            Vec::<ProcRow>::new(),
            sa("127.0.0.1:9876"),
            sa("127.0.0.1:1")
        ),
        Err(Unresolved::NoMatchingSocket)
    );
}

// --- Windows decoders (platform-independent) ---------------------------------

#[test]
fn win_port_decodes_network_order() {
    // Port 9876 = 0x2694 stored as bytes [0x26, 0x94, 0, 0]; read LE -> 0x9426.
    assert_eq!(win_port(0x0000_9426), 9876);
    assert_eq!(win_port(0xFFFF_9426), 9876, "high bits are ignored");
}

#[test]
fn win_ipv4_decodes_network_order() {
    // 127.0.0.1 stored as bytes [127,0,0,1]; read LE -> 0x0100007F.
    assert_eq!(win_ipv4(0x0100_007F), Ipv4Addr::new(127, 0, 0, 1));
}

fn win(local: &str, remote: &str, state: u32, pid: u32) -> WinRow {
    WinRow {
        local: sa(local),
        remote: sa(remote),
        state,
        pid,
    }
}

#[test]
fn win_owner_pid_matches_peer_row() {
    let rows = [
        win("127.0.0.1:9876", "127.0.0.1:50000", 5, 100),
        win("127.0.0.1:50000", "127.0.0.1:9876", 5, 200),
    ];
    assert_eq!(
        owner_pid(rows, sa("127.0.0.1:9876"), sa("127.0.0.1:50000")),
        Ok(200)
    );
    assert_eq!(
        owner_pid(rows, sa("127.0.0.1:9876"), sa("127.0.0.1:50001")),
        Err(Unresolved::NoMatchingSocket)
    );
}

#[test]
fn win_time_wait_and_pid_zero_rows_are_skipped_not_trusted() {
    let tw = win("127.0.0.1:50000", "127.0.0.1:9876", WIN_STATE_TIME_WAIT, 0);
    let orphan = win("127.0.0.1:50000", "127.0.0.1:9876", 5, 0);
    let live = win("127.0.0.1:50000", "127.0.0.1:9876", 5, 300);
    assert_eq!(
        owner_pid([tw, orphan], sa("127.0.0.1:9876"), sa("127.0.0.1:50000")),
        Err(Unresolved::SocketClosing)
    );
    assert_eq!(
        owner_pid([tw, live], sa("127.0.0.1:9876"), sa("127.0.0.1:50000")),
        Ok(300)
    );
}

#[test]
fn filetime_converts_to_unix_time() {
    // 2026-10-04T00:00:00Z = 1_791_072_000 s since the Unix epoch.
    let ticks: u64 = 116_444_736_000_000_000 + 1_791_072_000 * 10_000_000 + 5;
    let t = filetime_to_system_time(ticks as u32, (ticks >> 32) as u32).unwrap();
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
    assert_eq!(d.as_secs(), 1_791_072_000);
    assert_eq!(d.subsec_nanos(), 500);
    assert_eq!(filetime_to_system_time(0, 0), None, "pre-1970 is None");
}

// --- sock_diag codec (platform-independent) ----------------------------------

#[test]
fn sock_diag_request_encodes_the_client_socket_4_tuple() {
    let req = sock_diag_request(sa("127.0.0.1:9876"), sa("127.0.0.1:41234"), 7);
    assert_eq!(req.len(), 72, "16-byte nlmsghdr + 56-byte inet_diag_req_v2");
    assert_eq!(u32::from_ne_bytes(req[0..4].try_into().unwrap()), 72);
    assert_eq!(
        u16::from_ne_bytes(req[4..6].try_into().unwrap()),
        20,
        "SOCK_DIAG_BY_FAMILY"
    );
    assert_eq!(
        u16::from_ne_bytes(req[6..8].try_into().unwrap()),
        1,
        "NLM_F_REQUEST only, no dump"
    );
    assert_eq!(u32::from_ne_bytes(req[8..12].try_into().unwrap()), 7, "seq");
    assert_eq!(req[16], 2, "AF_INET");
    assert_eq!(req[17], 6, "IPPROTO_TCP");
    // sport = the PEER's port, dport = ours, network order.
    assert_eq!(&req[24..26], &41234u16.to_be_bytes());
    assert_eq!(&req[26..28], &9876u16.to_be_bytes());
    assert_eq!(&req[28..32], &[127, 0, 0, 1], "src = the peer address");
    assert_eq!(&req[44..48], &[127, 0, 0, 1], "dst = our address");
    assert_eq!(&req[64..72], &[0xff; 8], "INET_DIAG_NOCOOKIE");
}

/// A `SOCK_DIAG_BY_FAMILY` reply echoing `src:sport -> dst:dport` (IPv4).
fn diag_msg(state: u8, uid: u32, inode: u32, src: &str, dst: &str) -> Vec<u8> {
    let (src, dst) = (sa(src), sa(dst));
    let mut b = vec![0u8; 16 + 72];
    b[0..4].copy_from_slice(&88u32.to_ne_bytes());
    b[4..6].copy_from_slice(&20u16.to_ne_bytes());
    b[16] = 2;
    b[17] = state;
    b[16 + 4..16 + 6].copy_from_slice(&src.port().to_be_bytes());
    b[16 + 6..16 + 8].copy_from_slice(&dst.port().to_be_bytes());
    if let (IpAddr::V4(s4), IpAddr::V4(d4)) = (src.ip(), dst.ip()) {
        b[16 + 8..16 + 12].copy_from_slice(&s4.octets());
        b[16 + 24..16 + 28].copy_from_slice(&d4.octets());
    }
    b[16 + 64..16 + 68].copy_from_slice(&uid.to_ne_bytes());
    b[16 + 68..16 + 72].copy_from_slice(&inode.to_ne_bytes());
    b
}

const C: &str = "127.0.0.1:41234"; // the client (peer) end
const S: &str = "127.0.0.1:9876"; // the runner's end

fn diag(state: u8, uid: u32, inode: u32, src: &str, dst: &str) -> Result<u32, Unresolved> {
    owner_from_diag(
        parse_sock_diag_reply(&diag_msg(state, uid, inode, src, dst)).unwrap(),
        sa(S),
        sa(C),
    )
}

#[test]
fn sock_diag_reply_decodes_found_and_its_echoed_4_tuple() {
    assert_eq!(
        parse_sock_diag_reply(&diag_msg(1, 1001, 555, C, S)),
        Some(DiagReply::Found {
            state: 1,
            uid: 1001,
            inode: 555,
            src: sa(C),
            dst: sa(S)
        })
    );
    assert_eq!(
        diag(1, 1001, 555, C, S),
        Ok(1001),
        "ESTABLISHED client socket"
    );
    assert_eq!(
        diag(4, 1001, 555, C, S),
        Ok(1001),
        "FIN_WAIT1: sent and closed, still the connector"
    );
}

#[test]
fn sock_diag_refuses_a_listener_a_closing_socket_and_a_foreign_echo() {
    // inet_diag_find_one_icsk falls back to a LISTENER on the peer's port when
    // the client socket is gone; that must never vouch for the peer.
    assert_eq!(
        diag(10, 1000, 777, "127.0.0.1:41234", "0.0.0.0:0"),
        Err(Unresolved::NoMatchingSocket)
    );
    assert_eq!(
        diag(10, 1000, 777, C, S),
        Err(Unresolved::NoMatchingSocket),
        "LISTEN even with a matching echo"
    );
    assert_eq!(
        diag(6, 0, 0, C, S),
        Err(Unresolved::SocketClosing),
        "TIME_WAIT is not owned by uid 0"
    );
    assert_eq!(
        diag(1, 1001, 0, C, S),
        Err(Unresolved::SocketClosing),
        "orphaned, inode 0"
    );
    assert_eq!(
        diag(1, 1001, 555, "127.0.0.1:41299", S),
        Err(Unresolved::NoMatchingSocket),
        "echo names another socket"
    );
}

#[test]
fn sock_diag_errors_decode() {
    let mut err = vec![0u8; 20];
    err[0..4].copy_from_slice(&20u32.to_ne_bytes());
    err[4..6].copy_from_slice(&2u16.to_ne_bytes());
    err[16..20].copy_from_slice(&(-2i32).to_ne_bytes());
    assert_eq!(parse_sock_diag_reply(&err), Some(DiagReply::NotFound));
    assert_eq!(
        owner_from_diag(DiagReply::NotFound, sa(S), sa(C)),
        Err(Unresolved::NoMatchingSocket)
    );
    err[16..20].copy_from_slice(&(-22i32).to_ne_bytes());
    assert_eq!(
        owner_from_diag(parse_sock_diag_reply(&err).unwrap(), sa(S), sa(C)),
        Err(Unresolved::TableUnreadable)
    );
    assert_eq!(parse_sock_diag_reply(&[0u8; 3]), None, "truncated");
}

// --- the listener, end to end with an injected resolver -----------------------

struct Fixed {
    own: Result<LocalUser, String>,
    peer: Result<LocalUser, Unresolved>,
}

impl OwnerResolver for Fixed {
    fn own_user(&self) -> Result<LocalUser, String> {
        self.own.clone()
    }
    fn resolve(&self, _l: SocketAddr, _p: SocketAddr, _t: SystemTime) -> Resolution {
        Resolution {
            owner: self.peer.clone(),
            pid: None,
        }
    }
}

async fn serve_with(resolver: Fixed, mode: Mode) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guarded =
        GuardedListener::with_resolver(listener, "test", Arc::new(resolver), mode).unwrap();
    let addr = axum::serve::Listener::local_addr(&guarded).unwrap();
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));
    tokio::spawn(async move {
        let _ = axum::serve(guarded, app).await;
    });
    addr
}

/// The raw bytes a GET /health returns before the server closes or 3 s pass.
async fn get(addr: SocketAddr) -> Vec<u8> {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let _ = s
        .write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(3), s.read_to_end(&mut out)).await;
    out
}

fn is_ok_response(bytes: &[u8]) -> bool {
    let s = String::from_utf8_lossy(bytes);
    s.starts_with("HTTP/1.1 200") && s.ends_with("ok")
}

#[tokio::test]
async fn listener_admits_the_same_user() {
    let addr = serve_with(
        Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Ok(LocalUser::Uid(7)),
        },
        Mode::Enforce,
    )
    .await;
    assert!(is_ok_response(&get(addr).await));
    // A second connection is served too (the accept loop keeps going).
    assert!(is_ok_response(&get(addr).await));
}

#[tokio::test]
async fn listener_refuses_another_user_before_any_byte() {
    let addr = serve_with(
        Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Ok(LocalUser::Uid(8)),
        },
        Mode::Enforce,
    )
    .await;
    let before = STATS
        .refused_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        get(addr).await.is_empty(),
        "a refused peer gets no bytes at all"
    );
    let after = STATS
        .refused_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(after > before, "the otherUser counter moved");
}

#[tokio::test]
async fn listener_refuses_an_unresolved_peer() {
    let addr = serve_with(
        Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Err(Unresolved::NoMatchingSocket),
        },
        Mode::Enforce,
    )
    .await;
    assert!(get(addr).await.is_empty());
}

#[tokio::test]
async fn listener_refuses_everyone_when_own_user_is_unknown() {
    let addr = serve_with(
        Fixed {
            own: Err("no token".into()),
            peer: Ok(LocalUser::Uid(7)),
        },
        Mode::Enforce,
    )
    .await;
    assert!(get(addr).await.is_empty());
}

#[tokio::test]
async fn kill_switch_passes_every_peer_through() {
    let addr = serve_with(
        Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Ok(LocalUser::Uid(8)),
        },
        Mode::Off,
    )
    .await;
    assert!(is_ok_response(&get(addr).await));
}

#[tokio::test]
async fn shadow_admits_a_would_refuse_and_counts_it() {
    let addr = serve_with(
        Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Ok(LocalUser::Uid(9)),
        },
        Mode::Shadow,
    )
    .await;
    let before = STATS
        .shadow_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(is_ok_response(&get(addr).await), "shadow never refuses");
    let after = STATS
        .shadow_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "the shadowWouldRefuse.otherUser counter moved"
    );
}

/// Review finding L1: dropping the listener must release the port at once —
/// the Cognito callback re-binds its fixed port right after shutdown.
#[tokio::test]
async fn dropping_the_listener_releases_the_port_synchronously() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guarded = GuardedListener::with_resolver(
        listener,
        "test-drop",
        Arc::new(Fixed {
            own: Ok(LocalUser::Uid(7)),
            peer: Ok(LocalUser::Uid(7)),
        }),
        Mode::Enforce,
    )
    .unwrap();
    let addr = axum::serve::Listener::local_addr(&guarded).unwrap();
    drop(guarded);
    std::net::TcpListener::bind(addr).expect("port free immediately after drop");
}

#[test]
fn health_block_names_its_fields() {
    let v = health_json();
    for k in [
        "installed",
        "enabled",
        "mode",
        "supported",
        "killSwitchEnv",
        "admitted",
    ] {
        assert!(v.get(k).is_some(), "{k}");
    }
    assert_eq!(v["killSwitchEnv"], "QONTINUI_RUNNER_PEER_USER_GUARD");
    assert!(v["refusals"].get("otherUser").is_some());
    assert!(v["refusals"].get("unresolved").is_some());
    assert!(v["shadowWouldRefuse"].get("otherUser").is_some());
    assert!(v["shadowWouldRefuse"].get("unresolved").is_some());
}

// --- Linux: the real /proc resolver ------------------------------------------

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_proc_resolver_finds_this_process_as_the_peer_owner() {
    let resolver = linux::ProcNetResolver;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let client = tokio::net::TcpStream::connect(local).await.unwrap();
    let (_server, peer) = listener.accept().await.unwrap();
    assert_eq!(client.local_addr().unwrap(), peer);
    let r = tokio::task::spawn_blocking(move || resolver.resolve(local, peer, SystemTime::now()))
        .await
        .unwrap();
    // SAFETY: no preconditions.
    let me = unsafe { libc::geteuid() };
    assert_eq!(r.owner, Ok(LocalUser::Uid(me)));
    assert_eq!(linux::ProcNetResolver.own_user(), Ok(LocalUser::Uid(me)));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn netlink_and_proc_each_find_this_process_as_the_peer_owner() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    let _client = tokio::net::TcpStream::connect(local).await.unwrap();
    let (_server, peer) = listener.accept().await.unwrap();
    // SAFETY: no preconditions.
    let me = unsafe { libc::geteuid() };
    let nl = tokio::task::spawn_blocking(move || linux::netlink_owner(local, peer))
        .await
        .unwrap();
    assert_eq!(nl, Ok(Ok(me)), "exact sock_diag lookup");
    let pr = tokio::task::spawn_blocking(move || linux::proc_owner(local, peer))
        .await
        .unwrap();
    assert_eq!(pr, Ok(me), "/proc fallback");
}

/// Review finding M1, reproduced live: with no client socket for the
/// 4-tuple, the kernel's exact lookup falls back to a LISTENER bound on the
/// peer's port. A listener owned by this very user must not vouch for it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_listener_on_the_peer_port_does_not_vouch_for_the_peer() {
    let ours = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = ours.local_addr().unwrap();
    let decoy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ghost_peer = decoy.local_addr().unwrap();
    assert_eq!(
        linux::netlink_owner(local, ghost_peer),
        Ok(Err(Unresolved::NoMatchingSocket))
    );
    assert_eq!(
        linux::proc_owner(local, ghost_peer),
        Err(Unresolved::NoMatchingSocket)
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_proc_resolver_does_not_match_a_stranger_4_tuple() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap();
    // Port 1 on loopback never carries a client socket to `local`.
    let r = linux::ProcNetResolver.resolve(local, sa("127.0.0.1:1"), SystemTime::now());
    assert_eq!(r.owner, Err(Unresolved::NoMatchingSocket));
    assert_eq!(
        linux::netlink_owner(local, sa("127.0.0.1:1")),
        Ok(Err(Unresolved::NoMatchingSocket))
    );
}

/// A genuine second OS user, end to end through the real resolver and a real
/// `GuardedListener`. Needs a command prefix that runs a process as another
/// uid, e.g. `QONTINUI_PEER_GUARD_OTHER_USER_CMD="sudo -n -u nobody"`, and
/// `curl` on PATH. Ignored by default: CI hosts do not grant that.
#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "needs QONTINUI_PEER_GUARD_OTHER_USER_CMD (e.g. `sudo -n -u nobody`)"]
async fn a_genuine_other_user_is_refused() {
    let prefix = std::env::var("QONTINUI_PEER_GUARD_OTHER_USER_CMD")
        .expect("set QONTINUI_PEER_GUARD_OTHER_USER_CMD");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guarded = GuardedListener::with_resolver(
        listener,
        "test-other-user",
        Arc::new(linux::ProcNetResolver),
        Mode::Enforce,
    )
    .unwrap();
    let addr = axum::serve::Listener::local_addr(&guarded).unwrap();
    let app = axum::Router::new().route("/health", axum::routing::get(|| async { "ok" }));
    tokio::spawn(async move {
        let _ = axum::serve(guarded, app).await;
    });
    // Same user: admitted.
    assert!(is_ok_response(&get(addr).await));
    // Other user: refused (curl reports an empty reply / reset).
    let before = STATS
        .refused_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut parts = prefix.split_whitespace();
    let out = tokio::process::Command::new(parts.next().unwrap())
        .args(parts)
        .args(["curl", "-sS", "-m", "5", &format!("http://{addr}/health")])
        .output()
        .await
        .unwrap();
    let body = String::from_utf8_lossy(&out.stdout);
    assert_ne!(body, "ok", "another uid must not get the body");
    let after = STATS
        .refused_other_user
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(after > before, "otherUser counter moved");
}
