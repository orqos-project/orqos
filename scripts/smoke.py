#!/usr/bin/env python3
"""Exercise the MVP against explicitly selected local backends; no third-party packages."""

import argparse
import base64
import contextlib
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
IMAGE = "alpine:3.22"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def report(message):
    print("PASS " + message, flush=True)


def command(arguments, timeout=30):
    result = subprocess.run(arguments, capture_output=True, text=True, timeout=timeout)
    require(result.returncode == 0, result.stderr.strip() or "Command failed: " + " ".join(arguments))
    return result.stdout.strip()


def wait_for(predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.2)
    raise AssertionError("Timed out waiting for test condition")


class WebSocket:
    def __init__(self, port, path):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.buffer = b""
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall((f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
                             "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                             f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        while b"\r\n\r\n" not in self.buffer:
            self.fill()
        headers, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        self.status = int(headers.split(b" ")[1])
        if self.status == 101:
            expected = base64.b64encode(hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
            require(b"sec-websocket-accept: " + expected.lower() in headers.lower(), "Invalid WS handshake")
        else:
            self.close()

    def fill(self):
        chunk = self.socket.recv(65536)
        if not chunk:
            raise EOFError("WebSocket closed")
        self.buffer += chunk

    def receive(self, timeout=10):
        self.socket.settimeout(timeout)
        # Retain the complete frame until parsed, including across socket timeouts.
        while len(self.buffer) < 2:
            self.fill()
        first, second = self.buffer[:2]
        length, offset = second & 127, 2
        if length in (126, 127):
            width = 2 if length == 126 else 8
            while len(self.buffer) < offset + width:
                self.fill()
            length = int.from_bytes(self.buffer[offset:offset + width], "big")
            offset += width
        require(not second & 128, "Server frames must be unmasked")
        require(first & 128, "Unexpected fragmented server frame")
        while len(self.buffer) < offset + length:
            self.fill()
        payload, self.buffer = self.buffer[offset:offset + length], self.buffer[offset + length:]
        return first & 15, payload.decode()

    def close(self):
        self.socket.close()


class Daemon:
    def __init__(self, binary, environment, directory):
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            self.port = probe.getsockname()[1]
        self.log = Path(directory) / ("daemon-" + uuid.uuid4().hex + ".log")
        self.output = self.log.open("w")
        self.process = subprocess.Popen([str(binary)], cwd=ROOT,
            env=dict(environment, BIND_ADDR=f"127.0.0.1:{self.port}"),
            stdout=self.output, stderr=subprocess.STDOUT)

    def http(self, method, path, payload=None, expected=200):
        data = None if payload is None else json.dumps(payload).encode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}" + path, data=data, method=method)
        if data is not None:
            request.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                status, body = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, body = error.code, error.read()
        require(status == expected, f"{method} {path}: expected {expected}, got {status}: {body[:500]!r}")
        return body

    def ready(self):
        def check():
            require(self.process.poll() is None, "Daemon exited: " + self.log.read_text())
            try:
                self.http("GET", "/api/openapi.json")
                return True
            except urllib.error.URLError:
                return False
        wait_for(check, 15)

    def close(self):
        forced = False
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGINT)
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                forced = True
                self.process.kill()
                self.process.wait()
        self.output.close()
        require(not forced, "Daemon required forced shutdown: " + self.log.read_text())


@contextlib.contextmanager
def running(binary, environment, directory):
    daemon = Daemon(binary, environment, directory)
    try:
        daemon.ready()
        yield daemon
    finally:
        daemon.close()


def startup_checks(binary, environment, directory, backends):
    missing_socket = str(Path(directory) / "missing.sock")
    # Reserve a port without listening, so the fake Kubernetes endpoint refuses connections.
    with socket.socket() as closed:
        closed.bind(("127.0.0.1", 0))
        fake = Path(directory) / "unreachable-kubeconfig.json"
        fake.write_text(json.dumps({"apiVersion": "v1", "kind": "Config",
            "clusters": [{"name": "fake", "cluster": {"server": f"http://127.0.0.1:{closed.getsockname()[1]}"}}],
            "contexts": [{"name": "fake", "context": {"cluster": "fake", "user": "fake"}}],
            "users": [{"name": "fake", "user": {}}], "current-context": "fake"}))
        scenarios = [(mode, {}, {backend: backend == mode or mode == "both" for backend in ("docker", "kube")})
                     for mode in backends + (["both"] if len(backends) == 2 else [])]
        for healthy in backends:
            overrides = {"KUBECONFIG": str(fake)} if healthy == "docker" else {"DOCKER_SOCKET": missing_socket}
            scenarios.append(("both", overrides, {"docker": healthy == "docker", "kube": healthy == "kube"}))
        for mode, overrides, available in scenarios:
            expected = 200 if not overrides else 503
            with running(binary, dict(environment, ORQOS_BACKENDS=mode, **overrides), directory) as daemon:
                health = json.loads(daemon.http("GET", "/healthz", expected=expected))
                for backend, route in [("docker", "/docker/containers"), ("kube", "/kube/pods")]:
                    enabled = mode == "both" or mode == backend
                    require(health[backend]["enabled"] == enabled, f"Incorrect enabled state: {health}")
                    require(health[backend]["available"] == available[backend], f"Incorrect reachability: {health}")
                    status = 200 if available[backend] else (503 if enabled else 404)
                    daemon.http("GET", route, expected=status)
                report(f"startup/health {mode} ({available})")
        for mode in backends + (["both"] if len(backends) == 2 else []):
            daemon = Daemon(binary, dict(environment, ORQOS_BACKENDS=mode,
                DOCKER_SOCKET=missing_socket, KUBECONFIG=str(fake)), directory)
            try:
                daemon.process.wait(timeout=12)
                require(daemon.process.returncode != 0, "Unreachable startup should fail")
                require("No enabled backend is reachable" in daemon.log.read_text(), "Missing startup diagnosis")
                report("rejects startup with no reachable backend: " + mode)
            finally:
                daemon.close()


def exercise(daemon, prefix, backend):
    def execute(cmd, expected=200, **options):
        return json.loads(daemon.http("POST", prefix + "/exec", dict(cmd=cmd, **options), expected))

    result = execute(["sh", "-c", "printf hello; printf problem >&2; exit 7"])
    require(result == {"stdout": "hello", "stderr": "problem", "exit_code": 7}, str(result))
    result = execute(["sh", "-c", "head -c 131072 /dev/zero >&2; printf done"])
    require(result["stdout"] == "done" and result["stderr"] == "\0" * 131072 and result["exit_code"] == 0,
            "Large stderr was not captured correctly")
    daemon.http("POST", prefix + "/exec", {"cmd": []}, expected=400)
    report(backend + " buffered exec: streams, exit 7, large stderr, empty command")

    for content, filename in [("audit", "upload"), ("", "empty"), ("snowman ☃\n\0end", "unicode")]:
        path = "/home/" + filename
        daemon.http("POST", prefix + "/write-file", dict(path=path, content=content, overwrite=False, mode="0600", owner="0:0"))
        require(daemon.http("POST", prefix + "/read-file", {"path": path}) == content.encode(), "File bytes differ")
        require(execute(["stat", "-c", "%a:%u:%g", path])["stdout"].strip() == "600:0:0", "Incorrect metadata")
        daemon.http("POST", prefix + "/write-file", dict(path=path, content="changed", overwrite=False), expected=409)
        require(daemon.http("POST", prefix + "/read-file", {"path": path}) == content.encode(), "Conflict changed file")
        daemon.http("POST", prefix + "/write-file", dict(path=path, content="changed", overwrite=True))
        require(daemon.http("POST", prefix + "/read-file", {"path": path}) == b"changed", "Overwrite failed")
    result = execute(["sh", "-c", "printf '\\000\\377audit' > /home/binary; mkdir /home/empty-dir; "
                       "ln -s /home/binary /home/link; ln /home/binary /home/hard-link"])
    require(result["exit_code"] == 0, str(result))
    require(daemon.http("POST", prefix + "/read-file", {"path": "/home/binary"}) == b"\0\xffaudit", "Binary bytes differ")
    for path, status in [("/home/link", 403), ("/home/empty-dir", 400), ("/home", 400),
                         ("/home/missing", 404), ("/etc/passwd", 403), ("/home/../etc/passwd", 400),
                         ("/home/..", 400), ("/home-other/file", 403), ("relative", 400)]:
        daemon.http("POST", prefix + "/read-file", {"path": path}, expected=status)
    for field, invalid in [("mode", "invalid"), ("owner", "orqos-user-does-not-exist")]:
        body = daemon.http("POST", prefix + "/write-file", dict(path="/home/invalid-" + field,
            content="audit", **{field: invalid}), expected=500)
        require(("chmod" if field == "mode" else "chown").encode() in body, "Missing permission failure detail")
    daemon.http("POST", prefix + "/write-file", {"path": "/home/..", "content": "bad"}, expected=400)
    report(backend + " files: exact bytes, empty, binary, overwrite, metadata, validation and permission failures")

    for raw in [json.dumps(["sh", "-c", "printf ws-out; printf ws-err >&2; exit 7"]), "[]", "echo", '["echo",7]']:
        ws = WebSocket(daemon.port, prefix + "/exec/ws?" + urllib.parse.urlencode({"cmd": raw}))
        try:
            if raw.startswith('["sh"'):
                require(ws.status == 101, "WS exec did not upgrade")
                output = {"stdout": "", "stderr": ""}
                while True:
                    opcode, text = ws.receive()
                    require(opcode == 1, "WS exec closed before exit code")
                    if text.startswith("__exit_code:"):
                        require(text == "__exit_code:7", "Incorrect WS exit code: " + text)
                        break
                    frame = json.loads(text)
                    output[frame["stream"]] += frame["data"]
                require(output == {"stdout": "ws-out", "stderr": "ws-err"}, str(output))
            else:
                require(ws.status == 400, "Invalid WS command should return 400")
        finally:
            ws.close()
    report(backend + " WebSocket exec: JSON query, streams, exit 7 and invalid queries")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backend", choices=["docker", "kube", "both"], default="both")
    parser.add_argument("--docker-context")
    parser.add_argument("--kube-context")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/orqos")
    args = parser.parse_args()
    backends = ["docker", "kube"] if args.backend == "both" else [args.backend]
    for backend in backends:
        if getattr(args, backend + "_context") is None:
            parser.error("--" + backend + "-context is required")
    binary = args.binary.resolve()
    require(binary.is_file(), "Build the binary first: cargo build --locked")
    run = "orqos-smoke-" + uuid.uuid4().hex[:12]
    docker = ["docker", "--context", args.docker_context] if "docker" in backends else None
    kube = ["kubectl", "--context", args.kube_context, "--request-timeout=15s"] if "kube" in backends else None
    environment = os.environ.copy()
    for key in ["DOCKER_SOCKET", "DOCKER_HOST", "KUBECONFIG", "ORQOS_BACKENDS"]:
        environment.pop(key, None)
    environment.update(KUBE_NAMESPACE=run, ORQOS_READ_BASE="/home", RUST_LOG="info")
    if docker:
        host = command(docker + ["context", "inspect", args.docker_context, "--format", "{{.Endpoints.docker.Host}}"])
        require(host.startswith("unix:///"), "Smoke tests require a local Unix Docker socket")
        environment["DOCKER_HOST"] = host
        command(docker + ["image", "inspect", IMAGE])
    with tempfile.TemporaryDirectory(prefix="orqos-smoke-") as directory:
        if kube:
            config = command(kube + ["config", "view", "--minify", "--raw", "--flatten", "-o", "json"])
            server = json.loads(config)["clusters"][0]["cluster"]["server"]
            require(urllib.parse.urlparse(server).hostname in ("127.0.0.1", "localhost", "::1"),
                    "Smoke tests require a loopback Kubernetes endpoint")
            kubeconfig = Path(directory) / "kubeconfig.json"
            kubeconfig.write_text(config)
            kubeconfig.chmod(0o600)
            environment["KUBECONFIG"] = str(kubeconfig)
        startup_checks(binary, environment, directory, backends)
        namespace_created = False
        container_created = False
        try:
            if kube:
                command(kube + ["create", "namespace", run])
                namespace_created = True
            with running(binary, dict(environment, ORQOS_BACKENDS=args.backend), directory) as daemon:
                spec = json.loads(daemon.http("GET", "/api/openapi.json"))
                for path in ["/healthz", "/metrics", "/docker/containers/{id}/exec/ws", "/kube/pods/{name}/exec/ws"]:
                    require(path in spec["paths"], "OpenAPI missing " + path)
                report("OpenAPI coverage")
                events = WebSocket(daemon.port, "/events/ws")
                require(events.status == 101, "Events did not upgrade")
                observed, errors, stopped = [], [], threading.Event()

                def collect():
                    while not stopped.is_set():
                        try:
                            opcode, text = events.receive(timeout=0.5)
                            if opcode == 1:
                                observed.append(json.loads(text))
                            elif opcode == 8:
                                break
                        except socket.timeout:
                            continue
                        except (OSError, EOFError):
                            break
                        except Exception as error:
                            errors.append(str(error))
                            break

                thread = threading.Thread(target=collect, daemon=True)
                thread.start()
                try:
                    time.sleep(1.2)  # Let the idle-aware event watchers subscribe.
                    if docker:
                        missing = {"name": run + "-missing", "image": "orqos-no-such-image:" + uuid.uuid4().hex}
                        daemon.http("POST", "/docker/containers", missing, expected=404)
                        daemon.http("POST", "/docker/containers", dict(name=run, image=IMAGE, cmd=[]), expected=400)
                        container_created = True  # Clean up even if creation succeeds but start fails.
                        created = json.loads(daemon.http("POST", "/docker/containers", dict(name=run, image=IMAGE,
                            cmd=["sleep", "600"], network="none", labels={"orqos.smoke": run})))
                        listed = json.loads(daemon.http("GET", "/docker/containers?" + urllib.parse.urlencode({"label": "orqos.smoke=" + run})))
                        require([item["Id"] for item in listed] == [created["id"]], "Docker label filter failed")
                        daemon.http("POST", "/docker/containers", dict(name=run, image=IMAGE), expected=409)
                        exercise(daemon, "/docker/containers/" + run, "docker")
                        daemon.http("POST", "/docker/containers/" + run + "/stop", {"t": 1}, expected=204)
                        daemon.http("POST", "/docker/containers/" + run + "/remove", expected=204)
                        daemon.http("POST", "/docker/containers/" + run + "/remove", expected=404)
                        wait_for(lambda: any(e.get("source") == "docker" and
                            e.get("event", {}).get("Actor", {}).get("Attributes", {}).get("name") == run for e in observed))
                        report("docker create/list/filter/stop/remove and events; missing image/name conflict")
                    if kube:
                        manifest = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "smoke", "namespace": run,
                            "labels": {"orqos-smoke": run}}, "spec": {"restartPolicy": "Never", "terminationGracePeriodSeconds": 1,
                            "containers": [{"name": "main", "image": IMAGE, "command": ["sleep", "600"],
                                "resources": {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "100m", "memory": "64Mi"}}}]}}
                        daemon.http("POST", "/kube/pods", manifest, expected=201)
                        command(kube + ["wait", "-n", run, "pod/smoke", "--for=condition=Ready", "--timeout=120s", "--request-timeout=125s"], timeout=130)
                        listed = json.loads(daemon.http("GET", "/kube/pods?" + urllib.parse.urlencode({"label_selector": "orqos-smoke=" + run})))
                        require([item["metadata"]["name"] for item in listed] == ["smoke"], "Pod label filter failed")
                        exercise(daemon, "/kube/pods/smoke", "kube")
                        wait_for(lambda: any(e.get("source") == "kube" and e.get("type") == "APPLIED" and
                            e["event"]["metadata"].get("namespace") == run for e in observed))
                        daemon.http("POST", "/kube/pods/smoke/delete", expected=204)
                        wait_for(lambda: any(e.get("source") == "kube" and e.get("type") == "DELETED" and
                            e["event"]["metadata"].get("namespace") == run for e in observed))
                        daemon.http("POST", "/kube/pods/smoke/delete", expected=404)
                        report("kube create/list/filter/delete and APPLIED/DELETED events")
                    require(not errors, "Event collector errors: " + str(errors))
                finally:
                    stopped.set()
                    events.close()
                    thread.join(timeout=2)
        finally:
            if container_created:
                # Require both identity and this run's label before removing a fixture.
                selector = ["ps", "-a", "--filter", "name=^/" + run + "$",
                            "--filter", "label=orqos.smoke=" + run, "-q"]
                remaining = command(docker + selector)
                if remaining:
                    command(docker + ["rm", "-f", remaining])
                require(not command(docker + selector), "Container cleanup failed")
            if namespace_created:
                command(kube + ["delete", "namespace", run, "--wait=false"])
                wait_for(lambda: not command(kube + ["get", "namespace", run, "--ignore-not-found", "-o", "name"]), 60)
            report("disposable resources and daemon processes cleaned up")
    report("all requested MVP smoke checks")


if __name__ == "__main__":
    main()
