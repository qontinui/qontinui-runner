//! Corporate CA trust: which trust source each of the runner's TLS stacks
//! uses (plan `2026-10-10-spec-front-end-phase-9-generic-boundary`, Phase 5,
//! decision C3).
//!
//! On a managed machine IT installs the corporate root into the OS trust
//! store, so every stack must read that store; no product-level CA option is
//! added for Rust code. Node cannot read the store natively and takes the
//! profile's `network.ca_bundle` through `NODE_EXTRA_CA_CERTS`, which
//! [`super::apply_profile_environment`] exports; the Python bridge reads the
//! store through `truststore`.
//!
//! `network.ca_bundle` is exported to Node ONLY, as the additive
//! `NODE_EXTRA_CA_CERTS`. It is never exported as `SSL_CERT_FILE`: on Linux
//! that variable REPLACES the system trust store for rustls-native-certs and
//! OpenSSL (and so for `truststore` in the Python bridge), which would drop
//! the public roots instead of adding the corporate one. Python therefore
//! reads the OS store only, through `truststore`.
//!
//! [`census`] is the table the config report's `tls_trust` layer prints, so a
//! deployment can show its security team what it trusts. Each row says how its
//! verdict is known: measured by a test in this module (on the host named),
//! configured (a setting whose only trust source is the OS store), or UNKNOWN.
//! A row nobody measured is never rendered `os-store`.

use super::{GitTrustDecision, ProxyEnvOutcome};

/// Where a stack's trust anchors come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustVerdict {
    /// The OS trust store (SChannel / Security.framework / the system CA
    /// store on Linux) — a corporate root installed by IT is trusted.
    OsStore,
    /// A bundle compiled into or shipped with the stack — a corporate root is
    /// NOT trusted unless supplied separately.
    Bundled,
    /// Not established on this host. Never a default.
    Unknown,
}

impl TrustVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustVerdict::OsStore => "os-store",
            TrustVerdict::Bundled => "bundled",
            TrustVerdict::Unknown => "UNKNOWN",
        }
    }
}

/// One stack's row in the census.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TrustRow {
    /// The stack and what rides on it.
    pub stack: &'static str,
    /// The trust mechanism at this build.
    pub source: String,
    pub verdict: TrustVerdict,
    /// How the verdict is known.
    pub basis: String,
}

/// The census for this process, given the startup outcome (which says whether
/// a CA bundle was exported, and whether git was pointed at Schannel).
pub fn census(outcome: Option<&ProxyEnvOutcome>) -> Vec<TrustRow> {
    census_for(outcome, cfg!(windows))
}

