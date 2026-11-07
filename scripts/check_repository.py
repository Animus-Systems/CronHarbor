#!/usr/bin/env python3
"""Check real migrations and maintained documentation without planning artifacts."""

from pathlib import Path
import re
import sqlite3
import unittest
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parents[1]


class RepositoryChecks(unittest.TestCase):
    def database(self):
        db = sqlite3.connect(":memory:")
        self.addCleanup(db.close)
        for migration in sorted((ROOT / "migrations").glob("*.sql")):
            db.executescript(migration.read_text())
        self.assertEqual(db.execute("PRAGMA foreign_keys").fetchone(), (1,))
        for workload in ("a", "b"):
            db.execute("INSERT INTO workloads (id,namespace,name,kind,source_kind,created_at_us) VALUES (?, 'test', ?, 'scheduled_job', 'FILE', 0)", (workload, workload))
        for revision, workload, number in (("ra", "a", 1), ("ra2", "a", 2), ("rb", "b", 1)):
            db.execute("INSERT INTO workload_revisions VALUES (?, ?, ?, 'v1', '{}', 'hash', 'test', 0)", (revision, workload, number))
        for workload, revision in (("a", "ra"), ("b", "rb")):
            db.execute("UPDATE workloads SET current_revision_id=? WHERE id=?", (revision, workload))
        for schedule, workload, revision in (("sa", "a", "ra"), ("sa2", "a", "ra2"), ("sb", "b", "rb")):
            db.execute("INSERT INTO schedules (id,workload_id,revision_id,schedule_kind,fold_policy,gap_policy,misfire_policy,lateness_grace_seconds) VALUES (?, ?, ?, 'cron', 'first', 'skip', 'skip', 60)", (schedule, workload, revision))
            db.execute("INSERT INTO occurrences VALUES (?, ?, ?, ?, 0, 'DUE', NULL, 0)", ("o" + schedule, schedule, workload, revision))
        db.commit()
        self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])
        return db

    def reject(self, db, sql, parameters):
        trial = sqlite3.connect(":memory:")
        try:
            db.backup(trial)
            trial.execute("PRAGMA foreign_keys=ON")
            with self.assertRaises(sqlite3.IntegrityError):
                trial.execute(sql, parameters)
                trial.commit()
        finally:
            trial.close()

    def test_sql_rejects_wrong_current_revision(self):
        db = self.database()
        for revision in ("rb", "missing"):
            with self.subTest(revision=revision):
                self.reject(db, "UPDATE workloads SET current_revision_id=? WHERE id='a'", (revision,))

    def test_sql_rejects_wrong_schedule_revision(self):
        db = self.database()
        self.reject(db, "INSERT INTO schedules (id,workload_id,revision_id,schedule_kind,fold_policy,gap_policy,misfire_policy,lateness_grace_seconds) VALUES ('bad', 'a', ?, 'cron', 'first', 'skip', 'skip', 60)", ("rb",))

    def test_sql_accepts_deferred_bootstrap(self):
        db = self.database()
        db.execute("INSERT INTO workloads (id,namespace,name,kind,source_kind,current_revision_id,created_at_us) VALUES ('c', 'test', 'c', 'scheduled_job', 'FILE', 'rc', 0)")
        db.execute("INSERT INTO workload_revisions VALUES ('rc', 'c', 1, 'v1', '{}', 'hash', 'test', 0)")
        db.commit()
        self.assertEqual(db.execute("PRAGMA foreign_key_check").fetchall(), [])

    def test_sql_rejects_wrong_occurrence_associations(self):
        db = self.database()
        for schedule, workload, revision in (("sa", "b", "rb"), ("sa", "a", "ra2"), ("sb", "a", "ra")):
            with self.subTest(schedule=schedule, workload=workload, revision=revision):
                self.reject(db, "INSERT INTO occurrences VALUES ('bad', ?, ?, ?, 1, 'DUE', NULL, 0)", (schedule, workload, revision))

    def test_sql_rejects_wrong_execution_associations(self):
        db = self.database()
        for revision, occurrence in (("rb", None), ("ra", "osb"), ("ra", "osa2")):
            with self.subTest(revision=revision, occurrence=occurrence):
                self.reject(db, "INSERT INTO executions (id,workload_id,revision_id,occurrence_id,trigger_kind,created_at_us,state) VALUES ('bad', 'a', ?, ?, 'MANUAL', 0, 'CREATED')", (revision, occurrence))
        db.execute("INSERT INTO executions (id,workload_id,revision_id,occurrence_id,trigger_kind,created_at_us,state) VALUES ('valid', 'a', 'ra', 'osa', 'SCHEDULE', 0, 'CREATED')")
        db.execute("INSERT INTO executions (id,workload_id,revision_id,occurrence_id,trigger_kind,created_at_us,state) VALUES ('manual', 'a', 'ra', NULL, 'MANUAL', 0, 'CREATED')")
        db.commit()

    def test_sql_rejects_wrong_service_revision(self):
        db = self.database()
        self.reject(db, "INSERT INTO service_instances (id,workload_id,revision_id,desired_state,observed_state) VALUES ('bad', 'a', ?, 'STOPPED', 'ABSENT')", ("rb",))

    def test_fixed_grace_constraint(self):
        db = self.database()
        for invalid in (59, 61):
            with self.subTest(grace=invalid):
                self.reject(db, "UPDATE schedules SET lateness_grace_seconds=? WHERE id='sa'", (invalid,))
        db.execute("UPDATE schedules SET lateness_grace_seconds=60 WHERE id='sa'")
        db.commit()

    def test_local_document_links(self):
        documents = [ROOT / name for name in ("README.md", "AGENTS.md", "IMPLEMENTATION.md", "FULL-IMPLEMENTATION.md", "web/dashboard/README.md")]
        documents.extend(sorted((ROOT / "docs").rglob("*.md")))
        for path in documents:
            document = re.sub(r"^```[^\n]*\n.*?^```\s*$", "", path.read_text(), flags=re.M | re.S)
            targets = re.findall(r"!?\[[^\]]+\]\(([^)\s]+)\)", document)
            targets.extend(re.findall(r'<(?:img|a)\b[^>]*\b(?:src|href)="([^"]+)"', document))
            for target in targets:
                url = urlsplit(target.strip("<>"))
                if url.scheme or url.netloc:
                    continue
                with self.subTest(document=str(path.relative_to(ROOT)), target=target):
                    destination = path.parent / unquote(url.path) if url.path else path
                    self.assertTrue(destination.exists(), target)
                    if url.fragment:
                        self.assertTrue(destination.is_file(), target)
                        headings = re.findall(r"^#{1,6}\s+(.+)$", destination.read_text(), re.M)
                        anchors = [re.sub(r"[^\w\- ]", "", title.lower()).replace(" ", "-") for title in headings]
                        self.assertIn(unquote(url.fragment), anchors)


if __name__ == "__main__":
    print(f"Repository artifact checks; in-memory SQLite {sqlite3.sqlite_version}. Runtime/deployment tests run separately.", flush=True)
    unittest.main(verbosity=2)
