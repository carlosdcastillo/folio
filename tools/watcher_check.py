"""Prove the watcher meets the spec's latency budget against a running app.

Edits a file in the demo corpus with something that is not Folio, then waits
for a snapshot to appear in the store. The budget is one second from disk
change to snapshot.

    python tools/watcher_check.py
"""

import os
import sqlite3
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DB = os.path.join(ROOT, ".demo", "store", "store.db")
TARGET = os.path.join(ROOT, ".demo", "corpus", "markdowns", "FOLIO_SPEC.md")
BUDGET_SECONDS = 1.0


def versions():
    """Read-only so this never contends with the app's writer."""
    uri = "file:" + DB.replace(os.sep, "/") + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    try:
        return conn.execute(
            "SELECT COUNT(*) FROM snapshots WHERE path LIKE '%FOLIO_SPEC.md'"
        ).fetchone()[0]
    finally:
        conn.close()


def timeline():
    uri = "file:" + DB.replace(os.sep, "/") + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    try:
        return conn.execute(
            "SELECT id, source, author, size FROM snapshots "
            "WHERE path LIKE '%FOLIO_SPEC.md' ORDER BY created_at DESC, rowid DESC LIMIT 3"
        ).fetchall()
    finally:
        conn.close()


def main():
    if not os.path.exists(DB):
        raise SystemExit("no demo store; run: python tools/seed_demo.py")

    before = versions()
    print("versions before: %d" % before)

    with open(TARGET, "a", encoding="utf-8") as fh:
        fh.write("\n## Edited outside Folio\n\nA change made by another editor entirely.\n")
    started = time.time()

    deadline = started + 5
    while time.time() < deadline:
        now = versions()
        if now > before:
            elapsed = time.time() - started
            print("versions after:  %d" % now)
            print("snapshot recorded in %.2fs (budget %.1fs)" % (elapsed, BUDGET_SECONDS))
            for row in timeline():
                print("   ", row)
            if elapsed <= BUDGET_SECONDS:
                print("PASS")
                return 0
            print("SLOW — over budget")
            return 1
        time.sleep(0.02)

    print("FAIL: no snapshot within 5s. Is the app running with FOLIO_STORE set to the demo store?")
    return 1


if __name__ == "__main__":
    sys.exit(main())