/// [`census`] with the platform as a parameter, so both arms are testable on
/// any host.
pub fn census_for(outcome: Option<&ProxyEnvOutcome>, windows: bool) -> Vec<TrustRow> {
    let mut rows = vec![
        TrustRow {
            stack: "reqwest 0.13 (HTTP clients, OTLP export, updater)",
            source: "rustls-platform-verifier".into(),
            verdict: TrustVerdict::OsStore,
            basis: "measured on Linux by outbound_net::tls_trust::tests::\
                    reqwest_trusts_the_os_store (a throwaway root in the system trust \
                    input is trusted, an unrelated root is refused); on Windows and macOS \
                    the same verifier calls SChannel / Security.framework"
                .into(),
        },
        TrustRow {
            stack: "tokio-tungstenite 0.29 (coord /ws lanes, web relay, cloud tunnel)",
            source: "native-tls".into(),
            verdict: TrustVerdict::OsStore,
            basis: "measured on Linux by outbound_net::tls_trust::tests::\
                    websocket_tls_trusts_the_os_store; native-tls is SChannel on Windows \
                    and Security.framework on macOS"
                .into(),
        },
        TrustRow {
            stack: "sentry 0.35 (crash reports)",
            source: "native-tls (reqwest 0.12 default-tls)".into(),
            verdict: TrustVerdict::OsStore,
            basis: "the same native-tls connector as the WebSocket row; \
                    outbound_net::tls_trust::tests::webpki_roots_has_no_dependent_but_the_pinned_ones \
                    pins sentry off webpki-roots"
                .into(),
        },
    ];

    const GIT: &str = "git subprocesses (agent_pusher, canonical_corpus, ci_node)";
    let machine = "the machine's own git config (http.sslBackend / http.sslCAInfo, or Git \
                   for Windows' default OpenSSL bundle when it names neither)";
    rows.push(match (windows, outcome.map(|o| o.git_trust)) {
        (false, _) | (_, Some(GitTrustDecision::NotWindows)) => TrustRow {
            stack: GIT,
            source: "the system git's libcurl TLS backend".into(),
            verdict: TrustVerdict::Unknown,
            basis: "not measured: depends on how the distribution built libcurl \
                    (OpenSSL reads the system CA file; GnuTLS builds honour only \
                    GIT_SSL_CAINFO); the runner never changes it off Windows"
                .into(),
        },
        (true, Some(GitTrustDecision::Schannel)) => TrustRow {
            stack: GIT,
            source: "http.sslBackend=schannel (GIT_CONFIG_* exported at startup)".into(),
            verdict: TrustVerdict::OsStore,
            basis: "configured: the profile says network.trust = \"os\" and the machine's \
                    git config chose no backend or CA file; Schannel's only trust source is \
                    the Windows store; not measured on CI"
                .into(),
        },
        (true, Some(GitTrustDecision::Bundled)) => TrustRow {
            stack: GIT,
            source: machine.into(),
            verdict: TrustVerdict::Unknown,
            basis: "network.trust = \"bundled\": the runner leaves git as the machine \
                    configured it"
                .into(),
        },
        (true, Some(GitTrustDecision::OperatorConfigured)) => TrustRow {
            stack: GIT,
            source: machine.into(),
            verdict: TrustVerdict::Unknown,
            basis: "the machine's git config (or the operator's GIT_CONFIG_*) already \
                    chooses a TLS backend or CA file; the runner does not override it"
                .into(),
        },
        (true, Some(GitTrustDecision::NotRequested)) => TrustRow {
            stack: GIT,
            source: machine.into(),
            verdict: TrustVerdict::Unknown,
            basis: "the profile does not say network.trust = \"os\", so the runner \
                    leaves git alone; set it to point git at Schannel (the Windows store)"
                .into(),
        },
        (true, Some(GitTrustDecision::ConfigUnreadable | GitTrustDecision::UnreadableCount)) => {
            TrustRow {
                stack: GIT,
                source: machine.into(),
                verdict: TrustVerdict::Unknown,
                basis: "the startup step could not read git's configuration, so it left git \
                        alone"
                    .into(),
            }
        }
        (true, None) => TrustRow {
            stack: GIT,
            source: machine.into(),
            verdict: TrustVerdict::Unknown,
            basis: "this process did not run the startup step".into(),
        },
    });

    rows.push(TrustRow {
        stack: "claude CLI (Node)",
        source: match outcome.and_then(|o| o.node_extra_ca_certs.as_deref()) {
            Some(p) => format!("Node's bundled Mozilla roots + NODE_EXTRA_CA_CERTS={p}"),
            None if outcome.is_some() => "Node's bundled Mozilla roots".into(),
            None => "Node's bundled Mozilla roots (this process did not run the startup \
                     step; NODE_EXTRA_CA_CERTS not reflected)"
                .into(),
        },
        verdict: TrustVerdict::Bundled,
        basis: "measured on Linux by outbound_net::tls_trust::tests::\
                node_ignores_the_os_store_and_honours_extra_ca_certs when `node` is on \
                PATH: a root in the system trust input is refused, the same root in \
                NODE_EXTRA_CA_CERTS is trusted — set network.ca_bundle"
            .into(),
    });

    rows.push(TrustRow {
        stack: "python-bridge (requests / httpx / aiohttp)",
        source: "truststore.inject_into_ssl() at startup (OS store)".into(),
        verdict: TrustVerdict::Unknown,
        basis: "not measured on CI: the bridge's interpreter and its truststore install \
                are not part of the Rust test host. network.ca_bundle is NOT passed to it: \
                SSL_CERT_FILE would replace the OS store on Linux, so the corporate root \
                must be in the OS store"
            .into(),
    });
    rows
}

