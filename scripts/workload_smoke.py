#!/usr/bin/env python3
"""Live check of the workload backend: rootful Podman Quadlet pods managed through Orqos.

Requires root, systemd >= 254, Podman >= 5 and subordinate ID ranges for the `containers` user. Starts its own
Orqos (ORQOS_BACKENDS=workload) on a loopback port, so it can also restart the daemon. Everything it creates is
removed at the end, including after a failure.

    sudo python3 scripts/workload_smoke.py --orqos target/debug/orqos
"""
import argparse
import base64
import json
import os
import re
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
    "labels": {"example.org/app": "smoke"},
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
    ok(labels.get("example.org/app") == "smoke", "the Podman pod carries the spec's labels")

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


def net_spec(hostname, network, aliases):
    return {"hostname": hostname, "grace_seconds": 5, "network": {"name": network, "aliases": aliases},
            "containers": [{"name": "server", "image": IMAGE,
                            "command": ["/bin/sh", "-c", "while true; do hostname | nc -l -p 8080; done"]}]}


def resolve(container, name):
    """IPv4 addresses `name` resolves to inside the container, or None when it does not resolve."""
    result = subprocess.run(["podman", "exec", container, "getent", "ahostsv4", name], capture_output=True, text=True)
    return None if result.returncode else {line.split()[0] for line in result.stdout.splitlines()}


def networks(orqos):
    """Pods on an owned network resolve each other's aliases; other networks cannot; the network goes with its
    last pod."""
    sh("podman", "network", "create", "--label", "owner=someone-else", "orqos-net-taken")
    code, body = orqos.call("PUT", "/workload/pods/nettaken", net_spec("t-0", "taken", []))
    ok(code == 409, f"an unowned Podman network with the same name is not adopted ({code} {body})")

    pods = {
        "web0": net_spec("web-0", "smoke", ["web", "web.ns", "web.ns.svc", "web-0.web"]),
        "web1": net_spec("web-1", "smoke", ["web", "web.ns.svc", "web-1.web"]),
        "other": net_spec("other-0", "smoke-other", ["web-elsewhere"]),
    }
    for pod, spec in pods.items():
        code, body = orqos.call("PUT", f"/workload/pods/{pod}", spec)
        ok(code == 200, f"apply creates {pod} on network {spec['network']['name']} ({code} {body if code != 200 else ''})")
    for pod in pods:
        wait_state(orqos, pod, "running")

    labels = json.loads(sh("podman", "network", "inspect", "orqos-net-smoke", "--format", "json"))
    labels = (labels[0] if isinstance(labels, list) else labels).get("labels", {})
    ok("dev.orqos.owner" in labels and labels.get("dev.orqos.network") == "smoke",
       f"the Podman network carries ownership labels ({labels})")

    web0, web1 = resolve("orqos-web1-server", "web-0.web"), resolve("orqos-web1-server", "web-1.web")
    ok(web0 and web1 and len(web0) == 1 and len(web1) == 1 and web0 != web1,
       f"per-pod aliases resolve to one address each ({web0}, {web1})")
    ok(resolve("orqos-web1-server", "web") == web0 | web1, "a shared alias resolves to every pod that has it")
    ok(resolve("orqos-web0-server", "web.ns.svc") == web0 | web1, "dotted aliases resolve (web.ns.svc)")
    ok(resolve("orqos-web1-server", "web.ns") == web0, "an alias resolves only to the pods that have it")
    ok(sh("podman", "exec", "orqos-web1-server", "nc", "web-0.web", "8080") == "web-0",
       "a pod reaches another by alias over the network")
    ok(resolve("orqos-other-server", "web") is None and resolve("orqos-other-server", "web-0.web") is None,
       "a pod on another network cannot resolve them")
    web0_ip = next(iter(web0))
    reach = subprocess.run(["podman", "exec", "orqos-other-server", "timeout", "5", "nc", web0_ip, "8080"],
                           capture_output=True, text=True)
    ok(reach.stdout.strip() != "web-0", f"a pod on another network cannot reach them by IP either ({web0_ip})")
    sh("podman", "run", "-d", "--name", "smoke-default-client", IMAGE, "sleep", "300")
    reach = subprocess.run(["podman", "exec", "smoke-default-client", "timeout", "5", "nc", web0_ip, "8080"],
                           capture_output=True, text=True)
    ok(reach.stdout.strip() != "web-0", "a container on Podman's default network cannot reach them by IP")
    sh("podman", "rm", "-f", "-t", "0", "smoke-default-client")
    options = json.loads(sh("podman", "network", "inspect", "orqos-net-smoke", "--format", "json"))
    options = (options[0] if isinstance(options, list) else options).get("options", {})
    ok(options.get("isolate") == "strict", f"owned networks are created strictly isolated ({options})")

    code, listed = orqos.call("GET", "/workload/networks")
    smoke = next((n for n in listed if n["name"] == "smoke"), None) if code == 200 else None
    ok(smoke and smoke["exists"] and smoke["pods"] == ["web0", "web1"] and smoke["podman_name"] == "orqos-net-smoke",
       f"the networks endpoint lists the network and its pods ({listed})")

    exists = lambda name: subprocess.run(["podman", "network", "exists", name]).returncode == 0
    orqos.call("DELETE", "/workload/pods/web0")
    ok(exists("orqos-net-smoke"), "the network stays while a pod still names it")
    orqos.call("DELETE", "/workload/pods/web1")
    orqos.call("DELETE", "/workload/pods/other")
    ok(not exists("orqos-net-smoke") and not exists("orqos-net-smoke-other"),
       "removing a network's last pod removes the network")
    code, listed = orqos.call("GET", "/workload/networks")
    ok(code == 200 and listed == [], f"no owned networks remain ({listed})")
    ok(exists("orqos-net-taken"), "networks Orqos does not own are untouched")


