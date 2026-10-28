#!/usr/bin/env python3
"""Back up SQLite and reset conversation state while retaining accounts, payments and usage."""
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
        backup=path.with_name(path.name+'.before-conversation-reset-'+str(time.time_ns()))
        with closing(sqlite3.connect(path)) as db, closing(sqlite3.connect(backup)) as copy:
            os.chmod(backup,0o600)
            db.backup(copy)
            with db:
                db.execute("INSERT OR IGNORE INTO ledger(user_id,delta,reference,created_at) SELECT user_id,1,'refund:'||user_id||':'||id,? FROM requests WHERE response IS NULL AND error IS NULL",(int(time.time()),))
                db.execute("UPDATE usage SET finished_at=?,error='Conversation reset; token counts may be partial' WHERE finished_at IS NULL",(int(time.time()),))
                db.execute('DROP TABLE requests')
                db.execute('DROP TABLE conversations')
        print('Conversation state reset. Private database backup:',backup)
        print('Users, sessions, credits, purchases, settings and usage records were retained.')


if __name__=='__main__':
    reset(sys.argv[1])
