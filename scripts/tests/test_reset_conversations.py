from contextlib import closing
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[2]
spec=importlib.util.spec_from_file_location('reset_conversations',ROOT/'scripts/reset_conversations.py')
reset=importlib.util.module_from_spec(spec)
spec.loader.exec_module(reset)


class ConversationReset(unittest.TestCase):
    # Reset removes only conversation state, refunds incomplete requests, and creates a readable backup.
    def test_preserves_business_data_and_backs_up(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'assistant.db'
            schema=(ROOT/'crates/server/src/schema.sql').read_text(encoding='utf-8')
            with closing(sqlite3.connect(path)) as db, db:
                db.executescript(schema)
                db.execute("INSERT INTO users(id,email) VALUES(1,'fixture@example.test')")
                db.execute("INSERT INTO sessions VALUES('session',1,9999999999)")
                db.execute("INSERT INTO purchases VALUES('purchase',1,100,'stripe-fixture',1)")
                db.execute("INSERT INTO ledger(user_id,delta,reference,created_at) VALUES(1,10,'grant',1),(1,-1,'request:1:pending',1),(1,-1,'request:1:done',1)")
                db.execute("INSERT INTO requests(user_id,id,conversation_id,fingerprint) VALUES(1,'pending','chat','pending')")
                db.execute("INSERT INTO requests(user_id,id,conversation_id,fingerprint,response) VALUES(1,'done','chat','done','{}')")
                db.execute("INSERT INTO conversations(user_id,id,messages) VALUES(1,'chat','[]')")
                db.execute("INSERT INTO usage(user_id,request_id,conversation_id,account_home,purpose,started_at) VALUES(1,'pending','chat','account','conversation',1)")
            reset.reset(path)
            with closing(sqlite3.connect(path)) as db, db:
                self.assertEqual(db.execute('SELECT SUM(delta) FROM ledger').fetchone()[0],9)
                for table in ['users','sessions','purchases','usage']:
                    self.assertEqual(db.execute('SELECT COUNT(*) FROM '+table).fetchone()[0],1)
                self.assertIsNotNone(db.execute('SELECT finished_at FROM usage').fetchone()[0])
                self.assertFalse(db.execute("SELECT name FROM sqlite_master WHERE name IN ('conversations','requests')").fetchall())
                db.executescript(schema)
                self.assertEqual(db.execute('SELECT COUNT(*) FROM conversations').fetchone()[0],0)
            with closing(sqlite3.connect(next(Path(directory).glob('*.before-conversation-reset-*')))) as backup:
                self.assertEqual(backup.execute('SELECT COUNT(*) FROM requests').fetchone()[0],2)
                self.assertEqual(backup.execute('SELECT SUM(delta) FROM ledger').fetchone()[0],8)


if __name__=='__main__':
    unittest.main()