def mountpoint(volume):
    return sh("podman", "volume", "inspect", volume, "--format", "{{.Mountpoint}}")


def owner_mode(path):
    st = os.stat(path)
    return f"{st.st_uid}:{st.st_gid} {oct(st.st_mode & 0o7777)[2:]}"


def storage_spec(level, user=1000):
    return {"hostname": "st-0", "grace_seconds": 5, "supplementary_groups": [2000],
            "volumes": [{"name": "data", "volume": "smoke-data"},
                        {"name": "mem", "scratch": {"medium": "memory", "bytes": 1048576}},
                        {"name": "disk", "scratch": {"medium": "disk", "bytes": 1048576}}],
            "containers": [{"name": "app", "image": IMAGE, "user": user, "group": user,
                            "read_only_root": True, "no_new_privileges": True, "drop_all_capabilities": True,
                            "command": ["/bin/sh", "-c", "trap 'exit 0' TERM; while true; do sleep 1; done"],
                            "env": {"LEVEL": level},
                            "mounts": [{"volume": "data", "path": "/data"}, {"volume": "mem", "path": "/mem"},
                                       {"volume": "disk", "path": "/disk"},
                                       {"volume": "data", "path": "/ro", "read_only": True}]}]}


def storage(orqos):
    """Owned volumes keep data across replacement with container-relative ownership; scratch starts empty; the
    container runs as the given user, read-only, without capabilities or privilege escalation."""
    code, body = orqos.call("PUT", "/workload/volumes/smoke-data", {"group": 2000, "labels": {"example.org/k": "v"}})
    ok(code == 200 and body["spec"]["group"] == 2000, f"PUT creates an owned volume ({code} {body})")
    data = mountpoint("orqos-vol-smoke-data")
    ok(owner_mode(data) == "0:2000 2775", f"its root is root:2000, setgid group-writable ({owner_mode(data)})")
    labels = json.loads(sh("podman", "volume", "inspect", "orqos-vol-smoke-data", "--format", "json"))[0]["Labels"]
    ok("dev.orqos.owner" in labels and labels.get("dev.orqos.volume") == "smoke-data",
       f"the Podman volume carries ownership labels ({labels})")

    sh("podman", "volume", "create", "--label", "owner=someone-else", "orqos-vol-taken")
    code, body = orqos.call("PUT", "/workload/volumes/taken", {})
    ok(code == 409, f"an unowned Podman volume with the same name is not adopted ({code} {body})")
    unknown = storage_spec("a")
    unknown["volumes"][0]["volume"] = "missing"
    code, body = orqos.call("PUT", "/workload/pods/st-unknown", unknown)
    ok(code == 422 and "unknown volume" in body["error"], f"a pod naming an unknown volume is refused ({code} {body})")

    code, body = orqos.call("PUT", "/workload/pods/st", storage_spec("a"))
    ok(code == 200, f"apply creates a pod with volumes and hardening ({code} {body.get('error') if isinstance(body, dict) else body})")
    wait_state(orqos, "st", "running")
    c = "orqos-st-app"
    out = sh("podman", "exec", c, "sh", "-c",
             "id; grep -E 'CapEff|NoNewPrivs' /proc/self/status; echo one > /data/a && echo m > /mem/m && "
             "echo d > /disk/d && echo wrote; touch /x 2>/dev/null || echo read-only; "
             "touch /ro/x 2>/dev/null || echo ro-mount; head -c 2000000 /dev/zero > /mem/big 2>/dev/null || echo mem-full; "
             "rm -f /mem/big")
    ident = out.splitlines()[0]
    ok(re.search(r"uid=1000\b", ident) and re.search(r"gid=1000\b", ident)
       and re.search(r"[=,]2000\b", ident.split("groups=")[1]), f"runs as 1000:1000 with group 2000 ({ident})")
    ok("CapEff:\t0000000000000000" in out and "NoNewPrivs:\t1" in out, "no capabilities and no privilege escalation")
    ok("wrote" in out and "read-only" in out and "ro-mount" in out,
       "volumes are writable while the root filesystem and read-only mounts are not")
    ok("mem-full" in out, "memory scratch enforces its size")
    ok(owner_mode(os.path.join(data, "a")).split()[0] == "1000:2000",
       f"files keep container-relative ownership on disk, group from the setgid root ({owner_mode(os.path.join(data, 'a'))})")

    other = storage_spec("x", user=1001)
    code, _ = orqos.call("PUT", "/workload/pods/st2", other)
    wait_state(orqos, "st2", "running")
    ok(code == 200 and sh("podman", "exec", "orqos-st2-app", "sh", "-c", "cat /data/a && echo two > /data/b") == "one",
       "another pod (another user and ID range, same group) reads and writes the volume")
    code, body = orqos.call("GET", "/workload/volumes")
    listed = next((v for v in body if v["name"] == "smoke-data"), {}) if code == 200 else {}
    ok(listed.get("pods") == ["st", "st2"] and listed.get("spec", {}).get("labels") == {"example.org/k": "v"},
       f"the volumes endpoint lists the volume, its labels and its pods ({listed})")
    code, _ = orqos.call("DELETE", "/workload/volumes/smoke-data")
    ok(code == 409, "a mounted volume cannot be deleted")

    code, _ = orqos.call("PUT", "/workload/pods/st", storage_spec("b"))
    wait_state(orqos, "st", "running")
    out = sh("podman", "exec", c, "sh", "-c", "cat /data/a /data/b; find /mem /disk -mindepth 1 | wc -l")
    ok(code == 200 and out.split() == ["one", "two", "0"],
       f"after replacement the volume keeps its data and scratch volumes start empty ({out.split()})")
    sh("podman", "exec", c, "sh", "-c", "echo kept > /mem/k")
    sh("podman", "kill", c)
    time.sleep(2)
    wait_state(orqos, "st", "running")
    ok(sh("podman", "exec", c, "cat", "/mem/k") == "kept", "memory scratch survives a container restart")

    for pod in ("st", "st2"):
        orqos.call("DELETE", f"/workload/pods/{pod}")
    exists = lambda name: subprocess.run(["podman", "volume", "exists", name]).returncode == 0
    ok(not exists("orqos-st-scratch-mem") and not exists("orqos-st-scratch-disk"), "removing a pod removes its scratch volumes")
    ok(exists("orqos-vol-smoke-data"), "owned volumes outlive their pods")
    code, _ = orqos.call("DELETE", "/workload/volumes/smoke-data")
    ok(code == 200 and not exists("orqos-vol-smoke-data"), "DELETE removes an unused owned volume")
    ok(exists("orqos-vol-taken"), "volumes Orqos does not own are untouched")


