#!/usr/bin/env python3
"""Live check of the workload backend: rootful Podman Quadlet pods managed through Orqos.

Requires root, systemd >= 254, Podman >= 5 and subordinate ID ranges for the `containers` user. Starts its own
Orqos (ORQOS_BACKENDS=workload) on a loopback port, so it can also restart the daemon. Everything it creates is
removed at the end, including after a failure.

    sudo python3 scripts/workload_smoke.py --orqos target/debug/orqos
"""
import argparse
import json
import os
import secrets
import subprocess
import sys
import time
import urllib.error
import urllib.request

IMAGE = "docker.io/library/alpine:3.22"
UNITS = "/etc/containers/systemd/orqos"
TOKEN = secrets.token_hex(32)
FOREIGN = "smoke-foreign"


class Orqos:
    def __init__(self, binary, port):
        self.binary, self.port, self.process = binary, port, None

    def start(self):
        env = {"PATH": "/usr/sbin:/usr/bin:/sbin:/bin", "ORQOS_BACKENDS": "workload",
               "ORQOS_WORKLOAD_TOKEN": TOKEN, "BIND_ADDR": f"127.0.0.1:{self.port}", "RUST_LOG": "info"}
        self.process = subprocess.Popen([self.binary], env=env, stdout=sys.stderr, stderr=sys.stderr)
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{self.port}/healthz", timeout=2)
                return
            except urllib.error.HTTPError:
                return
            except OSError:
                if self.process.poll() is not None:
                    raise SystemExit("Orqos exited during startup")
                time.sleep(0.5)
        raise SystemExit("Orqos did not start")

    def stop(self):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            self.process.wait(timeout=30)

    def call(self, method, path, body=None):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}", method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request, timeout=700) as response:
                return response.status, json.loads(response.read() or b"null")
        except urllib.error.HTTPError as error:
            return error.code, json.loads(error.read() or b"null")


def sh(*args, check=True):
    return subprocess.run(args, check=check, capture_output=True, text=True).stdout.strip()


def ok(condition, message):
    if not condition:
        raise AssertionError(message)
    print(f"ok - {message}", flush=True)


def wait_state(orqos, pod, want, seconds=120):
    deadline = time.time() + seconds
    status = None
    while time.time() < deadline:
        code, status = orqos.call("GET", f"/workload/pods/{pod}")
        if code == 200 and status["state"] == want:
            return status
        time.sleep(1)
    raise AssertionError(f"{pod} did not reach {want}: {json.dumps(status)}")


def container_id(name):
    return sh("podman", "inspect", "--format", "{{.Id}}", name)


SPEC = {
    "hostname": "smoke-0",
    "grace_seconds": 5,
    "containers": [
        {"name": "server", "image": IMAGE,
         "command": ["/bin/sh", "-c", "while true; do echo pong | nc -l -p 8080; done"]},
        {"name": "client", "image": IMAGE, "command": ["/bin/sh", "-c", "sleep infinity"],
         "env": {"GREETING": "hello $USER 100%"}},
    ],
}


