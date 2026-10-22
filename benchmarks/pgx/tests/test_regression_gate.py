#!/usr/bin/env python3
"""Exercise the actual classifier subprocess and its CI exit status."""
import difflib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'classify-regress-diffs.py'


class RegressionGate(unittest.TestCase):
    def run_gate(self, expected, actual, allowed=True, missing=False):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            exp, got = root / 'select.out', root / 'actual.out'
            exp.write_text(expected)
            got.write_text(actual)
            diff = f'diff -U3 {exp} {got}\n' + ''.join(difflib.unified_diff(expected.splitlines(True), actual.splitlines(True), fromfile=str(exp), tofile=str(got)))
            (root / 'regression.diffs').write_text(diff)
            (root / 'allowlist.json').write_text(json.dumps({'select': 'Known plan shape only'} if allowed else {}))
            (root / 'regress.json').write_text(json.dumps({'git_commit': 'test', 'tests': [{'test': 'select', 'byte_exact': False}], 'summary': {'scheduled': 2 if missing else 1, 'reported': 1, 'not_reported': ['missing'] if missing else [], 'pg_regress_exit_code': 1, 'server_died_during_run': False}}))
            result = subprocess.run([sys.executable, str(SCRIPT), tmp, '--allowlist', str(root / 'allowlist.json')], capture_output=True, text=True)
            return result, json.loads((root / 'classification.json').read_text())

    def test_approved_plan_only_passes(self):
        result, report = self.run_gate(' QUERY PLAN\n Seq Scan\n(1 row)\n', ' QUERY PLAN\n Index Scan\n(1 row)\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(report['gate_passed'])

    def test_semantic_diff_fails_even_when_name_is_allowed(self):
        result, report = self.run_gate(' result\n true\n(1 row)\n', ' result\n false\n(1 row)\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(report['semantic'], ['select'])

    def test_new_plan_diff_fails(self):
        result, _ = self.run_gate(' QUERY PLAN\n Seq Scan\n(1 row)\n', ' QUERY PLAN\n Index Scan\n(1 row)\n', allowed=False)
        self.assertNotEqual(result.returncode, 0)

    def test_missing_scheduled_test_fails(self):
        result, _ = self.run_gate(' QUERY PLAN\n Seq Scan\n(1 row)\n', ' QUERY PLAN\n Index Scan\n(1 row)\n', missing=True)
        self.assertNotEqual(result.returncode, 0)


if __name__ == '__main__':
    unittest.main()