FILES = "/var/lib/orqos/workload/files"
PASSWORD = "p a$$ \"w%rd\" #1  "


def b64(text):
    return base64.b64encode(text.encode()).decode()


def secret_spec(password_secret="smoke-db"):
    return {"hostname": "sec-0", "grace_seconds": 5, "supplementary_groups": [2000],
            "volumes": [{"name": "tls", "secret": {"name": "smoke-tls", "mode": 0o440, "group": 2000}},
                        {"name": "conf", "secret": {"name": "smoke-tls", "group": 2000,
                                                    "items": [{"key": "tls.crt", "path": "certs/ca.pem"}]}}],
            "containers": [{"name": "app", "image": IMAGE, "user": 1000, "group": 1000, "read_only_root": True,
                            "command": ["/bin/sh", "-c", "trap 'exit 0' TERM; while true; do sleep 1; done"],
                            "env": {"LEVEL": "info"},
                            "secret_env": {"DB_PASSWORD": {"secret": password_secret, "key": "password"}},
                            "mounts": [{"volume": "tls", "path": "/etc/apk"},
                                       {"volume": "conf", "path": "/opt/app/ca.pem", "sub_path": "certs/ca.pem"}]}]}


def secret_files(orqos):
    """Secrets: values never come back; files are written root:group with their mode, bind-mounted read-only and
    idmapped (replacing the directory); environment values arrive verbatim; data is immutable."""
    code, body = orqos.call("PUT", "/workload/secrets/smoke-tls",
                            {"data": {"tls.crt": b64("CERT"), "tls.key": b64("KEY")}, "labels": {"example.org/k": "v"}})
    ok(code == 200 and body["keys"] == ["tls.crt", "tls.key"], f"PUT creates a secret and lists its keys ({code} {body})")
    code, body = orqos.call("PUT", "/workload/secrets/smoke-db", {"data": {"password": b64(PASSWORD)}})
    ok(code == 200, f"a second secret ({code} {body})")
    code, body = orqos.call("PUT", "/workload/secrets/smoke-tls", {"data": {"tls.crt": b64("OTHER"), "tls.key": b64("KEY")}})
    ok(code == 409, f"changing a secret's data is refused ({code} {body})")
    code, body = orqos.call("PUT", "/workload/secrets/smoke-tls",
                            {"data": {"tls.crt": b64("CERT"), "tls.key": b64("KEY")}, "labels": {"example.org/k": "w"}})
    ok(code == 200 and body["labels"] == {"example.org/k": "w"}, f"labels can change ({code} {body})")
    missing = secret_spec("smoke-missing")
    code, body = orqos.call("PUT", "/workload/pods/sec-missing", missing)
    ok(code == 422 and "unknown secret" in body["error"], f"a pod naming an unknown secret is refused ({code} {body})")
    code, body = orqos.call("PUT", "/workload/secrets/smoke-bad", {"data": {"password": b64("line\nbreak")}})
    bad = secret_spec("smoke-bad")
    code, body = orqos.call("PUT", "/workload/pods/sec-bad", bad)
    ok(code == 422 and "control characters" in body["error"],
       f"a value with a newline cannot become an environment variable ({code} {body})")

    code, body = orqos.call("PUT", "/workload/pods/sec", secret_spec())
    ok(code == 200, f"apply creates a pod with secret files and environment ({code} {body.get('error') if isinstance(body, dict) else body})")
    wait_state(orqos, "sec", "running")
    out = sh("podman", "exec", "orqos-sec-app", "sh", "-c",
             "stat -c '%u %g %a %n' /etc/apk /etc/apk/tls.key /opt/app/ca.pem; ls /etc/apk | tr '\\n' ' '; echo; "
             "cat /etc/apk/tls.key /opt/app/ca.pem; echo; printf '[%s]' \"$DB_PASSWORD\"; echo; "
             "touch /etc/apk/x 2>/dev/null || echo read-only")
    lines = out.splitlines()
    ok(lines[0] == "0 2000 550 /etc/apk" and lines[1] == "0 2000 440 /etc/apk/tls.key"
       and lines[2] == "0 2000 644 /opt/app/ca.pem",
       f"files and directories are root:2000 with their modes inside the pod ({lines[:3]})")
    ok(lines[3].split() == ["tls.crt", "tls.key"], f"a secret volume replaces the image's directory ({lines[3]})")
    ok(lines[4] == "KEYCERT", f"user 1000 with the group reads the files, including a sub_path file ({lines[4]})")
    ok(lines[5] == f"[{PASSWORD}]", f"the environment value arrives verbatim ({lines[5]})")
    ok(lines[6] == "read-only", "secret files are read-only")
    denied = subprocess.run(["podman", "run", "--rm", "--userns=auto:size=65536", "--user", "1001", "--mount",
                             f"type=bind,source={FILES}/sec/tls,destination=/t,ro,idmap", IMAGE, "cat", "/t/tls.key"],
                            capture_output=True)
    ok(denied.returncode != 0, "a container without the group cannot read 0440 files")
    on_disk = os.path.join(FILES, "sec", "tls", "tls.key")
    ok(owner_mode(on_disk) == "0:2000 440" and owner_mode(FILES) == "0:0 700",
       f"on disk the files are root:2000 under a root-only directory ({owner_mode(on_disk)}, {owner_mode(FILES)})")

    code, pod = orqos.call("GET", "/workload/pods/sec")
    code2, listed = orqos.call("GET", "/workload/secrets")
    leaked = [text for text in (json.dumps(pod), json.dumps(listed)) if "KEY" in text.replace("tls.key", "") or "w%rd" in text]
    ok(code == 200 and code2 == 200 and not leaked, "values appear in neither the pod nor the secrets listing")
    tls = next((s for s in listed if s["name"] == "smoke-tls"), {})
    ok(tls.get("pods") == ["sec"], f"the secrets endpoint lists the pods using a secret ({tls})")
    code, _ = orqos.call("DELETE", "/workload/secrets/smoke-tls")
    ok(code == 409, "a secret in use cannot be deleted")

    orqos.call("DELETE", "/workload/pods/sec")
    ok(not os.path.exists(os.path.join(FILES, "sec")), "removing the pod removes its secret files")
    for name in ("smoke-tls", "smoke-db", "smoke-bad"):
        code, _ = orqos.call("DELETE", f"/workload/secrets/{name}")
        ok(code == 200, f"DELETE removes the unused secret {name}")


