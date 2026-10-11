import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'host'))
spec = importlib.util.spec_from_file_location('admin', ROOT / 'packaging/host-admin.py')
admin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(admin)


class AdminTests(unittest.TestCase):
    def test_subnet_rejects_public_and_wrong_sizes(self):
        for value in ('8.8.0.0/16', '10.1.0.0/24', '::/16', '10.1.1.1/16'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                admin.subnet(value)
        self.assertEqual(str(admin.subnet('10.231.0.0/16')), '10.231.0.0/16')

    def test_route_overlap_and_unrelated_link_are_rejected(self):
        routes = [{'dst': 'default'}, {'dst': '10.231.2.0/24', 'dev': 'vpn0'}]
        with patch.object(admin.host, 'records', return_value=[]), patch.object(admin.host, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(routes), '')):
            with self.assertRaisesRegex(ValueError, 'overlaps host route'):
                admin.check_network('10.231.0.0/16')

    def test_owned_workspace_routes_are_allowed(self):
        routes = [{'dst': 'default'}, {'dst': '10.231.2.0/30', 'dev': 'owned-link'}]
        with patch.object(admin.host, 'records', return_value=[{'link': 'owned-link'}]), patch.object(admin.host, 'host_link', return_value=True), patch.object(admin.host, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(routes), '')):
            admin.check_network('10.231.0.0/16')

    def test_preexisting_account_is_not_adopted(self):
        with patch.object(admin.host, 'inventory', return_value={'subnet': '10.231.0.0/16', 'slots': {}}), patch.object(admin, 'check_network'), patch.object(admin.host, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')), patch.object(admin.host, 'atomic') as atomic:
            with self.assertRaisesRegex(ValueError, 'refusing adoption'):
                admin.provision('project-a')
            atomic.assert_not_called()

    def test_completed_slot_retry_does_not_create_account(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / 'provisioning.json'
            slot = {'user': 'rp-project-a', 'index': 1}
            journal.write_text(json.dumps({'project-a': {'slot': slot, 'complete': False}}))
            with patch.object(admin, 'JOURNAL', journal), patch.object(admin.host, 'protected'), patch.object(admin.host, 'inventory', return_value={'slots': {'project-a': slot}}), patch.object(admin.host, 'run') as run:
                admin.provision('project-a')
                run.assert_not_called()

    def test_mutated_existing_slot_is_not_reassigned(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / 'provisioning.json'
            journal.write_text(json.dumps({'project-a': {'slot': {'index': 1}}}))
            with patch.object(admin, 'JOURNAL', journal), patch.object(admin.host, 'protected'), patch.object(admin.host, 'inventory', return_value={'slots': {'project-a': {'index': 2}}}):
                with self.assertRaisesRegex(ValueError, 'not owned'):
                    admin.provision('project-a')

    def test_interrupted_group_creation_resumes_with_the_dedicated_group(self):
        group = SimpleNamespace(gr_gid=1200, gr_mem=[])
        with patch.object(admin.grp, 'getgrnam', return_value=group), patch.object(admin.pwd, 'getpwall', return_value=[]), patch.object(admin.host, 'run') as run:
            admin.create_account('rp-project-a', '/srv/rawpair/project-a')
            self.assertIn('--gid', run.call_args.args)
            self.assertNotIn('--user-group', run.call_args.args)

    def test_pending_group_used_by_another_identity_is_refused(self):
        group = SimpleNamespace(gr_gid=1200, gr_mem=[])
        with patch.object(admin.grp, 'getgrnam', return_value=group), patch.object(admin.pwd, 'getpwall', return_value=[SimpleNamespace(pw_gid=1200)]), patch.object(admin.host, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'shared or unsuitable'):
                admin.create_account('rp-project-a', '/srv/rawpair/project-a')
            run.assert_not_called()
