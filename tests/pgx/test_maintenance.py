#!/usr/bin/env python3
"""Maintenance retries with a real engine and persistent SQL sessions.

    PGX_DIST=<prefix> python3 -I tests/pgx/test_maintenance.py -v
"""
import os
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib.harness import Pgx  # noqa: E402


class MaintenanceCase(unittest.TestCase):
    def test_autovacuum_retries_without_new_writes_after_old_snapshot_leaves(self):
        with Pgx("avretry") as pgx:
            pgx.init()
            with open(os.path.join(pgx.data, "postgresql.conf"), "a") as config:
                config.write("\nautovacuum_naptime=1\n"
                             "autovacuum_vacuum_threshold=0\n"
                             "autovacuum_vacuum_scale_factor=0\n"
                             "autovacuum_analyze_threshold=2147483647\n"
                             "pgrust.autovacuum_skip_idle_databases=on\n")
            pgx.start()
            with pgx.connect() as writer, pgx.connect() as snapshot, pgx.connect() as monitor:
                writer.query("CREATE TABLE retry_t(n int); "
                             "INSERT INTO retry_t SELECT generate_series(1,10000); "
                             "ANALYZE retry_t")
                snapshot.query("BEGIN ISOLATION LEVEL REPEATABLE READ; "
                               "SELECT count(*) FROM retry_t")
                writer.query("DELETE FROM retry_t; SELECT pg_stat_force_next_flush()")

                def stats():
                    row = monitor.query("SELECT autovacuum_count,n_dead_tup "
                                        "FROM pg_stat_all_tables WHERE relname='retry_t'").rows[0]
                    return tuple(map(int, row))

                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    count, dead = stats()
                    if count >= 1 and dead >= 10000:
                        break
                    time.sleep(0.1)
                else:
                    self.fail("autovacuum never visited the written database: " + pgx.log_tail())

                # Let first-visit catalog/stat changes settle while old xmin
                # still protects every deleted row. Add no further writes.
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    time.sleep(0.1)
                before_count, dead = stats()
                self.assertEqual(dead, 10000)
                snapshot.query("ROLLBACK")

                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    count, dead = stats()
                    if dead == 0:
                        break
                    time.sleep(0.1)
                self.assertEqual(dead, 0, "unchanged written databases must be retried: " + pgx.log_tail())
                self.assertGreater(count, before_count)
                self.assertEqual(writer.query("SELECT 1").scalar(), "1")


if __name__ == "__main__":
    unittest.main()
