#!/usr/bin/env python3
"""PGX feature contracts against the assembled engine, without external drivers.

PGX_DIST=<prefix> python3 -I tests/pgx/test_features.py -v
"""
import json
import os
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import pgclient
from lib.harness import Pgx


def poll(predicate, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError("condition did not become true in %ss" % timeout)


JANITOR = {
    "pgrust.ephemeral_db_prefix": "'tdb_'",
    "pgrust.ephemeral_db_mint_roles": "'postgres'",
    "pgrust.ephemeral_db_grace": "2",
    "pgrust.ephemeral_db_pool_size": "1",
    "pgrust.ephemeral_db_max_per_role": "4",
    "pgrust.ephemeral_db_connection_limit": "1",
}


class Features(unittest.TestCase):
    def test_mint_pool_reaper_pins_and_clone_schema_isolation(self):
        with Pgx("mint", settings=JANITOR).up() as pg:
            pg.cli("create", "tpl", check=True)
            pg.sql("tpl", "CREATE TABLE payload(id int); INSERT INTO payload VALUES(42)")
            pg.sql("postgres", "SELECT pgrust_seal_template('tpl')")

            def status():
                return json.loads(pg.sql("postgres", "SELECT pgrust_runtime_status()::text").scalar())

            before = status()["janitor"]
            # First use registers the template for pooling and serves a cold mint.
            with pg.connect("tdb_tpl__cold") as cold:
                self.assertEqual(cold.scalar("SELECT id FROM payload"), "42")
                pg.sql("postgres", "SELECT pgrust_pin_database('tdb_tpl__cold')")
            poll(lambda: status()["databases"]["spares"] >= 1)
            self.assertGreater(status()["janitor"]["cold_mints"], before["cold_mints"])
            handouts = status()["janitor"]["pool_handouts"]
            with pg.connect("tdb_tpl__warm") as warm:
                self.assertEqual(warm.scalar("SELECT id FROM payload"), "42")
                with pg.connect("tdb_tpl__cold") as cold:
                    cold.query("ALTER TABLE payload ADD COLUMN isolated text")
                    self.assertEqual(warm.scalar("SELECT count(*) FROM pg_attribute WHERE attrelid='payload'::regclass AND attname='isolated'"), "0")
                    self.assertEqual(cold.scalar("SELECT count(*) FROM pg_attribute WHERE attrelid='payload'::regclass AND attname='isolated'"), "1")
            self.assertGreater(status()["janitor"]["pool_handouts"], handouts)
            poll(lambda: status()["databases"]["spares"] >= 1)
            poll(lambda: pg.sql("postgres", "SELECT count(*) FROM pg_database WHERE datname='tdb_tpl__warm'").scalar() == "0")
            self.assertEqual(pg.sql("postgres", "SELECT count(*) FROM pg_database WHERE datname='tdb_tpl__cold'").scalar(), "1")
            pg.sql("postgres", "SELECT pgrust_unpin_database('tdb_tpl__cold')")
            poll(lambda: pg.sql("postgres", "SELECT count(*) FROM pg_database WHERE datname='tdb_tpl__cold'").scalar() == "0")

    def test_mint_role_cap_and_connection_limit(self):
        settings = dict(JANITOR, **{"pgrust.ephemeral_db_pool_size": "0", "pgrust.ephemeral_db_max_per_role": "1", "pgrust.ephemeral_db_mint_roles": "'tenant'"})
        with Pgx("caps", settings=settings).up() as pg:
            pg.sql("postgres", "CREATE ROLE tenant LOGIN PASSWORD 'test-only'")
            pg.sql("postgres", "CREATE DATABASE tpl")
            pg.sql("postgres", "SELECT pgrust_seal_template('tpl')")
            with pgclient.connect_tcp("127.0.0.1", pg.port, "tdb_tpl__one", "tenant", "test-only") as first:
                self.assertEqual(first.scalar("SELECT 42"), "42")
                with self.assertRaises(pgclient.DbError) as cap:
                    pgclient.connect_tcp("127.0.0.1", pg.port, "tdb_tpl__two", "tenant", "test-only")
                self.assertEqual(cap.exception.sqlstate, "53400")
                with self.assertRaises(pgclient.DbError) as connections:
                    pgclient.connect_tcp("127.0.0.1", pg.port, "tdb_tpl__one", "tenant", "test-only")
                self.assertEqual(connections.exception.sqlstate, "53300")
                self.assertEqual(first.scalar("SELECT 1"), "1")
            self.assertEqual(pg.sql("postgres", "SELECT 1").scalar(), "1")

    def test_session_database_and_runtime_memory_refusals_recover(self):
        for kind, limit in (("session", 32), ("database", 32), ("runtime", 16)):
            with self.subTest(kind=kind), Pgx("mem-" + kind, settings={"pgrust.%s_memory_limit" % kind: str(limit)}).up() as pg:
                pg.cli("create", "limited", check=True)
                pg.cli("create", "sibling", check=True)
                # Runtime pressure is sampled asynchronously by the watchdog.
                if kind == "runtime":
                    poll(lambda: json.loads(pg.sql("postgres", "SELECT pgrust_runtime_status()::text").scalar())["memory"]["pss_bytes"] > limit * 1024 * 1024)
                with pg.connect("limited") as conn, pg.connect("sibling") as sibling:
                    with self.assertRaises(pgclient.DbError) as refused:
                        conn.query("SET work_mem = '256MB'")
                        conn.query("SELECT array_length(array_agg(g), 1) FROM generate_series(1,2000000) g")
                    self.assertEqual(refused.exception.sqlstate, "53200")
                    self.assertEqual(conn.scalar("SELECT 42"), "42", "same session recovers")
                    self.assertEqual(sibling.scalar("SELECT 1"), "1", "sibling database remains usable")


if __name__ == "__main__":
    unittest.main()
