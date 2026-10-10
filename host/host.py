#!/usr/bin/python3
"""Constrained native host operations. Invoked only by authenticated Orqos host mode.
Operator inventory and persistent ownership records are never workspace writable.
"""
import fcntl
import grp
import ipaddress
import json
import os
import pathlib
import pwd
import re
import secrets
import shutil
import stat
import subprocess
import sys
import time

from files import open_root, operate, delete_contents

CONFIG = pathlib.Path('/etc/orqos/host.json')
STATE = pathlib.Path('/var/lib/orqos/host')
SPECS = pathlib.Path('/run/orqos-specs')
ID = re.compile(r'^[a-z0-9][a-z0-9-]{0,47}$')
APP = re.compile(r'^[a-z][a-z0-9-]{0,31}$')


def run(*args, check=True, input=None):
    result = subprocess.run(args, text=True, input=input, capture_output=True, timeout=45,
                            env={'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LANG': 'C.UTF-8'})
    if check and result.returncode:
        raise RuntimeError(f'{args[0]} failed: {result.stderr.strip() or result.stdout.strip()}')
    return result


def protected(path, directory=False):
    info = os.lstat(path)
    if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
        raise RuntimeError(f'{path} must be root owned and not group/world writable')
    if directory and not stat.S_ISDIR(info.st_mode):
        raise RuntimeError(f'{path} must be a directory')