def run(orqos):
    code, body = orqos.call("GET", "/workload/capabilities")
    ok(code == 200 and body["problems"] == [], f"capabilities satisfied: {body}")
    unauth = urllib.request.Request(f"http://127.0.0.1:{orqos.port}/workload/pods")
    try:
        urllib.request.urlopen(unauth, timeout=5)
        ok(False, "requests without the bearer token are refused")
    except urllib.error.HTTPError as error:
        ok(error.code == 401, "requests without the bearer token are refused")

    # A Podman pod that Orqos does not own blocks a pod of the same name.
    sh("podman", "pod", "create", "--name", "orqos-taken", "--label", "owner=someone-else")
    code, body = orqos.call("PUT", "/workload/pods/taken", SPEC)
    ok(code == 409, f"an unowned Podman pod with the same name is not adopted ({code} {body})")
    sh("podman", "pod", "create", "--name", FOREIGN)

    code, body = orqos.call("PUT", "/workload/pods/smoke", SPEC)
    ok(code == 200, f"apply creates the pod ({code} {body.get('error') if isinstance(body, dict) else body})")
    wait_state(orqos, "smoke", "running")
    ok(sh("podman", "exec", "orqos-smoke-client", "hostname") == "smoke-0", "hostname comes from the spec")
    ok(sh("podman", "exec", "orqos-smoke-client", "nc", "127.0.0.1", "8080") == "pong",
       "containers share the pod's network namespace (client reaches server on 127.0.0.1)")
    ok(sh("podman", "exec", "orqos-smoke-client", "printenv", "GREETING") == "hello $USER 100%",
       "environment values arrive literally")
    uid_map = sh("podman", "exec", "orqos-smoke-client", "cat", "/proc/self/uid_map").split()
    ok(uid_map[0] == "0" and uid_map[1] != "0", f"UserNS=auto maps container root away from host root ({uid_map[:3]})")
    labels = json.loads(sh("podman", "pod", "inspect", "orqos-smoke", "--format", "json"))
    labels = (labels[0] if isinstance(labels, list) else labels)["Labels"]
    ok("dev.orqos.owner" in labels and labels["dev.orqos.pod"] == "smoke", "the Podman pod carries ownership labels")

    before = {c: container_id(f"orqos-smoke-{c}") for c in ("server", "client")}
    code, _ = orqos.call("PUT", "/workload/pods/smoke", SPEC)
    ok(code == 200 and before == {c: container_id(f"orqos-smoke-{c}") for c in before},
       "re-applying an unchanged spec keeps the containers")

    changed = json.loads(json.dumps(SPEC))
    changed["containers"][1]["env"]["EXTRA"] = "1"
    code, _ = orqos.call("PUT", "/workload/pods/smoke", changed)
    wait_state(orqos, "smoke", "running")
    ok(code == 200 and container_id("orqos-smoke-client") != before["client"], "a changed spec replaces the containers")

    sh("podman", "kill", "orqos-smoke-server")
    time.sleep(2)
    status = wait_state(orqos, "smoke", "running")
    server = next(c for c in status["containers"] if c["name"] == "server")
    ok(server["restarts"] >= 1, f"a killed container is restarted by systemd (restarts={server['restarts']})")

    code, status = orqos.call("POST", "/workload/pods/smoke/stop")
    ok(code == 200 and status["state"] == "stopped" and subprocess.run(
        ["podman", "pod", "exists", "orqos-smoke"]).returncode != 0, "stop stops the pod and removes it from Podman")
    code, _ = orqos.call("POST", "/workload/pods/smoke/start")
    wait_state(orqos, "smoke", "running")
    ok(code == 200, "start brings it back")

    client = container_id("orqos-smoke-client")
    orqos.stop()
    time.sleep(2)
    ok(sh("podman", "inspect", "--format", "{{.State.Running}}", "orqos-smoke-client") == "true",
       "the pod keeps running while Orqos is stopped")
    orqos.start()
    status = wait_state(orqos, "smoke", "running")
    ok(container_id("orqos-smoke-client") == client, "a restarted Orqos reports the same running pod without changing it")

    token_dir = os.path.join(UNITS, json.load(open("/var/lib/orqos/workload/smoke.json"))["token"])
    code, body = orqos.call("DELETE", "/workload/pods/smoke")
    ok(code == 200 and not os.path.exists(token_dir), "remove deletes the pod's units")
    ok(subprocess.run(["podman", "pod", "exists", "orqos-smoke"]).returncode != 0, "remove leaves no Podman pod")
    code, _ = orqos.call("GET", "/workload/pods/smoke")
    ok(code == 404, "remove deletes the record")
    ok(subprocess.run(["podman", "pod", "exists", FOREIGN]).returncode == 0
       and subprocess.run(["podman", "pod", "exists", "orqos-taken"]).returncode == 0,
       "pods Orqos does not own are untouched")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--orqos", required=True, help="path to the orqos binary")
    parser.add_argument("--port", type=int, default=9099)
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise SystemExit("run as root: the workload backend drives rootful Podman")
    sh("podman", "pull", "-q", IMAGE)
    orqos = Orqos(os.path.abspath(args.orqos), args.port)
    orqos.start()
    try:
        run(orqos)
        print("workload smoke: all checks passed")
    finally:
        try:
            if orqos.process.poll() is None:
                orqos.call("DELETE", "/workload/pods/smoke")
        except Exception:  # cleanup must continue
            pass
        orqos.stop()
        for pod in (FOREIGN, "orqos-taken", "orqos-smoke"):
            subprocess.run(["podman", "pod", "rm", "--force", "--ignore", pod], capture_output=True)


if __name__ == "__main__":
    main()
