#!/usr/bin/python3
"""Unprivileged workspace supervisor, run inside the owned systemd cgroup/netns."""
import asyncio
import json
import os
import signal
import subprocess
import sys


async def bridge(reader, writer, port):
    remote = None
    try:
        upstream, remote = await asyncio.wait_for(asyncio.open_connection("127.0.0.1", port), 5)
        async def copy(source, destination):
            try:
                while chunk := await source.read(65536):
                    destination.write(chunk)
                    await destination.drain()
            finally:
                try:
                    destination.write_eof()
                except (OSError, AttributeError):
                    pass
        await asyncio.gather(copy(reader, remote), copy(upstream, writer))
    except (OSError, asyncio.TimeoutError):
        # Return a usable HTTP error for a stopped/unready development server.
        writer.write(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 31\r\nConnection: close\r\n\r\nDevelopment server unavailable\n")
        await writer.drain()
    finally:
        writer.close()
        if remote:
            remote.close()


async def main():
    os.environ["SHELL"] = "/bin/bash"
    spec_path = sys.argv[1]
    with open(spec_path) as source:
        spec = json.load(source)
    runtime = spec["runtime"]
    socket = f"{runtime}/tmux.sock"
    subprocess.run(["/usr/bin/tmux", "-S", socket, "-f", "/dev/null", "new-session", "-d",
                    "-s", "shared", "-c", spec["project"], "/bin/bash"], check=True)
    terminal = subprocess.Popen(["/usr/bin/ttyd", "-W", "-i", f"{runtime}/terminal.sock",
        "-b", f'/terminal/{spec["id"]}', "/usr/bin/tmux", "-S", socket, "attach", "-t", "shared"])
    stopping = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stopping.set)
    servers = {}
    try:
        while not stopping.is_set():
            if terminal.poll() is not None:
                raise RuntimeError("ttyd exited; see workspace journal")
            if subprocess.run(["/usr/bin/tmux", "-S", socket, "has-session", "-t", "shared"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
                subprocess.run(["/usr/bin/tmux", "-S", socket, "-f", "/dev/null", "new-session", "-d",
                                "-s", "shared", "-c", spec["project"], "/bin/bash"], check=True)

            with open(spec_path) as source:
                apps = json.load(source)["applications"]
            for name in set(servers) - set(apps):
                servers.pop(name)[1].close()
                os.unlink(f"{runtime}/app-{name}.sock")
            for name, port in apps.items():
                if name in servers and servers[name][0] == port:
                    continue
                if name in servers:
                    servers.pop(name)[1].close()
                    os.unlink(f"{runtime}/app-{name}.sock")
                server = await asyncio.start_unix_server(
                    lambda r, w, p=port: bridge(r, w, p), path=f"{runtime}/app-{name}.sock")
                os.chmod(f"{runtime}/app-{name}.sock", 0o600)
                servers[name] = (port, server)
            try:
                await asyncio.wait_for(stopping.wait(), 0.5)
            except asyncio.TimeoutError:
                pass
    finally:
        for _, server in servers.values():
            server.close()
        terminal.terminate()
        subprocess.run(["/usr/bin/tmux", "-S", socket, "kill-server"], check=False)


if __name__ == "__main__":
    os.umask(0o077)
    asyncio.run(main())