def atomic(path, value, mode=0o600):
    temporary = path.with_name(path.name + '.new')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, mode)
    with os.fdopen(fd, 'w') as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(temporary, mode)
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def inventory():
    protected(CONFIG)
    with CONFIG.open() as source:
        config = json.load(source)
    slots = config['slots']
    users, projects, indexes = set(), set(), set()
    for key, slot in slots.items():
        if not ID.fullmatch(key):
            raise ValueError('Invalid inventory slot name')
        account = pwd.getpwnam(slot['user'])
        if account.pw_uid < 1000 or account.pw_uid == os.getuid() or account.pw_dir != slot['home']:
            raise ValueError(f'{key}: workspace account needs a dedicated non-root uid and matching home')
        if not slot['home'].startswith('/srv/rawpair/'):
            raise ValueError('Workspace homes must be under /srv/rawpair')
        if account.pw_shell not in ('/usr/sbin/nologin', '/sbin/nologin', '/bin/false'):
            raise ValueError('Workspace accounts must disable host login; terminal shells are launched explicitly')
        if account.pw_gid < 1000 or grp.getgrgid(account.pw_gid).gr_name != slot['user']:
            raise ValueError('Workspace accounts require their own dedicated primary group')
        if any(slot['user'] in group.gr_mem and group.gr_gid != account.pw_gid for group in grp.getgrall()):
            raise ValueError('Workspace accounts must not have supplementary groups')
        shadow = next((line.split(':') for line in pathlib.Path('/etc/shadow').read_text().splitlines()
                       if line.split(':', 1)[0] == slot['user']), None)
        if not shadow or not shadow[1].startswith(('!', '*')) or not shadow[7] or int(shadow[7]) > int(time.time() // 86400):
            raise ValueError('Lock and expire workspace accounts to deny host/SSH access, including SSH forwarding')
        if slot['user'] in users or slot['project'] in projects or slot['index'] in indexes:
            raise ValueError('Inventory accounts, projects, and network indexes must be unique')
        if not isinstance(slot['index'], int) or not 1 <= slot['index'] <= 16382:
            raise ValueError('Network index must be 1..16382')
        if not slot['project'].startswith(slot['home'].rstrip('/') + '/'):
            raise ValueError('Project must be inside the configured workspace home')
        for path in (slot['home'], slot['project']):
            fd = open_root(path)
            info = os.fstat(fd)
            os.close(fd)
            if info.st_uid != account.pw_uid:
                raise ValueError(f'{path} must belong to {slot["user"]}')
        users.add(slot['user'])
        projects.add(slot['project'])
        indexes.add(slot['index'])
    network = ipaddress.ip_network(config.get('subnet', '10.231.0.0/16'))
    private_ranges = [ipaddress.ip_network(x) for x in ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16')]
    if network.version != 4 or network.prefixlen != 16 or not any(network.subnet_of(x) for x in private_ranges):
        raise ValueError('subnet must be a private IPv4 /16')
    for resolver in config.get('dns', ['1.1.1.1', '9.9.9.9']):
        addr = ipaddress.ip_address(resolver)
        if addr.version != 4 or not addr.is_global:
            raise ValueError('Use public IPv4 DNS resolvers reachable from workspace namespaces')
    return config


def validate_id(value):
    if not isinstance(value, str) or not ID.fullmatch(value):
        raise ValueError('Invalid workspace ID')
    return value


def record_path(id):
    return STATE / (validate_id(id) + '.json')


def load(id):
    path = record_path(id)
    if not path.exists():
        raise FileNotFoundError('Workspace is not registered')
    protected(path)
    with path.open() as source:
        return json.load(source)


def save(record):
    record['updated_at'] = int(time.time())
    atomic(record_path(record['id']), record)


def records():
    return [load(path.stem) for path in sorted(STATE.glob('*.json'))]


def limits(request, slot):
    maximum = slot.get('limits', {})
    result = {}
    for key, default, minimum, hardmax in [('cpu_percent', 200, 10, 6400),
                                         ('memory_mb', 2048, 128, 1048576),
                                         ('tasks', 512, 32, 65536)]:
        ceiling = maximum.get(key, default)
        value = request.get(key, ceiling)
        if not isinstance(value, int) or isinstance(value, bool) or not minimum <= value <= min(ceiling, hardmax):
            raise ValueError(f'{key} must be {minimum}..{min(ceiling, hardmax)}')
        result[key] = value
    return result


def unit_properties(record):
    result = run('systemctl', 'show', record['unit'], '--no-pager',
                 '--property=LoadState,ActiveState,SubState,Description,User,NetworkNamespacePath,Result,ControlGroup',
                 check=False)
    props = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    if result.returncode and props.get('LoadState') != 'not-found':
        raise RuntimeError('Cannot observe systemd: ' + result.stderr.strip())
    return props


def owned_unit(record, props):
    if props.get('LoadState') == 'not-found':
        return False
    expected = 'Orqos workspace ' + record['token']
    if (props.get('Description') != expected or props.get('User') != record['user'] or
            props.get('NetworkNamespacePath') != '/run/netns/' + record['netns']):
        raise RuntimeError('Unit ownership mismatch; refusing to operate on ' + record['unit'])
    return True


def observation(record):
    props = unit_properties(record)
    exists = owned_unit(record, props)
    active = props.get('ActiveState', 'inactive')
    if not exists or active == 'inactive':
        status = 'failed' if record.get('error') else 'stopped'
    elif active in ('activating', 'reloading'):
        status = 'starting'
    elif active == 'active':
        status = 'running'
    elif active == 'deactivating':
        status = 'stopping'
    else:
        status = 'failed'
    return {'id': record['id'], 'slot': record['slot'], 'status': status,
            'unit': record['unit'], 'netns': record['netns'], 'project': record['project'],
            'user': record['user'], 'limits': record['limits'], 'applications': record['applications'],
            'updated_at': record.get('updated_at'), 'error': record.get('error'),
            'service_result': props.get('Result')}


def nft_name(record):
    return 'orqos_' + record['token'][:16]


def owned_table(record):
    name = nft_name(record)
    result = run('nft', '-j', 'list', 'table', 'inet', name, check=False)
    if result.returncode:
        # Absence must be distinguished from permission / netlink failure.
        tables = json.loads(run('nft', '-j', 'list', 'tables').stdout)
        if any(x.get('table', {}).get('name') == name for x in tables['nftables']):
            raise RuntimeError('Cannot inspect owned nftables table')
        return False
    tables = [x['table'] for x in json.loads(result.stdout)['nftables'] if 'table' in x]
    if not tables or tables[0].get('comment') != record['token']:
        raise RuntimeError('Firewall ownership mismatch; refusing table ' + name)
    return True


def netns_owned(record):
    path = '/run/netns/' + record['netns']
    if not os.path.exists(path):
        return False
    info = os.stat(path)
    if not record.get('netns_identity') and record.get('network_creating'):
        # Durable creation intent plus a full random 128-bit name and root-owned
        # namespace handle closes the crash window between ip netns add and
        # recording its inode. Unprivileged workspace users cannot create this
        # host resource. Once recorded, the exact inode must always match.
        if info.st_uid != 0 or record['netns'] != 'oq-' + record['token']:
            raise RuntimeError('Incomplete namespace creation has invalid ownership')
        record['netns_identity'] = [info.st_dev, info.st_ino]
        save(record)
    if record.get('netns_identity') != [info.st_dev, info.st_ino]:
        raise RuntimeError('Network namespace ownership mismatch: ' + path)
    return True


def host_link(record):
    result = run('ip', '-j', 'link', 'show', record['link'], check=False)
    if result.returncode:
        # Verify ip/netlink itself is working before treating a link as absent.
        run('ip', '-j', 'link', 'show')
        return False
    data = json.loads(result.stdout)[0]
    if data.get('ifalias') != 'orqos:' + record['token']:
        raise RuntimeError('Virtual link ownership mismatch: ' + record['link'])
    return True


def cleanup_network(record):
    # Service must already be stopped. Deleting the host veth deletes its peer.
    if owned_table(record):
        run('nft', 'delete', 'table', 'inet', nft_name(record))
    if host_link(record):
        run('ip', 'link', 'delete', record['link'])
    if netns_owned(record):
        run('ip', 'netns', 'delete', record['netns'])
    record.pop('netns_identity', None)
    record.pop('network_creating', None)
    save(record)
    spec = SPECS / (record['id'] + '.json')
    spec.unlink(missing_ok=True)
    (SPECS / (record['id'] + '.resolv.conf')).unlink(missing_ok=True)


def setup_network(record, config):
    name = record['netns']
    if os.path.exists('/run/netns/' + name):
        raise RuntimeError('Namespace exists before start; stop workspace to recover owned resources')
    record['network_creating'] = True
    save(record)
    run('ip', 'netns', 'add', name)
    info = os.stat('/run/netns/' + name)
    record['netns_identity'] = [info.st_dev, info.st_ino]
    save(record)
    # A dedicated table is an atomic, ownership-labelled transaction. It blocks
    # host/private/cross-workspace access while allowing IPv4 internet egress.
    base = int(ipaddress.ip_network(config.get('subnet', '10.231.0.0/16')).network_address)
    base += 4 * record['index']
    host = str(ipaddress.ip_address(base + 1))
    guest = str(ipaddress.ip_address(base + 2))
    link, peer, table = record['link'], 'guest0', nft_name(record)
    if run('ip', 'link', 'show', link, check=False).returncode == 0:
        raise RuntimeError('Refusing preexisting virtual link')
    # Alias is set in the creation request, avoiding an unlabelled host link.
    run('ip', 'link', 'add', link, 'alias', 'orqos:' + record['token'], 'type', 'veth',
        'peer', 'name', peer, 'netns', name)
    run('ip', 'addr', 'add', host + '/30', 'dev', link)
    run('ip', 'link', 'set', link, 'up')
    for args in [('link', 'set', 'lo', 'up'), ('addr', 'add', guest + '/30', 'dev', peer),
                 ('link', 'set', peer, 'up'), ('route', 'add', 'default', 'via', host)]:
        run('ip', '-n', name, *args)
    run('ip', 'netns', 'exec', name, 'sysctl', '-q', '-w', 'net.ipv6.conf.all.disable_ipv6=1')
    # Existing firewall policy is preserved; operators with an additional
    # forwarding firewall must permit this deployment's outbound/reply traffic.
    rules = f'''table inet {table} {{
      comment "{record['token']}";
      chain input {{ type filter hook input priority -20; policy accept;
        iifname "{link}" counter drop
      }}
      chain output {{ type filter hook output priority -20; policy accept;
        oifname "{link}" counter drop
      }}
      chain forward {{ type filter hook forward priority -20; policy accept;
        iifname "{link}" ip saddr != {guest} counter drop
        iifname "{link}" ip daddr {{ 0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16, 224.0.0.0/4, 240.0.0.0/4 }} counter drop
        oifname "{link}" ct state != established,related counter drop
      }}
      chain nat {{ type nat hook postrouting priority srcnat; policy accept;
        iifname "{link}" ip saddr {guest} masquerade
      }}
    }}'''
    if owned_table(record):
        raise RuntimeError('Firewall table already exists before start')
    run('nft', '-f', '-', input=rules)
    record.pop('network_creating', None)
    save(record)


def write_spec(record, config):
    atomic(SPECS / (record['id'] + '.json'), {
        'id': record['id'], 'runtime': '/run/' + record['runtime'],
        'project': record['project'], 'applications': record['applications']}, 0o644)
    resolver = SPECS / (record['id'] + '.resolv.conf')
    # Root-only parent for writes; file itself is read-only to the workspace.
    resolver.write_text(''.join('nameserver ' + x + '\n' for x in config.get('dns', ['1.1.1.1', '9.9.9.9'])))
    resolver.chmod(0o644)


def stop(record):
    props = unit_properties(record)
    if owned_unit(record, props):
        run('systemctl', 'stop', record['unit'])
        props = unit_properties(record)
        if props.get('LoadState') != 'not-found' and props.get('ActiveState') not in ('inactive', 'failed'):
            raise RuntimeError('Workspace service did not stop; refusing network cleanup')
        run('systemctl', 'reset-failed', record['unit'], check=False)
    cleanup_network(record)
    record['error'] = None
    save(record)
    return observation(record)


def start(record, config):
    props = unit_properties(record)
    if owned_unit(record, props) and props.get('ActiveState') in ('active', 'activating'):
        if not netns_owned(record):
            raise RuntimeError('Active workspace lost its owned namespace; stop then start')
        return observation(record)
    stop(record)
    try:
        setup_network(record, config)
        write_spec(record, config)
        resources = record['limits']
        properties = {
            'User': record['user'], 'Group': pwd.getpwnam(record['user']).pw_gid,
            'WorkingDirectory': record['project'], 'KillMode': 'control-group',
            'TimeoutStopSec': '15s', 'SendSIGKILL': 'yes', 'Delegate': 'no',
            'CPUQuota': str(resources['cpu_percent']) + '%',
            'MemoryMax': str(resources['memory_mb']) + 'M', 'MemorySwapMax': '0',
            'TasksMax': resources['tasks'], 'OOMPolicy': 'stop', 'Restart': 'no',
            'RuntimeDirectory': record['runtime'], 'RuntimeDirectoryMode': '0700',
            'NetworkNamespacePath': '/run/netns/' + record['netns'],
            'NoNewPrivileges': 'yes', 'PrivateTmp': 'yes', 'ProtectSystem': 'strict',
            'ProtectHome': 'yes', 'ProtectKernelTunables': 'yes', 'ProtectKernelModules': 'yes',
            'ProtectControlGroups': 'yes', 'RestrictSUIDSGID': 'yes',
            'CapabilityBoundingSet': '', 'RestrictNamespaces': 'yes',
            'RestrictAddressFamilies': 'AF_UNIX AF_INET AF_INET6',
            'ReadWritePaths': record['home'],
            'InaccessiblePaths': '-/etc/orqos -/var/lib/orqos -/etc/rawpair -/opt/rawpair',
            'BindReadOnlyPaths': f'/run/orqos-specs/{record["id"]}.resolv.conf:/etc/resolv.conf',
            'UMask': '0077', 'Environment': 'HOME=' + record['home'],
        }
        command = ['systemd-run', '--quiet', '--service-type=exec',
                   '--unit=' + record['unit'], '--description=Orqos workspace ' + record['token']]
        command += ['--property=' + key + '=' + str(value) for key, value in properties.items()]
        command += ['--setenv=PATH=' + record['home'] + '/.local/bin:' + record['home'] + '/.cargo/bin:' + record['home'] + '/bin:/usr/local/bin:/usr/bin:/bin', '--setenv=LANG=C.UTF-8', '--setenv=SHELL=/bin/bash',
                    '--setenv=TERM=xterm-256color', '/usr/bin/python3',
                    '/usr/lib/orqos/workspace.py', str(SPECS / (record['id'] + '.json'))]
        run(*command)
        for _ in range(60):
            status = observation(record)
            if status['status'] == 'failed':
                raise RuntimeError('Workspace service failed; journalctl -u ' + record['unit'])
            if os.path.exists('/run/' + record['runtime'] + '/terminal.sock'):
                record['error'] = None
                save(record)
                return observation(record)
            time.sleep(0.1)
        raise RuntimeError('ttyd did not become ready; journalctl -u ' + record['unit'])
    except Exception as error:
        try:
            stop(record)
        except Exception as cleanup:
            error = RuntimeError(f'{error}; cleanup failed: {cleanup}')
        record['error'] = str(error)
        save(record)
        raise error


def as_workspace(record, request):
    # fork and permanently drop all supplementary groups before any project I/O.
    readfd, writefd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(readfd)
        try:
            account = pwd.getpwnam(record['user'])
            os.setgroups([])
            os.setgid(account.pw_gid)
            os.setuid(account.pw_uid)
            os.umask(0o077)
            rootfd = open_root(record['project'])
            try:
                if request['operation'] == 'delete-files':
                    result = delete_contents(rootfd)
                else:
                    result = operate(rootfd, request)
            finally:
                os.close(rootfd)
            response = {'ok': result}
        except Exception as error:
            response = {'error': str(error)}
        with os.fdopen(writefd, 'w') as stream:
            json.dump(response, stream)
        os._exit(0)
    os.close(writefd)
    with os.fdopen(readfd) as stream:
        response = json.load(stream)
    os.waitpid(pid, 0)
    if 'error' in response:
        raise ValueError(response['error'])
    return response['ok']


def dispatch(request):
    config = inventory()
    operation = request['operation']
    if operation == 'health':
        for command in ('systemctl', 'systemd-run', 'ip', 'nft', 'tmux', 'ttyd'):
            if not shutil.which(command):
                raise RuntimeError('Install prerequisite: ' + command)
        run('systemctl', 'show', '--property=Version')
        run('ip', 'netns', 'list')
        run('nft', '-j', 'list', 'tables')
        if pathlib.Path('/proc/sys/net/ipv4/ip_forward').read_text().strip() != '1':
            raise RuntimeError('Enable net.ipv4.ip_forward=1 for workspace egress')
        return {'status': 'ok'}
    if operation == 'reconcile':
        errors = []
        for record in records():
            try:
                observed = observation(record)
                if observed['status'] in ('stopped', 'failed'):
                    if observed['status'] == 'failed' and not record.get('error'):
                        record['error'] = 'Workspace service failed: ' + str(observed.get('service_result'))
                        save(record)
                    cleanup_network(record)
            except Exception as error:
                errors.append({'id': record['id'], 'error': str(error)})
        return {'errors': errors}
    if operation == 'inventory':
        used = {x['slot']: x['id'] for x in records()}
        return {'slots': [{'id': key, 'label': slot.get('label', key),
                           'workspace_id': used.get(key), 'limits': slot.get('limits', {})}
                          for key, slot in config['slots'].items()]}
    if operation == 'list':
        return {'workspaces': [observation(x) for x in records()]}
    id = validate_id(request.get('id'))
    if operation == 'configure':
        slot_id = request.get('slot')
        if slot_id not in config['slots']:
            raise ValueError('Select an operator-provisioned inventory slot')
        slot = config['slots'][slot_id]
        for existing in records():
            if existing['slot'] == slot_id and existing['id'] != id:
                raise ValueError('Inventory slot is already assigned to a workspace')
        if record_path(id).exists():
            record = load(id)
            if record['slot'] != slot_id:
                raise ValueError('An existing workspace cannot change Unix account or project root')
            if observation(record)['status'] not in ('stopped', 'failed'):
                raise ValueError('Stop workspace before changing resource limits')
        else:
            token = secrets.token_hex(16)
            record = {'id': id, 'slot': slot_id, 'user': slot['user'], 'home': slot['home'],
                      'project': slot['project'], 'index': slot['index'], 'token': token,
                      'unit': 'orqos-ws-' + token + '.service', 'netns': 'oq-' + token,
                      'link': 'oq' + token[:12], 'runtime': 'orqos-ws-' + token,
                      'applications': {}, 'error': None}
        record['limits'] = limits(request.get('limits', {}), slot)
        save(record)
        return observation(record)
    record = load(id)
    # Operator mapping changes cannot silently redirect an existing registration.
    slot = config['slots'].get(record['slot'], {})
    if any(record[key] != slot.get(key) for key in ('user', 'home', 'project', 'index')):
        raise ValueError('Inventory mapping changed; restore it before operating on this registration')
    if operation == 'inspect':
        return observation(record)
    if operation == 'start':
        return start(record, config)
    if operation == 'stop':
        return stop(record)
    if operation == 'remove':
        stop(record)
        record_path(id).unlink()
        return {'status': 'removed', 'files_preserved': True}
    if operation in ('list-files', 'read-file', 'write-file', 'delete-files'):
        if operation == 'delete-files':
            if request.get('confirmation') != id:
                raise ValueError('Confirm destructive deletion with the exact workspace ID')
            if observation(record)['status'] not in ('stopped', 'failed'):
                raise ValueError('Stop the workspace before deleting project files')
        return as_workspace(record, request)
    if operation in ('put-application', 'remove-application'):
        name = request.get('name')
        if not isinstance(name, str) or not APP.fullmatch(name):
            raise ValueError('Invalid application name')
        if operation == 'put-application':
            port = request.get('port')
            if not isinstance(port, int) or isinstance(port, bool) or not 1024 <= port <= 65535:
                raise ValueError('Application HTTP port must be 1024..65535')
            if name not in record['applications'] and len(record['applications']) >= 32:
                raise ValueError('Maximum 32 applications per workspace')
            record['applications'][name] = port
        else:
            record['applications'].pop(name, None)
        save(record)
        if observation(record)['status'] == 'running':
            write_spec(record, config)
        return observation(record)
    if operation == 'connect':
        if observation(record)['status'] != 'running' or not netns_owned(record):
            raise ValueError('Workspace is not running')
        target = request.get('application')
        if target is None:
            name = 'terminal.sock'
        elif target in record['applications']:
            name = 'app-' + target + '.sock'
        else:
            raise ValueError('Application is not registered')
        return {'socket': '/run/' + record['runtime'] + '/' + name,
                'uid': pwd.getpwnam(record['user']).pw_uid,
                'cgroup': unit_properties(record).get('ControlGroup')}
    raise ValueError('Unsupported host operation')


def main():
    if os.geteuid() != 0:
        raise RuntimeError('Host backend must run as root through orqos-host.service')
    os.umask(0o077)
    for path, mode in ((STATE, 0o700), (SPECS, 0o755)):
        path.mkdir(mode=mode, parents=True, exist_ok=True)
        protected(path, True)
        # mkdir is filtered by our restrictive umask. Workspace supervisors
        # must traverse SPECS to read their root-owned, read-only launch spec.
        path.chmod(mode)
    # Serialize mutations, status observations and connections against cleanup.
    with (STATE / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        request = json.loads(sys.stdin.read(16 * 1024 * 1024))
        return dispatch(request)


if __name__ == '__main__':
    try:
        print(json.dumps({'ok': main()}))
    except Exception as error:
        print(json.dumps({'error': str(error)}))
        sys.exit(1)