RESPONDER = ("while true; do if [ -f /tmp/ready ]; then s='200 OK'; else s='503 Service Unavailable'; fi; "
             "printf \"HTTP/1.1 $s\\r\\nContent-Length: 0\\r\\nConnection: close\\r\\n\\r\\n\" | nc -l -p 8080; done")
TLS_SERVER = ("apk add --no-cache openssl >/dev/null 2>&1 && openssl req -x509 -newkey ec -pkeyopt "
              "ec_paramgen_curve:P-256 -nodes -keyout /tmp/k -out /tmp/c -subj /CN=x -days 1 2>/dev/null && "
              "exec openssl s_server -quiet -www -accept 8443 -cert /tmp/c -key /tmp/k")


def probe(action, period=1, timeout=1, threshold=1):
    return {**action, "period_seconds": period, "timeout_seconds": timeout, "failure_threshold": threshold}


def container_state(orqos, pod, name):
    code, status = orqos.call("GET", f"/workload/pods/{pod}")
    assert code == 200, status
    return status, next(c for c in status["containers"] if c["name"] == name)


def until(description, check, seconds=120):
    deadline = time.time() + seconds
    last = None
    while time.time() < deadline:
        last = check()
        if last:
            return last
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {description}")


def probes(orqos):
    """Startup gates readiness and liveness; readiness follows HTTP(S) answers from inside the pod's network
    namespace; failing liveness restarts the container under its restart policy; timed-out exec probes leave no
    process behind."""
    spec = {"hostname": "pr-0", "grace_seconds": 2, "containers": [
        {"name": "app", "image": IMAGE, "command": ["/bin/sh", "-c", RESPONDER],
         "startup": probe({"exec": ["test", "-f", "/tmp/started"]}, threshold=120),
         "readiness": probe({"http": {"path": "/ready", "port": 8080, "scheme": "http"}}),
         "liveness": probe({"exec": ["/bin/sh", "-c", "test ! -f /tmp/dead"]}, threshold=2)},
        {"name": "tls", "image": IMAGE, "command": ["/bin/sh", "-c", TLS_SERVER],
         "readiness": probe({"http": {"path": "/", "port": 8443, "scheme": "https"}}, period=2, timeout=2)},
        {"name": "slow", "image": IMAGE, "command": ["sleep", "infinity"],
         "readiness": probe({"exec": ["sleep", "30"]}, period=3, timeout=1)},
    ]}
    code, body = orqos.call("PUT", "/workload/pods/pr", spec)
    ok(code == 200, f"apply creates a pod with probes ({code} {body.get('error') if isinstance(body, dict) else ''})")
    wait_state(orqos, "pr", "running")
    status, app = container_state(orqos, "pr", "app")
    ok(not app["started"] and not app["ready"] and not status["ready"],
       f"before its startup probe passes a container is neither started nor ready ({app})")
    sh("podman", "exec", "orqos-pr-app", "touch", "/tmp/started")
    app = until("startup", lambda: (lambda c: c if c["started"] else None)(container_state(orqos, "pr", "app")[1]))
    time.sleep(2)
    _, app = container_state(orqos, "pr", "app")
    ok(app["started"] and not app["ready"] and "HTTP 503" in (app["last_probe_failure"] or ""),
       f"once started, readiness follows the HTTP answer from inside the pod ({app})")
    sh("podman", "exec", "orqos-pr-app", "touch", "/tmp/ready")
    until("app readiness", lambda: container_state(orqos, "pr", "app")[1]["ready"])
    tls = until("HTTPS readiness", lambda: (lambda c: c if c["ready"] else None)(container_state(orqos, "pr", "tls")[1]),
                seconds=180)
    ok(tls["ready"], "an HTTPS probe passes against a self-signed certificate")
    _, slow = container_state(orqos, "pr", "slow")
    ok(not slow["ready"] and "timed out" in (slow["last_probe_failure"] or ""), f"a hanging exec probe times out ({slow})")
    time.sleep(1)
    left = sh("podman", "exec", "orqos-pr-slow", "ps", "-o", "args")
    ok("sleep 30" not in left, f"timed-out exec probes leave no process behind ({left.splitlines()})")
    status, _ = container_state(orqos, "pr", "app")
    ok(not status["ready"], "a pod is ready only when every container is")

    sh("podman", "exec", "orqos-pr-app", "rm", "/tmp/ready")
    until("unreadiness", lambda: not container_state(orqos, "pr", "app")[1]["ready"])
    _, app = container_state(orqos, "pr", "app")
    ok(app["restarts"] == 0 and app["probe_restarts"] == 0,
       "failing readiness makes a container unready without restarting it")

    before = container_id("orqos-pr-app")
    sh("podman", "exec", "orqos-pr-app", "touch", "/tmp/dead")
    app = until("a liveness restart", lambda: (lambda c: c if c["probe_restarts"] >= 1 else None)(
        container_state(orqos, "pr", "app")[1]))
    until("the restarted container", lambda: subprocess.run(
        ["podman", "inspect", "--format", "{{.State.Running}}", "orqos-pr-app"],
        capture_output=True, text=True).stdout.strip() == "true")
    ok(container_id("orqos-pr-app") != before and "liveness" in app["last_probe_failure"],
       f"failing liveness restarts the container ({app['probe_restarts']} probe restarts)")
    time.sleep(2)
    _, app = container_state(orqos, "pr", "app")
    ok(not app["started"], "a restarted container starts over: not started until its startup probe passes")

    never = {"hostname": "pn-0", "grace_seconds": 2, "restart": "never", "containers": [
        {"name": "app", "image": IMAGE, "command": ["sleep", "infinity"],
         "liveness": probe({"exec": ["false"]})}]}
    code, _ = orqos.call("PUT", "/workload/pods/pn", never)
    ok(code == 200, "apply creates a pod with restart: never")
    until("the never-restart liveness stop", lambda: container_state(orqos, "pn", "app")[1]["probe_restarts"] >= 1)
    time.sleep(4)
    status, app = container_state(orqos, "pn", "app")
    ok(app["active_state"] != "active" and not status["ready"],
       f"with restart: never a failed liveness probe leaves the container down ({app['active_state']})")
    for pod in ("pr", "pn"):
        orqos.call("DELETE", f"/workload/pods/{pod}")


