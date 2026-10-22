#!/usr/bin/env python3
"""Real installer/download/extraction tests; synthetic gh tests only its boundary.

OIDC attestations can only be produced and verified in the release workflow.
These local tests ensure an unavailable/rejecting verifier fails closed.
"""
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / 'install.sh'


class QuietHandler(SimpleHTTPRequestHandler):
    def log_message(self, *_):
        pass


class InstallerSecurity(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.version = '0.0.0-security-test'
        arch = {'x86_64': 'amd64', 'aarch64': 'arm64', 'arm64': 'arm64'}.get(os.uname().machine, os.uname().machine)
        self.name = f'pgx-{self.version}-{os.uname().sysname.lower()}-{arch}'
        self.release = self.root / 'releases' / f'v{self.version}'
        self.release.mkdir(parents=True)
        self.archive = self.release / (self.name + '.tar.gz')
        self.pack()
        handler = lambda *a, **kw: QuietHandler(*a, directory=str(self.root / 'releases'), **kw)
        self.server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.env = dict(os.environ, PGX_VERSION=self.version, PGX_BASE_URL=f'http://127.0.0.1:{self.server.server_port}', PGX_INSTALL_DIR=str(self.root / 'installed'), PGX_BIN_DIR=str(self.root / 'bin'), PGX_INSECURE_ALLOW_HTTP='1', PGX_INSECURE_SKIP_VERIFY='1')

    def pack(self, extra=None):
        with tarfile.open(self.archive, 'w:gz') as tar:
            directory = tarfile.TarInfo(self.name + '/')
            directory.type = tarfile.DIRTYPE
            directory.mode = 0o755
            tar.addfile(directory)
            for path, data in [('bin/pgx', b'#!/bin/sh\necho pgx-security-test\n'), ('share/pgx/dist.json', b'{}')]:
                info = tarfile.TarInfo(self.name + '/' + path)
                info.mode = 0o755 if path.endswith('/pgx') else 0o644
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
            if extra:
                tar.addfile(extra)
        self.checksum()
        Path(str(self.archive) + '.sigstore.json').write_text('{}')

    def checksum(self):
        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        Path(str(self.archive) + '.sha256').write_text(digest + '  ' + self.archive.name + '\n')

    def run_install(self):
        return subprocess.run(['sh', str(INSTALLER)], env=self.env, text=True, capture_output=True)

    def assert_refused(self, diagnostic):
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(diagnostic, result.stderr)
        self.assertFalse((self.root / 'installed').exists())

    def test_download_install_and_execute(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / 'bin/pgx').is_symlink())
        self.assertEqual(subprocess.check_output([self.root / 'bin/pgx', 'version'], text=True).strip(), 'pgx-security-test')

    def test_tampered_checksum_refused(self):
        with self.archive.open('ab') as f:
            f.write(b'tampered')
        self.assert_refused('checksum mismatch')

    def test_rewritten_checksum_still_requires_signed_provenance(self):
        with self.archive.open('ab') as f:
            f.write(b'tampered')
        self.checksum()
        self.env.pop('PGX_INSECURE_SKIP_VERIFY')
        fake = self.root / 'tools'
        fake.mkdir()
        gh = fake / 'gh'
        gh.write_text('#!/bin/sh\nexit 1\n')
        gh.chmod(0o755)
        self.env['PATH'] = str(fake) + os.pathsep + self.env['PATH']
        self.assert_refused('signed build provenance verification failed')

    def test_http_refused_without_explicit_override(self):
        self.env.pop('PGX_INSECURE_ALLOW_HTTP')
        self.assert_refused('HTTPS is required')

    def test_path_traversal_refused(self):
        self.pack(tarfile.TarInfo(self.name + '/../escape'))
        self.assert_refused('unsafe archive member')
        self.assertFalse((self.root / 'escape').exists())

    def test_symlink_refused(self):
        info = tarfile.TarInfo(self.name + '/escape')
        info.type = tarfile.SYMTYPE
        info.linkname = '/tmp'
        self.pack(info)
        self.assert_refused('unsafe links or special files')

    def test_internal_hardlink_is_safe(self):
        info = tarfile.TarInfo(self.name + '/same-data')
        info.type = tarfile.LNKTYPE
        info.linkname = self.name + '/share/pgx/dist.json'
        self.pack(info)
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_external_hardlink_is_refused(self):
        info = tarfile.TarInfo(self.name + '/escape')
        info.type = tarfile.LNKTYPE
        info.linkname = '/etc/passwd'
        self.pack(info)
        self.assert_refused('unsafe links or special files')

    def test_unrelated_binary_is_not_replaced(self):
        target = self.root / 'bin/pgx'
        target.parent.mkdir()
        target.write_text('unrelated')
        self.assert_refused('refusing to replace')
        self.assertEqual(target.read_text(), 'unrelated')


if __name__ == '__main__':
    unittest.main()
