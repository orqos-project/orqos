#!/usr/bin/python3
"""Exercise installed workspaces only on a disposable privileged CI runner."""
import http.client
import json
import os
from pathlib import Path
import subprocess
import time


def request(method, path, body=None):
    token = next(line.split('=', 1)[1] for line in Path('/etc/orqos/host.env').read_text().splitlines()
                 if line.startswith('ORQOS_HOST_TOKEN='))
    connection = http.client.HTTPConnection('127.0.0.1', 9080, timeout=60)
    connection.request(method, path, json.dumps(body) if body is not None else None,
                       {'authorization': 'Bearer ' + token, 'content-type': 'application/json'})
    response = connection.getresponse()
    content = response.read().decode()
    connection.close()
    assert response.status == 200, (method, path, response.status, content)
    return content


def action(identifier, operation, **attrs):
    print(identifier, operation, flush=True)
    return json.loads(request('POST', f'/host/workspaces/{identifier}/actions', dict(operation=operation, **attrs)))


def main():
    assert os.geteuid() == 0 and os.environ.get('GITHUB_ACTIONS') == 'true', 'Disposable CI host only'
    for _ in range(40):
        try:
            request('GET', '/host/inventory')
            break
        except (ConnectionError, OSError):
            time.sleep(0.25)
    for suffix in ('a', 'b'):
        identifier = 'ws-acceptance-' + suffix
        action(identifier, 'configure', slot='project-' + suffix)
        assert action(identifier, 'start')['status'] == 'running'
        record = json.loads(Path('/var/lib/orqos/host', identifier + '.json').read_text())
        action(identifier, 'write-file', path='smoke.txt', content='runtime acceptance\n')
        assert action(identifier, 'read-file', path='smoke.txt')['contents'] == 'runtime acceptance\n'
        command = ['runuser', '-u', record['user'], '--', 'tmux', '-S', '/run/' + record['runtime'] + '/tmux.sock']
        subprocess.run(command + ['send-keys', '-t', 'shared', 'python3 -m http.server 3000 --bind 127.0.0.1', 'Enter'], check=True)
        action(identifier, 'put-application', name='web', port=3000)
        for _ in range(30):
            try:
                assert request('GET', f'/host/workspaces/{identifier}/applications/web/proxy/smoke.txt') == 'runtime acceptance\n'
                break
            except AssertionError:
                time.sleep(0.25)
        else:
            raise AssertionError('Application proxy did not become ready')
        assert '<html' in request('GET', f'/host/workspaces/{identifier}/terminal/').lower()
    action('ws-acceptance-a', 'stop')
    assert json.loads(request('GET', '/host/workspaces/ws-acceptance-b'))['status'] == 'running'
    subprocess.run(['systemctl', 'restart', 'orqos-host'], check=True)
    for _ in range(40):
        try:
            assert json.loads(request('GET', '/host/workspaces/ws-acceptance-b'))['status'] == 'running'
            break
        except (ConnectionError, OSError):
            time.sleep(0.25)
    for suffix in ('a', 'b'):
        action('ws-acceptance-' + suffix, 'remove')
        assert Path('/srv/rawpair/project-' + suffix, 'project/smoke.txt').read_text() == 'runtime acceptance\n'
    print('Two installed workspaces, shared terminal, files, HTTP proxies, independent stop, control restart and data-preserving removal passed')


if __name__ == '__main__':
    try:
        main()
    except Exception:
        for path in Path('/var/lib/orqos/host').glob('*.json'):
            record = json.loads(path.read_text())
            subprocess.run(['journalctl', '-u', record['unit'], '--no-pager', '-n', '50'])
            subprocess.run(['systemctl', 'show', record['unit'], '--property=Result,ExecMainStatus,ActiveState'])
            subprocess.run(['ip', '-j', 'link', 'show', record['link']])
            subprocess.run(['ip', '-d', '-j', 'link', 'show', record['link']])
            alias = Path('/sys/class/net', record['link'], 'ifalias')
            if alias.exists():
                print('Kernel interface alias:', repr(alias.read_text()), flush=True)
        raise
