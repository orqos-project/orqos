import os
import pathlib
import tempfile
import unittest

from files import open_root, operate


class FileBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.directory.name) / 'project'
        self.root.mkdir()
        self.outside = pathlib.Path(self.directory.name) / 'secret'
        self.outside.write_text('outside')
        self.fd = open_root(str(self.root))

    def tearDown(self):
        os.close(self.fd)
        self.directory.cleanup()

    def test_symlinks_traversal_and_hardlinks_cannot_read_or_overwrite_outside(self):
        (self.root / 'link').symlink_to(self.outside)
        os.link(self.outside, self.root / 'hardlink')
        for path in ('../secret', '/etc/passwd', 'link', 'hardlink', 'x/../link'):
            with self.assertRaises((ValueError, OSError)):
                operate(self.fd, {'operation': 'read-file', 'path': path})
            with self.assertRaises((ValueError, OSError)):
                operate(self.fd, {'operation': 'write-file', 'path': path, 'content': 'changed'})
        self.assertEqual(self.outside.read_text(), 'outside')

    def test_nested_directory_symlink_and_replacement_are_rejected(self):
        (self.root / 'nested').mkdir()
        (self.root / 'nested' / 'file').write_text('original')
        (self.root / 'nested').rename(self.root / 'old')
        (self.root / 'nested').symlink_to(self.root / 'old', target_is_directory=True)
        with self.assertRaises(OSError):
            operate(self.fd, {'operation': 'write-file', 'path': 'nested/file', 'content': 'bad'})
        self.assertEqual((self.root / 'old' / 'file').read_text(), 'original')

    def test_write_read_list_and_size_limit(self):
        (self.root / 'src').mkdir()
        operate(self.fd, {'operation': 'write-file', 'path': 'src/main.py', 'content': 'print("hello")\n'})
        self.assertEqual(operate(self.fd, {'operation': 'read-file', 'path': 'src/main.py'})['contents'], 'print("hello")\n')
        self.assertEqual(operate(self.fd, {'operation': 'list-files'})['files'], ['src/main.py'])
        with self.assertRaises(ValueError):
            operate(self.fd, {'operation': 'write-file', 'path': 'src/main.py', 'content': 'a' * (2 * 1024 * 1024 + 1)})
        self.assertEqual((self.root / 'src/main.py').read_text(), 'print("hello")\n')


if __name__ == '__main__':
    unittest.main()
