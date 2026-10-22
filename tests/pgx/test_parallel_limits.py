#!/usr/bin/env python3
"""Real parallel-worker memory-limit and persistent-session recovery gate.

    PGX_DIST=<assembled prefix> python3 -I tests/pgx/test_parallel_limits.py -v
"""
import json
import os
import sys
import threading
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient  # noqa: E402
from lib.harness import Pgx  # noqa: E402


def plan_nodes(node):
    yield node
    for child in node.get("Plans", []):
        yield from plan_nodes(child)


class ParallelLimits(unittest.TestCase):
    def test_worker_refusal_preserves_failed_and_sibling_sessions(self):
        settings = {
            "max_worker_processes": 8,
            "max_parallel_workers": 4,
            "max_parallel_workers_per_gather": 4,
            "pgrust.session_memory_limit": 16,
            "pgrust.memory_watchdog_interval": 100,
            # Exercise the Gather worker path even on machines that normally
            # select the separate runtime analytics pool.
            "pgrust.runtime": "off",
        }
        with Pgx("parallel-limits", settings=settings) as p:
            p.up()
            with p.connect() as admin:
                admin.query("CREATE DATABASE bystander")
            with p.connect() as failed, p.connect("bystander") as sibling, p.connect() as monitor:
                failed.query(
                    "SET max_parallel_workers=4; "
                    "SET max_parallel_workers_per_gather=4; "
                    "SET work_mem='1GB'; "
                    "SET parallel_leader_participation=off; "
                    "SET debug_parallel_query=on; "
                    "SET parallel_setup_cost=0; SET parallel_tuple_cost=0; "
                    "SET statement_timeout='15s'"
                )
                failed.query(
                    "CREATE TABLE oom_input(n int); "
                    "INSERT INTO oom_input VALUES(16); ANALYZE oom_input"
                )
                # Read n from a table to prevent planner constant folding. The
                # delay is part of the worker workload: it lets the other
                # connection observe the actual worker before its allocation.
                query = (
                    "SELECT length(repeat('x', n + "
                    "CASE WHEN pg_sleep(1) IS NULL THEN 0 ELSE 0 END)) "
                    "FROM oom_input"
                )
                plan = json.loads(
                    failed.query("EXPLAIN (ANALYZE, FORMAT JSON) " + query).scalar()
                )[0]["Plan"]
                launched = max(node.get("Workers Launched", 0) for node in plan_nodes(plan))
                self.assertGreaterEqual(launched, 1, plan)
                self.assertEqual(plan["Node Type"], "Gather", plan)
                self.assertTrue(plan.get("Single Copy"), plan)
                failed.query("UPDATE oom_input SET n=48*1024*1024")
                outcome = {}

                def allocate_in_worker():
                    try:
                        outcome["result"] = failed.query(query).rows
                    except pgclient.DbError as error:
                        outcome.update(sqlstate=error.sqlstate, hint=error.hint, context=error.fields.get("W", ""))
                    except Exception as error:
                        outcome["unexpected_error"] = repr(error)

                task = threading.Thread(target=allocate_in_worker, daemon=True)
                task.start()
                seen = set()
                sibling_queries = 0
                deadline = time.monotonic() + 20
                try:
                    while task.is_alive() and time.monotonic() < deadline:
                        seen.update(
                            tuple(row) for row in monitor.query(
                                "SELECT pid, leader_pid, backend_type FROM pg_stat_activity "
                                "WHERE backend_type='parallel worker' "
                                "AND leader_pid=%d" % failed.pid
                            ).rows
                        )
                        self.assertEqual(sibling.query("SELECT 1").scalar(), "1")
                        sibling_queries += 1
                        time.sleep(0.01)
                finally:
                    task.join(timeout=20)
                self.assertFalse(task.is_alive(), "parallel allocation did not finish")
                self.assertTrue(seen, "no actual parallel worker observed during the allocation")
                self.assertGreater(sibling_queries, 0)
                self.assertEqual(outcome.get("sqlstate"), "53200", outcome)
                self.assertIn("pgrust.session_memory_limit", outcome.get("hint") or "")
                self.assertIn("parallel worker", outcome.get("context", ""), outcome)
                # Reuse both original connections: reconnecting would hide a
                # crashed backend or a runtime-wide crash cascade.
                self.assertEqual(failed.query("SELECT 1").scalar(), "1")
                self.assertEqual(sibling.query("SELECT 1").scalar(), "1")
                print(json.dumps({
                    "case": "parallel-worker-memory-limit",
                    "workers_launched": launched,
                    "actual_workers": sorted(seen),
                    "session_limit_mb": 16,
                    "refused_allocation_mb": 48,
                    "sqlstate": outcome["sqlstate"],
                    "error_context": outcome["context"],
                    "bystander_queries_during": sibling_queries,
                    "same_session_after": "1",
                    "sibling_session_after": "1",
                }, sort_keys=True))


if __name__ == "__main__":
    unittest.main()