def unit_state(unit, prop="ActiveState"):
    return sh("systemctl", "show", "-p", prop, "--value", unit)


def init_and_sidecars(orqos):
    """Init containers run once, in order, retried in place; a sidecar's startup probe gates the containers; sidecars
    stop after the containers; restart: never with a failing init leaves the pod failed."""
    work = [{"volume": "work", "path": "/work"}]
    flaky = "n=$(cat /work/n 2>/dev/null || echo 0); echo $((n+1)) > /work/n; [ $n -ge 2 ]"
    sleep = "trap 'exit 0' TERM; while true; do sleep 1; done"
    spec = {"hostname": "is-0", "grace_seconds": 2,
            "volumes": [{"name": "work", "scratch": {"medium": "memory", "bytes": 1048576}}],
            "init": [{"name": "one", "image": IMAGE, "command": ["/bin/sh", "-c", "echo -n 1 > /work/order"], "mounts": work},
                     {"name": "two", "image": IMAGE, "command": ["/bin/sh", "-c", "echo -n 2 >> /work/order"], "mounts": work},
                     {"name": "flaky", "image": IMAGE, "command": ["/bin/sh", "-c", flaky], "mounts": work}],
            "sidecars": [{"name": "side", "image": IMAGE, "mounts": work,
                          "command": ["/bin/sh", "-c", f"sleep 3; date +%s > /work/token; {sleep}"],
                          "startup": probe({"exec": ["test", "-s", "/work/token"]}, threshold=60)}],
            "containers": [{"name": "main", "image": IMAGE, "mounts": work,
                            "command": ["/bin/sh", "-c", "date +%s > /work/main-start; cat /work/order > /work/main-saw; "
                                        f"test -s /work/token && echo yes > /work/main-token; {sleep}"]}]}
    code, body = orqos.call("PUT", "/workload/pods/is", spec)
    ok(code == 200, f"apply creates a pod with init containers and a gated sidecar ({code} {body.get('error') if isinstance(body, dict) else ''})")
    until("the main container", lambda: unit_state("orqos-is-c-main.service") == "active", seconds=180)
    out = sh("podman", "exec", "orqos-is-main", "sh", "-c",
             "cat /work/order; echo; cat /work/n; cat /work/main-saw; echo; cat /work/main-token; cat /work/token /work/main-start")
    lines = out.split()
    ok(lines[0] == "12" and lines[2] == "12", f"init containers ran in order and the main saw their output ({lines})")
    ok(lines[1] == "3" and unit_state("orqos-is-c-flaky.service", "SubState") == "exited",
       f"a failing init container is retried in place until it succeeds ({lines[1]} runs)")
    ok(lines[3] == "yes" and int(lines[5]) >= int(lines[4]),
       f"the main started only after the sidecar's startup probe passed (token {lines[4]}, main {lines[5]})")
    status = wait_state(orqos, "is", "running")
    kinds = [(c["name"], c["kind"]) for c in status["containers"]]
    ok(kinds == [("one", "init"), ("two", "init"), ("flaky", "init"), ("side", "sidecar"), ("main", "container")],
       f"status lists every container with its kind ({kinds})")
    inits = [c for c in status["containers"] if c["kind"] == "init"]
    ok(all(c["sub_state"] == "exited" for c in inits) and status["ready"],
       "completed init containers read exited, and the pod is ready")

    since = time.time()
    orqos.call("POST", "/workload/pods/is/stop")
    log = sh("journalctl", "--since", f"@{int(since) - 1}", "-o", "short-unix", "--no-pager",
             "-u", "orqos-is-c-main.service", "-u", "orqos-is-c-side.service")
    stopped = {u: float(l.split()[0]) for l in log.splitlines() if "Stopped" in l
               for u in ("main", "side") if f"orqos-is-c-{u}.service" in l or f"-c-{u}" in l}
    ok(stopped.get("main", 0) <= stopped.get("side", 0) and "side" in stopped,
       f"containers stop before their sidecars ({stopped})")
    orqos.call("DELETE", "/workload/pods/is")

    never = {"hostname": "in-0", "restart": "never", "grace_seconds": 2,
             "init": [{"name": "fail", "image": IMAGE, "command": ["/bin/sh", "-c", "exit 1"]}],
             "containers": [{"name": "main", "image": IMAGE, "command": ["sleep", "infinity"]}]}
    code, _ = orqos.call("PUT", "/workload/pods/in", never)
    until("the failed init", lambda: unit_state("orqos-in-c-fail.service") == "failed", seconds=120)
    time.sleep(3)
    _, status = orqos.call("GET", "/workload/pods/in")
    ok(unit_state("orqos-in-c-main.service") != "active" and status["state"] == "failed" and not status["ready"],
       f"with restart: never a failing init container fails the pod start ({status['state']})")
    orqos.call("DELETE", "/workload/pods/in")


