#!/usr/bin/python3
"""Local administrator tools; no HTTP administration endpoints."""
import argparse
import fcntl
import grp
import ipaddress
import json
import os
from pathlib import Path
import pwd
import re
import secrets
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
import host

CONFIG = Path('/etc/orqos')
BASE = Path('/var/lib/orqos')
JOURNAL = BASE / 'provisioning.json'


def subnet(value):
    network = ipaddress.ip_network(value)
    ranges = map(ipaddress.ip_network, ('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16'))
    if network.version != 4 or network.prefixlen != 16 or not any(network.subnet_of(x) for x in ranges):
        raise ValueError('Choose a private IPv4 /16 subnet')
    return network


def check_network(value):
    network = subnet(value)
    owned = set()
    for record in host.records():
        if host.host_link(record):
            owned.add(record['link'])
    routes = json.loads(host.run('ip', '-j', '-4', 'route', 'show', 'table', 'all').stdout)
    for route in routes:
        destination = route.get('dst', 'default')
        if destination == 'default' or route.get('dev') in owned:
            continue
        if network.overlaps(ipaddress.ip_network(destination, strict=False)):
            raise ValueError(f'{network} overlaps host route {destination}; choose another subnet')


def init(value):
    check_network(value)
    if (CONFIG / 'host.json').exists():
        host.protected(CONFIG / 'host.json')
        existing = json.loads((CONFIG / 'host.json').read_text())
        if existing['subnet'] != value:
            raise ValueError('Existing inventory subnet differs; preserve it or migrate explicitly')
    else:
        host.atomic(CONFIG / 'host.json', {'subnet': value, 'dns': ['1.1.1.1', '9.9.9.9'], 'slots': {}})
    env = CONFIG / 'host.env'
    if not env.exists():
        fd, temporary = tempfile.mkstemp(prefix='.host.env-', dir=CONFIG)
        try:
            with os.fdopen(fd, 'w') as stream:
                stream.write('ORQOS_HOST_TOKEN=' + secrets.token_hex(32) + '\n')
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, env)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    host.protected(env)
    host.run('sysctl', '-w', 'net.ipv4.ip_forward=1')
    forwarding = Path('/etc/sysctl.d/70-orqos-host.conf')
    forwarding.write_text('net.ipv4.ip_forward=1\n')
    forwarding.chmod(0o644)
    print('Orqos initialized. Provision slots, then enable --now orqos-host.')


def create_account(account, home):
    # useradd can be interrupted after group creation but before passwd creation.
    # A durable provisioning intent already owns this previously absent name.
    try:
        group = grp.getgrnam(account)
    except KeyError:
        group = None
    if group and (group.gr_gid < 1000 or group.gr_mem or any(user.pw_gid == group.gr_gid for user in pwd.getpwall())):
        raise ValueError('Pending workspace group is shared or unsuitable; refusing adoption')
    group_options = ['--gid', account] if group else ['--user-group']
    host.run('useradd', '--create-home', '--home-dir', home, '--shell', '/usr/sbin/nologin',
             '--expiredate', '1970-01-02', *group_options, account)


def provision(name):
    if not re.fullmatch(r'[a-z][a-z0-9-]{0,24}', name):
        raise ValueError('Slot names: a-z followed by up to 24 lowercase letters, digits or hyphens')
    config = host.inventory()
    if JOURNAL.exists():
        host.protected(JOURNAL)
    journal = json.loads(JOURNAL.read_text()) if JOURNAL.exists() else {}
    if name in config['slots']:
        if name not in journal or journal[name]['slot'] != config['slots'][name]:
            raise ValueError('Existing slot is not owned by this provisioner; inspect inventory')
        print(f'{name} already provisioned')
        return
    check_network(config['subnet'])
    account, home = 'rp-' + name, '/srv/rawpair/' + name
    if name not in journal:
        if host.run('getent', 'passwd', account, check=False).returncode == 0 or host.run('getent', 'group', account, check=False).returncode == 0 or Path(home).exists():
            raise ValueError(f'Existing account/group/home for {name}; refusing adoption')
        used = {slot['index'] for slot in config['slots'].values()}
        used.update(entry['slot']['index'] for entry in journal.values())
        index = next((i for i in range(1, 16383) if i not in used), None)
        if index is None:
            raise ValueError('No network indexes available')
        journal[name] = {'slot': {'label': name, 'user': account, 'home': home,
                                 'project': home + '/project', 'index': index,
                                 'limits': {'cpu_percent': 200, 'memory_mb': 2048, 'tasks': 512}},
                         'complete': False}
        host.atomic(JOURNAL, journal)
    slot = journal[name]['slot']
    if any(slot['index'] == x['index'] for x in config['slots'].values()):
        raise ValueError('Pending network index is now assigned; inspect provisioning journal')
    Path('/srv/rawpair').mkdir(mode=0o755, parents=True, exist_ok=True)
    host.protected('/srv/rawpair', True)
    try:
        user = pwd.getpwnam(account)
    except KeyError:
        create_account(account, home)
        user = pwd.getpwnam(account)
    if (user.pw_dir != home or user.pw_shell != '/usr/sbin/nologin' or user.pw_uid < 1000 or
            user.pw_gid < 1000 or grp.getgrgid(user.pw_gid).gr_name != account):
        raise ValueError('Pending account does not match intended ownership')
    host.run('passwd', '--lock', account)
    host.run('usermod', '--expiredate', '1970-01-02', account)
    for directory in (home, home + '/project', home + '/.local', home + '/.local/bin'):
        path = Path(directory)
        if path.is_symlink():
            raise ValueError('Refusing symlink in workspace home')
        path.mkdir(mode=0o700, exist_ok=True)
        os.chown(path, user.pw_uid, user.pw_gid)
        path.chmod(0o700)
    config['slots'][name] = slot
    host.atomic(CONFIG / 'host.json', config)
    host.inventory()  # Apply the same restrictions as execution.
    journal[name]['complete'] = True
    host.atomic(JOURNAL, journal)
    print(f'Provisioned {name} at network index {slot["index"]}')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    initialize = commands.add_parser('init')
    initialize.add_argument('--subnet', default='10.231.0.0/16')
    network = commands.add_parser('check-subnet')
    network.add_argument('subnet')
    add = commands.add_parser('provision')
    add.add_argument('name')
    commands.add_parser('validate')
    commands.add_parser('diagnose')
    commands.add_parser('stop-all')
    args = parser.parse_args()
    if os.geteuid() != 0:
        raise ValueError('Run with sudo')
    if args.command == 'check-subnet':
        check_network(args.subnet)
        return
    if args.command == 'diagnose':
        config = host.inventory()
        check_network(config['subnet'])
        host.dispatch({'operation': 'health'})
        return
    os.umask(0o077)
    if not any((BASE / marker).exists() for marker in ('package-owner', 'source-owner')):
        raise ValueError('Install the package or source assets before initialization')
    CONFIG.mkdir(mode=0o755, parents=True, exist_ok=True)
    host.STATE.mkdir(mode=0o700, parents=True, exist_ok=True)
    host.protected(CONFIG, True)
    host.protected(BASE, True)
    with (host.STATE / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.command == 'init':
            init(args.subnet)
        elif args.command == 'provision':
            provision(args.name)
        elif args.command == 'validate':
            config = host.inventory()
            check_network(config['subnet'])
            host.dispatch({'operation': 'health'})
        else:
            # Stops verify persisted unit/namespace/link/firewall ownership, keeping records.
            for record in host.records():
                host.stop(record)


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
