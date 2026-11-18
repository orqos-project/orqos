//! Startup, readiness and liveness probes (orqos#26), run by Orqos rather than Podman healthchecks: Podman has one
//! healthcheck per container and runs it inside the container, while a pod needs readiness and liveness apart and
//! HTTP probes that work without an HTTP client in the image.
//!
//! Each pod with probes has one task. Every second it runs the probes that are due: `exec` probes with
//! `podman exec`, HTTP(S) probes from a socket opened inside the pod's network namespace (a fresh thread enters it
//! with `setns`, connects to `127.0.0.1:<port>`, and the socket keeps that namespace). A new container instance
//! starts over: not started, not ready. A startup or liveness probe that reaches its failure threshold stops the
//! container (`podman stop`), so the unit's restart policy decides what follows.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

use super::render;
use super::spec::{ContainerSpec, HttpProbe, PodSpec, Probe, Scheme};
use super::units;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Startup,
    Readiness,
    Liveness,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Startup => "startup",
            Kind::Readiness => "readiness",
            Kind::Liveness => "liveness",
        }
    }
}

/// What probes have found about one container instance.
#[derive(Clone, Debug, Default, PartialEq, Serialize, ToSchema)]
pub struct ProbeState {
    /// The container instance the state belongs to (its ID and start time).
    #[serde(skip)]
    pub instance: String,
    /// The startup probe has passed (or there is none).
    pub started: bool,
    /// The readiness probe passes (or the container started and has none).
    pub ready: bool,
    #[serde(skip)]
    pub failures: BTreeMap<String, u32>,
    /// Restarts caused by failing startup or liveness probes, since Orqos started.
    pub probe_restarts: u32,
    pub last_probe_failure: Option<String>,
}

/// One probe result.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Success,
    Failure(String),
}

/// Applies one result: updates the state and says whether the container must be restarted. Readiness needs
/// `failure_threshold` consecutive failures to turn unready and one success to turn ready; startup and liveness
/// restart the container at their threshold.
pub fn step(
    state: &mut ProbeState,
    kind: Kind,
    outcome: Outcome,
    threshold: u32,
    has_readiness: bool,
) -> bool {
    let failures = state.failures.entry(kind.name().into()).or_default();
    match outcome {
        Outcome::Success => {
            *failures = 0;
            match kind {
                Kind::Startup => {
                    state.started = true;
                    state.ready = !has_readiness;
                }
                Kind::Readiness => state.ready = true,
                Kind::Liveness => {}
            }
            false
        }
        Outcome::Failure(message) => {
            *failures += 1;
            let reached = *failures >= threshold;
            state.last_probe_failure = Some(format!("{}: {message}", kind.name()));
            match kind {
                Kind::Readiness => {
                    if reached {
                        state.ready = false;
                    }
                    false
                }
                Kind::Startup | Kind::Liveness => reached,
            }
        }
    }
}

/// HTTP statuses 200–399 succeed, as in Kubernetes.
pub fn http_success(status: u16) -> bool {
    (200..400).contains(&status)
}

/// The status code of an HTTP/1.x status line.
fn status_code(line: &str) -> Option<u16> {
    let mut parts = line.split(' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/1.") {
        return None;
    }
    parts.next()?.parse().ok()
}

/// Probe state of every container, by (pod ID, container name).
pub type States = Arc<DashMap<(String, String), ProbeState>>;

/// Whether a container spec has any probe.
pub fn has_probes(c: &ContainerSpec) -> bool {
    c.startup.is_some() || c.readiness.is_some() || c.liveness.is_some()
}

fn container_name(id: &str, container: &str) -> String {
    format!("{}-{container}", render::pod_name(id))
}