def resources(orqos):
    """Limits and requests become the container's cgroup settings; a process past its memory limit is OOM-killed and
    restarted; CPU limits above the host's CPUs are capped; capacity is reported."""
    code, capacity = orqos.call("GET", "/workload/capacity")
    ok(code == 200 and capacity["cpu_millis"] >= 1000 and capacity["memory_bytes"] > 1 << 28,
       f"capacity reports the host's CPUs and memory ({capacity})")
    hog = "trap 'exit 0' TERM; while true; do sleep 1; done"
    spec = {"hostname": "rs-0", "grace_seconds": 2, "containers": [
        {"name": "app", "image": IMAGE, "command": ["/bin/sh", "-c", hog],
         "resources": {"cpu_limit_millis": 500, "cpu_request_millis": 250,
                       "memory_limit_bytes": 64 << 20, "memory_request_bytes": 32 << 20}},
        {"name": "wide", "image": IMAGE, "command": ["/bin/sh", "-c", hog],
         "resources": {"cpu_limit_millis": capacity["cpu_millis"] + 4000}}]}
    code, body = orqos.call("PUT", "/workload/pods/rs", spec)
    ok(code == 200, f"apply creates a pod with resources ({code} {body.get('error') if isinstance(body, dict) else ''})")
    wait_state(orqos, "rs", "running")
    read = sh("podman", "exec", "orqos-rs-app", "sh", "-c",
              "cd /sys/fs/cgroup && cat memory.max memory.swap.max memory.low cpu.max cpu.weight").splitlines()
    ok(read[:4] == ["67108864", "0", "33554432", "50000 100000"],
       f"limits and requests reach the cgroup: memory.max, memory.swap.max, memory.low, cpu.max ({read})")
    ok(read[4] != "100", f"the CPU request sets cpu.weight ({read[4]})")
    wide = sh("podman", "exec", "orqos-rs-wide", "cat", "/sys/fs/cgroup/cpu.max").split()
    ok(int(wide[0]) // int(wide[1]) == capacity["cpu_millis"] // 1000,
       f"a CPU limit above the host's CPUs is capped at them ({wide})")
    def oom_kills():
        events = sh("podman", "exec", "orqos-rs-app", "cat", "/sys/fs/cgroup/memory.events", check=False)
        line = next((l for l in events.splitlines() if l.startswith("oom_kill ")), "oom_kill 0")
        return int(line.split()[1])
    # dd allocates its whole 100 MB block at once, past the 64 MiB limit.
    hog = subprocess.run(["podman", "exec", "orqos-rs-app", "dd", "if=/dev/zero", "of=/dev/null", "bs=100M", "count=1"],
                         capture_output=True, text=True)
    until("the OOM kill", lambda: oom_kills() >= 1, seconds=30)
    ok(hog.returncode != 0 and oom_kills() >= 1,
       f"a process past the memory limit is OOM-killed (exit {hog.returncode}, oom_kill {oom_kills()})")
    orqos.call("DELETE", "/workload/pods/rs")


