#!/usr/bin/env python3
"""Reset conversation state without creating backups; retain accounts, payments and usage."""
from contextlib import closing
import os
from pathlib import Path
import sqlite3
import sys
import time


# Run with the backend stopped; the OS lock also protects against a second backend process.
def reset(path):
    path=Path(path).resolve(strict=True)
    with path.with_suffix('.lockfile').open('a') as lock:
        if os.name=='posix':
            import fcntl
            fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        with closing(sqlite3.connect(path)) as db:
            with db:
                db.execute("UPDATE usage SET finished_at=?,error='Conversation reset; token counts may be partial' WHERE finished_at IS NULL",(int(time.time()),))
                db.execute('DROP TABLE IF EXISTS provider_counters')
                db.execute('DROP TABLE IF EXISTS tool_calls')
                db.execute('DROP TABLE IF EXISTS request_generation')
                db.execute('DROP TABLE requests')
                db.execute('DROP TABLE conversations')
                db.execute('DROP TABLE IF EXISTS billing_tasks')
            db.execute('PRAGMA wal_checkpoint(TRUNCATE)')
            db.execute('VACUUM')
        print('Conversation state reset. No backup was created.')
        print('Users, sessions, credits, purchases, settings and usage records were retained.')


if __name__=='__main__':
    reset(sys.argv[1])
