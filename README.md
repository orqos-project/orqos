# Orqos

**Orqos** is a small daemon that controls containers by speaking to the Docker (or Podman) API directly.

It runs as a background process, listens for JSON commands, and performs basic container operations like starting, stopping, uploading files, or running commands. That's it.

Orqos doesn't know anything about your application. It doesn't track state. It doesn't pretend to be a scheduler. It just does what it's told and gets out of the way.

---

## Features

* Talks to Docker or Podman using API v1.41
* Accepts input as JSON (stdin or socket)
* Outputs results as JSON
* No dependencies on Docker CLI
* Can be run as a systemd service or from the shell

---

## Example

```json
{
  "op": "start",
  "container": "devbox",
  "image": "ubuntu:latest",
  "env": ["FOO=bar"]
}
```

Output:

```json
{
  "status": "started",
  "id": "acbd1234",
  "ip": "172.18.0.2"
}
```

---

## Use Cases

* Used by [RawPair](https://github.com/rawpair/rawpair) to start per-user development containers
* Used by [Rezn](https://github.com/rezn-project/rezn) as the execution layer for declarative container orchestration
* Can be embedded in CI runners, custom PaaS setups, or your own tools

---

## Requirements

* Linux
* Docker or Podman
* Access to `/var/run/docker.sock` or equivalent