REGISTRY = "localhost:5000"
REGISTRY_CONF = "/etc/containers/registries.conf.d/99-orqos-smoke.conf"
PRIVATE = f"{REGISTRY}/smoke/private:1"


def start_registry(workdir):
    """A local registry with htpasswd auth on plain HTTP (allowed for localhost:5000 by a smoke-only drop-in)."""
    os.makedirs(os.path.dirname(REGISTRY_CONF), exist_ok=True)
    with open(REGISTRY_CONF, "w") as f:
        f.write(f'[[registry]]\nlocation = "{REGISTRY}"\ninsecure = true\n')
    auth_dir = os.path.join(workdir, "auth")
    os.makedirs(auth_dir, exist_ok=True)
    htpasswd = sh("podman", "run", "--rm", "--entrypoint", "htpasswd", "docker.io/library/httpd:2.4", "-Bbn",
                  "smoke", "s3cret")
    with open(os.path.join(auth_dir, "htpasswd"), "w") as f:
        f.write(htpasswd + "\n")
    sh("podman", "run", "-d", "--name", "orqos-smoke-registry", "-p", "127.0.0.1:5000:5000",
       "-v", f"{auth_dir}:/auth", "-e", "REGISTRY_AUTH=htpasswd", "-e", "REGISTRY_AUTH_HTPASSWD_REALM=smoke",
       "-e", "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd", "docker.io/library/registry:2")
    until("the registry", lambda: subprocess.run(["curl", "-s", "-o", "/dev/null", "-w", "%{http_code}",
                                                  f"http://{REGISTRY}/v2/"], capture_output=True,
                                                 text=True).stdout == "401", seconds=60)
    authfile = os.path.join(workdir, "push-auth.json")
    sh("podman", "login", "--authfile", authfile, "-u", "smoke", "-p", "s3cret", REGISTRY)
    sh("podman", "tag", IMAGE, PRIVATE)
    sh("podman", "push", "--authfile", authfile, PRIVATE)
    sh("podman", "rmi", PRIVATE)


def docker_config(user, password):
    token = base64.b64encode(f"{user}:{password}".encode()).decode()
    return b64(json.dumps({"auths": {REGISTRY: {"auth": token}}}))