/// The running container's instance (ID and start time), or None when it does not run.
async fn instance(name: &str) -> Option<String> {
    let out = units::run(
        "podman",
        &[
            "inspect",
            "--format",
            "{{.State.Running}} {{.Id}} {{.State.StartedAt}}",
            name,
        ],
        &[],
        Duration::from_secs(10),
    )
    .await
    .ok()?;
    let (running, rest) = out.trim().split_once(' ')?;
    (running == "true").then(|| rest.to_string())
}

/// The PID of the pod's infra container, whose network namespace the pod shares.
async fn infra_pid(id: &str) -> Result<i32, String> {
    let infra = units::run(
        "podman",
        &[
            "pod",
            "inspect",
            "--format",
            "{{.InfraContainerID}}",
            &render::pod_name(id),
        ],
        &[],
        Duration::from_secs(10),
    )
    .await
    .map_err(|e| e.to_string())?;
    let pid = units::run(
        "podman",
        &["inspect", "--format", "{{.State.Pid}}", infra.trim()],
        &[],
        Duration::from_secs(10),
    )
    .await
    .map_err(|e| e.to_string())?;
    pid.trim()
        .parse()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| "the pod's infra container is not running".into())
}

/// Kills every process whose environment carries `ORQOS_PROBE=<token>`: a timed-out `podman exec` leaves its
/// process running in the container, and the token (inherited by its children) finds exactly those.
fn kill_tagged(token: &str) -> usize {
    let needle = format!("ORQOS_PROBE={token}");
    let mut killed = 0;
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if environ
            .split(|b| *b == 0)
            .any(|var| var == needle.as_bytes())
        {
            // SAFETY: sending a signal to a PID has no memory-safety implications.
            if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
                killed += 1;
            }
        }
    }
    killed
}

async fn exec(name: &str, argv: &[String], timeout: Duration) -> Outcome {
    let token = format!("{:032x}", rand::random::<u128>());
    let mut command = tokio::process::Command::new("podman");
    command
        .arg("exec")
        .arg("--env")
        .arg(format!("ORQOS_PROBE={token}"))
        .arg(name)
        .args(argv)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Err(_) => {
            let killed = tokio::task::spawn_blocking(move || kill_tagged(&token))
                .await
                .unwrap_or(0);
            tracing::debug!(container = name, killed, "exec probe timed out");
            Outcome::Failure(format!("timed out after {}s", timeout.as_secs()))
        }
        Ok(Err(e)) => Outcome::Failure(format!("podman exec: {e}")),
        Ok(Ok(out)) if out.status.success() => Outcome::Success,
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let detail = stderr.lines().last().unwrap_or_default().trim();
            Outcome::Failure(format!(
                "exit status {}{}{}",
                out.status.code().unwrap_or(-1),
                if detail.is_empty() { "" } else { ": " },
                detail.chars().take(200).collect::<String>()
            ))
        }
    }
}

/// Connects to `127.0.0.1:<port>` inside the network namespace of `pid`. The namespace is entered by a fresh OS
/// thread, which ends right after, so no other task ever runs in it; the socket keeps the namespace it was made in.
pub async fn connect_in(
    pid: i32,
    port: u16,
    timeout: Duration,
) -> std::io::Result<tokio::net::TcpStream> {
    let stream = tokio::task::spawn_blocking(move || {
        std::thread::spawn(move || -> std::io::Result<std::net::TcpStream> {
            use std::os::fd::AsRawFd;
            let ns = std::fs::File::open(format!("/proc/{pid}/ns/net"))?;
            // SAFETY: setns on a valid namespace fd only changes this thread's network namespace.
            if unsafe { libc::setns(ns.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
            let stream = std::net::TcpStream::connect_timeout(&address, timeout)?;
            stream.set_nonblocking(true)?;
            Ok(stream)
        })
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("probe thread panicked")))
    })
    .await
    .map_err(std::io::Error::other)??;
    tokio::net::TcpStream::from_std(stream)
}

/// Accepts any server certificate: IR v2's `https` probes check that the endpoint answers, not who it is.
#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls() -> tokio_rustls::TlsConnector {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