/// The census as one line for the config report's value column.
pub fn render_line(rows: &[TrustRow]) -> String {
    rows.iter()
        .map(|r| format!("{} = {} [{}]", r.stack, r.verdict.as_str(), r.source))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // ---------------------------------------------------------------------
    // webpki-roots: the bundled-roots crate is gone from crash reporting.
    // ---------------------------------------------------------------------

    /// The packages that may still depend on `webpki-roots`, pinned EXACTLY
    /// so the assertion stays falsifiable both ways: a new bundled-roots
    /// dependent fails it, and so would a pinned one going away.
    ///
    /// Measured empty once crash reporting moved to native-tls: sentry's
    /// `rustls` feature was the only thing enabling reqwest 0.12's
    /// `rustls-tls-webpki-roots`, and through it `hyper-rustls`'s
    /// `webpki-roots` feature, so with it gone nothing in the graph links the
    /// bundled roots. reqwest 0.13 verifies with `rustls-platform-verifier`
    /// (the OS store, measured below).
    const PINNED_WEBPKI_ROOTS_DEPENDENTS: &[&str] = &[];

    /// Names of the packages whose resolved dependencies include
    /// `webpki-roots`, from `cargo metadata` over this crate's lockfile.
    fn webpki_roots_dependents() -> Vec<String> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let out = Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--offline",
                "--manifest-path",
            ])
            .arg(&manifest)
            .output()
            .expect("run cargo metadata");
        assert!(
            out.status.success(),
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let name_of = |id: &str| -> String {
            meta["packages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|p| p["id"] == id)
                .and_then(|p| p["name"].as_str())
                .unwrap_or("?")
                .to_string()
        };
        let mut dependents: Vec<String> = meta["resolve"]["nodes"]
            .as_array()
            .expect("a resolve graph")
            .iter()
            .filter(|node| {
                node["deps"].as_array().is_some_and(|deps| {
                    deps.iter().any(|d| {
                        d["pkg"]
                            .as_str()
                            .is_some_and(|id| name_of(id) == "webpki-roots")
                    })
                })
            })
            .map(|node| name_of(node["id"].as_str().unwrap()))
            .filter(|n| n != "webpki-roots")
            .collect();
        dependents.sort();
        dependents.dedup();
        dependents
    }

    #[test]
    fn webpki_roots_has_no_dependent_but_the_pinned_ones() {
        let dependents = webpki_roots_dependents();
        assert!(
            !dependents.iter().any(|d| d == "sentry"),
            "sentry must use native-tls (the OS store), not webpki-roots: {dependents:?}"
        );
        assert_eq!(
            dependents, PINNED_WEBPKI_ROOTS_DEPENDENTS,
            "the crates still pulling bundled roots changed — re-run the census"
        );
    }

    #[test]
    fn sentry_is_built_with_native_tls() {
        let manifest =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .unwrap();
        let line = manifest
            .lines()
            .find(|l| l.trim_start().starts_with("sentry = "))
            .expect("a sentry dependency line");
        assert!(
            line.contains("\"native-tls\""),
            "sentry must enable native-tls: {line}"
        );
        assert!(
            !line.contains("\"rustls\""),
            "sentry must not enable rustls: {line}"
        );
    }

    // ---------------------------------------------------------------------
    // Measured rows: a throwaway root in the system trust input.
    //
    // On Linux the system trust input of OpenSSL (native-tls) and of
    // rustls-native-certs (rustls-platform-verifier's Linux arm) is
    // SSL_CERT_FILE / SSL_CERT_DIR. Both are process-global and read once, so
    // each probe runs in a CHILD process — this test binary re-executed on the
    // ignored `probe_child` test — with its own environment. The positive arm
    // (our root in the input) must connect; the negative control (an
    // unrelated root) must not, which proves verification is on and the trust
    // came from the input rather than from a bundle.
    // ---------------------------------------------------------------------

    struct Pki {
        dir: tempfile::TempDir,
        root: PathBuf,
        other_root: PathBuf,
        identity: native_tls::Identity,
    }

    fn mint_pki() -> Pki {
        use rcgen::{
            BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose,
        };
        let ca = |cn: &str| {
            let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, cn);
            CertifiedIssuer::self_signed(params, KeyPair::generate().unwrap()).unwrap()
        };
        let root = ca("qontinui throwaway test root");
        let other = ca("qontinui unrelated test root");
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&leaf_key, &root)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().join("root.pem");
        let other_path = dir.path().join("other.pem");
        std::fs::write(&root_path, root.pem()).unwrap();
        std::fs::write(&other_path, other.pem()).unwrap();
        std::fs::create_dir(dir.path().join("empty-cert-dir")).unwrap();
        let identity = native_tls::Identity::from_pkcs8(
            leaf.pem().as_bytes(),
            leaf_key.serialize_pem().as_bytes(),
        )
        .unwrap();
        Pki {
            dir,
            root: root_path,
            other_root: other_path,
            identity,
        }
    }

    #[derive(Clone, Copy)]
    enum Serve {
        Http,
        WebSocket,
    }

    /// A TLS server on 127.0.0.1 presenting the leaf, answering each
    /// connection with a tiny HTTP 200 or a WebSocket upgrade.
    async fn spawn_tls_server(identity: native_tls::Identity, serve: Serve) -> u16 {
        let acceptor =
            tokio_native_tls::TlsAcceptor::from(native_tls::TlsAcceptor::new(identity).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    match serve {
                        Serve::Http => {
                            let mut buf = [0u8; 2048];
                            let _ = tls.read(&mut buf).await;
                            let _ = tls
                                .write_all(
                                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                                )
                                .await;
                            let _ = tls.shutdown().await;
                        }
                        Serve::WebSocket => {
                            let _ = tokio_tungstenite::accept_async(tls).await;
                        }
                    }
                });
            }
        });
        port
    }

    /// Run `probe_child` in a fresh copy of this test binary with `trust` as
    /// the system trust input. `true` when the child connected.
    fn probe_in_child(probe: &str, trust: &Path, empty_dir: &Path) -> bool {
        let exe = std::env::current_exe().unwrap();
        let mut child = Command::new(exe)
            .args([
                "outbound_net::tls_trust::tests::probe_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("QONTINUI_TLS_PROBE", probe)
            .env("SSL_CERT_FILE", trust)
            .env("SSL_CERT_DIR", empty_dir)
            .env("NO_PROXY", "*")
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status.success();
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the TLS probe child did not finish in 60s ({probe})");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The child half. Ignored in a normal run (it needs the parent's
    /// environment); the parent runs it with `--ignored`.
    #[test]
    #[ignore = "child process of the tls_trust measurements; run by them, not directly"]
    fn probe_child() {
        let probe = std::env::var("QONTINUI_TLS_PROBE").expect("run by a parent test");
        let (kind, url) = probe.split_once('|').unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            match kind {
                "reqwest" => {
                    let resp = reqwest::Client::new()
                        .get(url)
                        .send()
                        .await
                        .expect("TLS ok");
                    assert!(resp.status().is_success());
                }
                "ws" => {
                    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
                    let req = url.into_client_request().unwrap();
                    crate::outbound_net::connect_ws(req, Duration::from_secs(20))
                        .await
                        .expect("TLS ok");
                }
                other => panic!("unknown probe {other}"),
            }
        });
    }

    fn linux_only(row: &str) -> bool {
        if cfg!(target_os = "linux") {
            return true;
        }
        eprintln!("{row}: not measured on this host — the census row stays as documented, not re-verified");
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reqwest_trusts_the_os_store() {
        if !linux_only("reqwest") {
            return;
        }
        let pki = mint_pki();
        let port = spawn_tls_server(pki.identity.clone(), Serve::Http).await;
        let probe = format!("reqwest|https://localhost:{port}/");
        let empty = pki.dir.path().join("empty-cert-dir");
        let (root, other) = (pki.root.clone(), pki.other_root.clone());
        let (trusted, refused) = tokio::task::spawn_blocking(move || {
            (
                probe_in_child(&probe, &root, &empty),
                probe_in_child(&probe, &other, &empty),
            )
        })
        .await
        .unwrap();
        assert!(
            trusted,
            "reqwest must trust a root placed in the system trust input"
        );
        assert!(
            !refused,
            "negative control: an unrelated root must not verify the leaf"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn websocket_tls_trusts_the_os_store() {
        if !linux_only("tokio-tungstenite") {
            return;
        }
        let pki = mint_pki();
        let port = spawn_tls_server(pki.identity.clone(), Serve::WebSocket).await;
        let probe = format!("ws|wss://localhost:{port}/ws");
        let empty = pki.dir.path().join("empty-cert-dir");
        let (root, other) = (pki.root.clone(), pki.other_root.clone());
        let (trusted, refused) = tokio::task::spawn_blocking(move || {
            (
                probe_in_child(&probe, &root, &empty),
                probe_in_child(&probe, &other, &empty),
            )
        })
        .await
        .unwrap();
        assert!(
            trusted,
            "the WebSocket TLS stack must trust a root in the system trust input"
        );
        assert!(
            !refused,
            "negative control: an unrelated root must not verify the leaf"
        );
    }

    /// Node (the `claude` CLI) ignores the OS trust input and honours
    /// `NODE_EXTRA_CA_CERTS` — the reason `network.ca_bundle` exists. Runs
    /// when `node` is on PATH; otherwise the row is not re-verified here.
    #[tokio::test(flavor = "multi_thread")]
    async fn node_ignores_the_os_store_and_honours_extra_ca_certs() {
        if !linux_only("node") {
            return;
        }
        if Command::new("node").arg("--version").output().is_err() {
            eprintln!("node: not on PATH — not measured on this host");
            return;
        }
        let pki = mint_pki();
        let port = spawn_tls_server(pki.identity.clone(), Serve::Http).await;
        let root = pki.root.clone();
        let empty = pki.dir.path().join("empty-cert-dir");
        let script = format!(
            "require('https').get('https://localhost:{port}/', r => process.exit(r.statusCode === 200 ? 0 : 3))\
             .on('error', () => process.exit(2));"
        );
        let run = move |extra: Option<&Path>| -> bool {
            let mut cmd = Command::new("node");
            cmd.arg("-e")
                .arg(&script)
                .env("SSL_CERT_FILE", &root)
                .env("SSL_CERT_DIR", &empty)
                .env_remove("NODE_OPTIONS")
                .env_remove("NODE_EXTRA_CA_CERTS")
                .env_remove("NODE_USE_SYSTEM_CA")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(p) = extra {
                cmd.env("NODE_EXTRA_CA_CERTS", p);
            }
            cmd.status().unwrap().success()
        };
        let root_for_extra = pki.root.clone();
        let (os_only, with_extra) =
            tokio::task::spawn_blocking(move || (run(None), run(Some(&root_for_extra))))
                .await
                .unwrap();
        assert!(
            !os_only,
            "Node must NOT pick up a root from the system trust input (bundled)"
        );
        assert!(
            with_extra,
            "Node must trust a root supplied via NODE_EXTRA_CA_CERTS"
        );
    }

    // ---------------------------------------------------------------------
    // The census rows themselves.
    // ---------------------------------------------------------------------

    fn outcome(git_trust: GitTrustDecision, ca: Option<&str>) -> ProxyEnvOutcome {
        ProxyEnvOutcome {
            arm: crate::outbound_net::ProxyEnvArm::None,
            proxy: None,
            profile: None,
            no_proxy: String::new(),
            exported: vec![],
            trust: None,
            ca_bundle: ca.map(str::to_string),
            git_ssl_backend: (git_trust == GitTrustDecision::Schannel)
                .then(|| "schannel".to_string()),
            git_trust,
            node_extra_ca_certs: ca.map(str::to_string),
        }
    }

    #[test]
    fn census_git_row_follows_the_startup_decision() {
        let git = |o: Option<ProxyEnvOutcome>, windows: bool| {
            census_for(o.as_ref(), windows)
                .into_iter()
                .find(|r| r.stack.starts_with("git"))
                .unwrap()
        };
        let on = git(Some(outcome(GitTrustDecision::Schannel, None)), true);
        assert_eq!(on.verdict, TrustVerdict::OsStore);
        assert!(on.source.contains("schannel"));
        for d in [
            GitTrustDecision::NotRequested,
            GitTrustDecision::Bundled,
            GitTrustDecision::OperatorConfigured,
            GitTrustDecision::ConfigUnreadable,
            GitTrustDecision::UnreadableCount,
        ] {
            let row = git(Some(outcome(d, None)), true);
            assert_eq!(
                row.verdict,
                TrustVerdict::Unknown,
                "{d:?}: the machine decides"
            );
            assert!(row.source.contains("machine's own git config"), "{d:?}");
        }
        assert_eq!(git(None, true).verdict, TrustVerdict::Unknown);
        let linux = git(Some(outcome(GitTrustDecision::NotWindows, None)), false);
        assert_eq!(
            linux.verdict,
            TrustVerdict::Unknown,
            "unmeasured is never os-store"
        );
    }

    #[test]
    fn census_names_the_ca_bundle_for_node_and_python() {
        let rows = census_for(
            Some(&outcome(
                GitTrustDecision::NotWindows,
                Some("/corp/root.pem"),
            )),
            false,
        );
        let node = rows
            .iter()
            .find(|r| r.stack.starts_with("claude CLI"))
            .unwrap();
        assert!(node.source.contains("NODE_EXTRA_CA_CERTS=/corp/root.pem"));
        let py = rows
            .iter()
            .find(|r| r.stack.starts_with("python-bridge"))
            .unwrap();
        assert!(
            !py.source.contains("SSL_CERT_FILE"),
            "the bundle never reaches Python as SSL_CERT_FILE (it would replace the OS store)"
        );
        assert!(py.basis.contains("replace the OS store"));
        assert_eq!(py.verdict, TrustVerdict::Unknown);
        assert!(render_line(&rows).contains("sentry 0.35 (crash reports) = os-store"));
    }
}
