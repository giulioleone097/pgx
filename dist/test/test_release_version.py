#!/usr/bin/env python3
"""Invoke the release version selector exactly as the workflow does."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'release-version.sh'


class ReleaseVersion(unittest.TestCase):
    def select(self, event, ref, value=''):
        return subprocess.run([str(SCRIPT)], env=dict(os.environ, GITHUB_EVENT_NAME=event, GITHUB_REF_NAME=ref, INPUT_VERSION=value), text=True, capture_output=True)

    def test_dispatch_uses_input_without_branch_prefix(self):
        result = self.select('workflow_dispatch', 'main', '0.1.1')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, 'version=0.1.1\n')

    def test_tag_uses_tag_without_input_suffix(self):
        result = self.select('push', 'v1.2.3', '0.1.1')
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, 'version=1.2.3\n')

    def test_shell_payload_is_rejected_without_execution(self):
        with tempfile.TemporaryDirectory() as tmp:
            marker = Path(tmp) / 'executed'
            result = self.select('workflow_dispatch', 'main', f'$(touch {marker})')
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(marker.exists())

    def test_multiline_output_injection_is_rejected(self):
        result = self.select('workflow_dispatch', 'main', '0.1.1\nmalicious=output')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, '')


if __name__ == '__main__':
    unittest.main()