async fn request<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut stream: S,
    http: &HttpProbe,
) -> Outcome {
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nUser-Agent: orqos-probe\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        http.path, http.port
    );
    if let Err(e) = stream.write_all(request.as_bytes()).await {
        return Outcome::Failure(format!("sending the request: {e}"));
    }
    let mut head = Vec::new();
    let mut buffer = [0u8; 512];
    while !head.windows(2).any(|w| w == b"\r\n") && head.len() < 4096 {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => head.extend_from_slice(&buffer[..n]),
            Err(e) => return Outcome::Failure(format!("reading the response: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&head);
    let line = text.split("\r\n").next().unwrap_or_default();
    match status_code(line) {
        Some(status) if http_success(status) => Outcome::Success,
        Some(status) => Outcome::Failure(format!("HTTP {status}")),
        None => Outcome::Failure("no HTTP status line".into()),
    }
}

/// One HTTP(S) probe through a stream already connected to the endpoint.
pub async fn http_over(stream: tokio::net::TcpStream, http: &HttpProbe) -> Outcome {
    match http.scheme {
        Scheme::Http => request(stream, http).await,
        Scheme::Https => {
            let name =
                rustls::pki_types::ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
            match tls().connect(name, stream).await {
                Ok(tls) => request(tls, http).await,
                Err(e) => Outcome::Failure(format!("TLS handshake: {e}")),
            }
        }
    }
}

async fn http(id: &str, http: &HttpProbe, timeout: Duration) -> Outcome {
    let attempt = async {
        let pid = match infra_pid(id).await {
            Ok(pid) => pid,
            Err(e) => return Outcome::Failure(e),
        };
        match connect_in(pid, http.port, timeout).await {
            Ok(stream) => http_over(stream, http).await,
            Err(e) => Outcome::Failure(format!("connecting to port {}: {e}", http.port)),
        }
    };
    match tokio::time::timeout(timeout, attempt).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Failure(format!("timed out after {}s", timeout.as_secs())),
    }
}

async fn run_probe(id: &str, container: &str, probe: &Probe) -> Outcome {
    let timeout = Duration::from_secs(probe.timeout_seconds.into());
    match (&probe.exec, &probe.http) {
        (Some(argv), _) => exec(&container_name(id, container), argv, timeout).await,
        (_, Some(h)) => http(id, h, timeout).await,
        _ => Outcome::Failure("no probe action".into()),
    }
}

/// Runs a pod's probes until cancelled.
pub async fn run(id: String, spec: PodSpec, states: States, cancel: CancellationToken) {
    let mut due: BTreeMap<(String, Kind), Instant> = BTreeMap::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        for c in spec.containers.iter().filter(|c| has_probes(c)) {
            let now = Instant::now();
            let probes = [
                (Kind::Startup, &c.startup),
                (Kind::Readiness, &c.readiness),
                (Kind::Liveness, &c.liveness),
            ];
            if !probes.iter().any(|(kind, probe)| {
                probe.is_some()
                    && due
                        .get(&(c.name.clone(), *kind))
                        .is_none_or(|at| *at <= now)
            }) {
                continue;
            }
            let key = (id.clone(), c.name.clone());
            let Some(current) = instance(&container_name(&id, &c.name)).await else {
                let mut state = states.entry(key).or_default();
                state.started = false;
                state.ready = false;
                continue;
            };
            {
                let mut state = states.entry(key.clone()).or_default();
                if state.instance != current {
                    let kept = (state.probe_restarts, state.last_probe_failure.take());
                    *state = ProbeState {
                        instance: current.clone(),
                        started: c.startup.is_none(),
                        ready: c.startup.is_none() && c.readiness.is_none(),
                        probe_restarts: kept.0,
                        last_probe_failure: kept.1,
                        ..Default::default()
                    };
                    due.retain(|(name, _), _| name != &c.name);
                }
            }
            let started = states.get(&key).is_some_and(|s| s.started);
            for (kind, probe) in probes {
                let Some(probe) = probe else { continue };
                // Before startup succeeds, only the startup probe runs; after, it never runs again.
                if (kind == Kind::Startup) == started {
                    continue;
                }
                let slot = (c.name.clone(), kind);
                if due.get(&slot).is_some_and(|at| *at > now) {
                    continue;
                }
                due.insert(slot, now + Duration::from_secs(probe.period_seconds.into()));
                let outcome = run_probe(&id, &c.name, probe).await;
                if cancel.is_cancelled() {
                    return;
                }
                let restart = {
                    let mut state = states.entry(key.clone()).or_default();
                    if state.instance != current {
                        break;
                    }
                    let restart = step(
                        &mut state,
                        kind,
                        outcome,
                        probe.failure_threshold,
                        c.readiness.is_some(),
                    );
                    if restart {
                        state.probe_restarts += 1;
                        state.started = false;
                        state.ready = false;
                        state.instance = String::new();
                    }
                    restart
                };
                if restart {
                    let name = container_name(&id, &c.name);
                    let grace = spec.grace_seconds.to_string();
                    tracing::info!(pod = %id, container = %c.name, probe = kind.name(), "probe failed; stopping the container");
                    if let Err(error) = units::run(
                        "podman",
                        &["stop", "--time", &grace, &name],
                        &[],
                        Duration::from_secs(u64::from(spec.grace_seconds) + 60),
                    )
                    .await
                    {
                        tracing::warn!(pod = %id, container = %c.name, %error, "stopping the container failed");
                    }
                    break;
                }
                // A startup success lets readiness and liveness run from the next tick.
                if kind == Kind::Startup {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> ProbeState {
        ProbeState::default()
    }

    #[test]
    fn startup_gates_and_restarts_at_its_threshold() {
        let mut s = fresh();
        assert!(!step(
            &mut s,
            Kind::Startup,
            Outcome::Failure("no".into()),
            3,
            true
        ));
        assert!(!step(
            &mut s,
            Kind::Startup,
            Outcome::Failure("no".into()),
            3,
            true
        ));
        assert_eq!(s.last_probe_failure.as_deref(), Some("startup: no"));
        assert!(step(
            &mut s,
            Kind::Startup,
            Outcome::Failure("no".into()),
            3,
            true
        ));
        let mut s = fresh();
        step(
            &mut s,
            Kind::Startup,
            Outcome::Failure("no".into()),
            3,
            true,
        );
        assert!(!step(&mut s, Kind::Startup, Outcome::Success, 3, true));
        assert!(
            s.started && !s.ready,
            "with a readiness probe, started is not ready"
        );
        let mut s = fresh();
        step(&mut s, Kind::Startup, Outcome::Success, 3, false);
        assert!(s.started && s.ready, "without one, started is ready");
    }

    #[test]
    fn readiness_flips_after_its_threshold_and_back_on_one_success() {
        let mut s = ProbeState {
            started: true,
            ..fresh()
        };
        assert!(!step(&mut s, Kind::Readiness, Outcome::Success, 2, true));
        assert!(s.ready);
        assert!(!step(
            &mut s,
            Kind::Readiness,
            Outcome::Failure("HTTP 503".into()),
            2,
            true
        ));
        assert!(s.ready, "one failure below the threshold keeps it ready");
        assert!(!step(
            &mut s,
            Kind::Readiness,
            Outcome::Failure("HTTP 503".into()),
            2,
            true
        ));
        assert!(!s.ready);
        assert!(!step(&mut s, Kind::Readiness, Outcome::Success, 2, true));
        assert!(s.ready);
    }

    #[test]
    fn liveness_restarts_only_after_consecutive_failures() {
        let mut s = ProbeState {
            started: true,
            ready: true,
            ..fresh()
        };
        assert!(!step(
            &mut s,
            Kind::Liveness,
            Outcome::Failure("x".into()),
            2,
            true
        ));
        assert!(!step(&mut s, Kind::Liveness, Outcome::Success, 2, true));
        assert!(!step(
            &mut s,
            Kind::Liveness,
            Outcome::Failure("x".into()),
            2,
            true
        ));
        assert!(step(
            &mut s,
            Kind::Liveness,
            Outcome::Failure("x".into()),
            2,
            true
        ));
        assert!(s.ready, "liveness never changes readiness");
    }

    #[test]
    fn timed_out_probe_processes_are_found_by_their_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .env("ORQOS_PROBE", token)
            .spawn()
            .unwrap();
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .env("ORQOS_PROBE", "another")
            .spawn()
            .unwrap();
        assert_eq!(kill_tagged(token), 1);
        assert!(!child.wait().unwrap().success());
        assert!(
            other.try_wait().unwrap().is_none(),
            "untagged processes live on"
        );
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn statuses_follow_kubernetes() {
        assert!(http_success(200) && http_success(302) && http_success(399));
        assert!(!http_success(199) && !http_success(404) && !http_success(503));
        assert_eq!(status_code("HTTP/1.1 503 Service Unavailable"), Some(503));
        assert_eq!(status_code("HTTP/1.0 200 OK"), Some(200));
        assert_eq!(status_code("SSH-2.0-OpenSSH"), None);
    }

    async fn serve_once(
        tls: Option<tokio_rustls::TlsAcceptor>,
        reply: &'static str,
    ) -> (u16, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            async fn handle<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
                mut s: S,
                reply: &str,
            ) -> String {
                let mut buffer = [0u8; 1024];
                let n = s.read(&mut buffer).await.unwrap();
                s.write_all(reply.as_bytes()).await.unwrap();
                s.shutdown().await.ok();
                String::from_utf8_lossy(&buffer[..n]).into_owned()
            }
            match tls {
                Some(acceptor) => handle(acceptor.accept(socket).await.unwrap(), reply).await,
                None => handle(socket, reply).await,
            }
        });
        (port, task)
    }

    fn probe(port: u16, scheme: Scheme) -> HttpProbe {
        HttpProbe {
            path: "/health/ready?full=1".into(),
            port,
            scheme,
        }
    }

    #[tokio::test]
    async fn http_probes_send_a_plain_get_and_read_the_status() {
        let (port, server) = serve_once(None, "HTTP/1.1 204 No Content\r\n\r\n").await;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        assert_eq!(
            http_over(stream, &probe(port, Scheme::Http)).await,
            Outcome::Success
        );
        let request = server.await.unwrap();
        assert!(
            request.starts_with("GET /health/ready?full=1 HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains(&format!("Host: 127.0.0.1:{port}\r\n")));

        let (port, _server) = serve_once(None, "HTTP/1.1 503 Unavailable\r\n\r\n").await;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        assert_eq!(
            http_over(stream, &probe(port, Scheme::Http)).await,
            Outcome::Failure("HTTP 503".into())
        );
    }

    // A self-signed P-256 certificate for "localhost", valid until 2126, used only by this test.
    const TEST_CERT: &str = include_str!("../../tests/fixtures/probe-test.crt");
    const TEST_KEY: &str = include_str!("../../tests/fixtures/probe-test.key");

    #[tokio::test]
    async fn https_probes_accept_any_certificate() {
        use rustls::pki_types::pem::PemObject;
        let certs = rustls::pki_types::CertificateDer::pem_slice_iter(TEST_CERT.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(TEST_KEY.as_bytes()).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let (port, server) = serve_once(Some(acceptor), "HTTP/1.1 200 OK\r\n\r\n").await;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        assert_eq!(
            http_over(stream, &probe(port, Scheme::Https)).await,
            Outcome::Success
        );
        assert!(server.await.unwrap().starts_with("GET /health/ready"));
    }
}