def registry(orqos):
    """Pulls use the pod's registry credentials and follow its pull policy; failures are reported at apply and
    change nothing; digests pin images."""
    workdir = "/tmp/orqos-smoke-registry"
    start_registry(workdir)
    for name, password in (("smoke-reg-bad", "wrong"), ("smoke-reg", "s3cret")):
        code, body = orqos.call("PUT", f"/workload/secrets/{name}", {"data": {".dockerconfigjson": docker_config("smoke", password)}})
        ok(code == 200, f"a pull secret is stored ({name})")
    code, body = orqos.call("PUT", "/workload/secrets/smoke-reg-junk", {"data": {".dockerconfigjson": b64("{}")}})

    def pod(image, secret, pull="missing"):
        return {"hostname": "rg-0", "grace_seconds": 2, "image_pull_secrets": [secret] if secret else [],
                "containers": [{"name": "app", "image": image, "pull": pull, "command": ["sleep", "infinity"]}]}

    code, body = orqos.call("PUT", "/workload/pods/rg", pod(PRIVATE, "smoke-reg-junk"))
    ok(code == 422 and "auths" in body["error"], f"a pull secret without auths is refused ({code} {body})")
    code, body = orqos.call("PUT", "/workload/pods/rg", pod(PRIVATE, "smoke-reg-bad"))
    ok(code == 502 and PRIVATE in body["error"] and "unauthorized" in body["error"].lower(),
       f"a pull with wrong credentials is refused at apply, naming the image ({code} {body.get('error', '')[:200]})")
    ok(orqos.call("GET", "/workload/pods/rg")[0] == 404, "a failed pull creates nothing")
    code, body = orqos.call("PUT", "/workload/pods/rg", pod(PRIVATE, "smoke-reg", pull="always"))
    ok(code == 200, f"with the right credentials the private image is pulled and runs ({code})")
    status = wait_state(orqos, "rg", "running")
    image_id = sh("podman", "image", "inspect", "--format", "{{.Id}}", PRIVATE)
    ok(status["containers"][0]["image_id"] == image_id, f"status shows the image the container runs ({image_id[:12]})")
    auth = os.path.join(FILES, "rg", "registry-auth.json")
    ok(owner_mode(auth) == "0:0 600", f"the merged credentials are a root-only file ({owner_mode(auth)})")
    digest = sh("podman", "image", "inspect", "--format", "{{.Digest}}", PRIVATE)
    pinned = f"{REGISTRY}/smoke/private@{digest}"
    code, body = orqos.call("PUT", "/workload/pods/rg", pod(pinned, "smoke-reg"))
    ok(code == 200, f"an image pinned by digest runs ({code} {body.get('error') if isinstance(body, dict) else ''})")
    wait_state(orqos, "rg", "running")
    wrong = f"{REGISTRY}/smoke/private@sha256:{'0' * 64}"
    code, body = orqos.call("PUT", "/workload/pods/rg", pod(wrong, "smoke-reg"))
    ok(code == 502, f"a digest the registry does not have fails the pull ({code})")
    ok(wait_state(orqos, "rg", "running")["spec"]["containers"][0]["image"] == pinned,
       "and the running pod is left as it was")
    orqos.call("DELETE", "/workload/pods/rg")
    ok(not os.path.exists(auth), "removing the pod removes its credentials")
    code, body = orqos.call("PUT", "/workload/pods/rn", pod(f"{REGISTRY}/smoke/absent:1", None, pull="never"))
    ok(code == 422 and "pull policy never" in body["error"], f"never refuses an absent image ({code} {body})")
    for name in ("smoke-reg-bad", "smoke-reg", "smoke-reg-junk"):
        orqos.call("DELETE", f"/workload/secrets/{name}")


def stop_registry():
    subprocess.run(["podman", "rm", "--force", "--ignore", "orqos-smoke-registry"], capture_output=True)
    subprocess.run(["podman", "rmi", "--force", PRIVATE], capture_output=True)
    if os.path.exists(REGISTRY_CONF):
        os.remove(REGISTRY_CONF)


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
        networks(orqos)
        storage(orqos)
        secret_files(orqos)
        probes(orqos)
        init_and_sidecars(orqos)
        resources(orqos)
        registry(orqos)
        print("workload smoke: all checks passed")
    finally:
        try:
            if orqos.process.poll() is None:
                for pod in ("smoke", "web0", "web1", "other", "nettaken", "st", "st2", "st-unknown", "sec", "pr", "pn", "is", "in", "rs", "rg", "rn"):
                    orqos.call("DELETE", f"/workload/pods/{pod}")
                orqos.call("DELETE", "/workload/volumes/smoke-data")
                for name in ("smoke-tls", "smoke-db", "smoke-bad", "smoke-reg", "smoke-reg-bad", "smoke-reg-junk"):
                    orqos.call("DELETE", f"/workload/secrets/{name}")
        except Exception:  # cleanup must continue
            pass
        orqos.stop()
        stop_registry()
        for pod in (FOREIGN, "orqos-taken", "orqos-smoke"):
            subprocess.run(["podman", "pod", "rm", "--force", "--ignore", pod], capture_output=True)
        subprocess.run(["podman", "rm", "--force", "--ignore", "smoke-default-client"], capture_output=True)
        subprocess.run(["podman", "volume", "rm", "--force", "orqos-vol-taken", "orqos-vol-smoke-data"],
                       capture_output=True)
        for network in ("orqos-net-taken", "orqos-net-smoke", "orqos-net-smoke-other"):
            subprocess.run(["podman", "network", "rm", "--force", network], capture_output=True)


if __name__ == "__main__":
    main()
