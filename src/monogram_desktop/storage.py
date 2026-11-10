"""Account-scoped local history. Passwords and encryption keys never enter this store."""
from dataclasses import asdict
from pathlib import Path
import json
import os
import sqlite3

from .fixtures import Chat, Message


class HistoryStore:
    def __init__(self, path=None):
        if path is None:
            root = Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state")) / "monogram-desktop"
            root.mkdir(parents=True, exist_ok=True, mode=0o700)
            path = root / "history.sqlite3"
        self.db = sqlite3.connect(path)
        if str(path) != ":memory:":
            Path(path).chmod(0o600)
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS accounts (jid TEXT PRIMARY KEY, config TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS chats (
                account TEXT NOT NULL REFERENCES accounts(jid) ON DELETE CASCADE,
                peer TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(account,peer));
            CREATE TABLE IF NOT EXISTS messages (
                account TEXT NOT NULL, peer TEXT NOT NULL, id TEXT NOT NULL,
                timestamp TEXT NOT NULL, body TEXT NOT NULL,
                PRIMARY KEY(account,peer,id),
                FOREIGN KEY(account,peer) REFERENCES chats(account,peer) ON DELETE CASCADE);
            CREATE INDEX IF NOT EXISTS message_order ON messages(account,peer,timestamp,id);
            CREATE TABLE IF NOT EXISTS aliases (
                account TEXT NOT NULL, peer TEXT NOT NULL, own INTEGER NOT NULL,
                alias TEXT NOT NULL, message TEXT NOT NULL,
                PRIMARY KEY(account,peer,own,alias));
            CREATE TABLE IF NOT EXISTS preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        """)

    def accounts(self):
        return [json.loads(row[0]) for row in self.db.execute("SELECT config FROM accounts ORDER BY jid")]

    def account(self, jid):
        return next((a for a in self.accounts() if a["jid"] == jid), None)

    def save_account(self, config):
        # An allowlist makes it impossible for a password field to leak into config.
        safe = {key: config[key] for key in ("jid", "host", "port", "direct_tls")}
        with self.db:
            self.db.execute("INSERT INTO accounts VALUES (?,?) ON CONFLICT(jid) DO UPDATE SET config=excluded.config", (safe["jid"], json.dumps(safe)))

    def preference(self, key, default=""):
        row = self.db.execute("SELECT value FROM preferences WHERE key=?", (key,)).fetchone()
        return row[0] if row else default

    def set_preference(self, key, value):
        with self.db:
            self.db.execute("INSERT OR REPLACE INTO preferences VALUES (?,?)", (key, value))

    def save_chat(self, account, chat):
        body = asdict(chat)
        del body["messages"]
        self.db.execute("INSERT INTO chats VALUES (?,?,?) ON CONFLICT(account,peer) DO UPDATE SET body=excluded.body",
                        (account, chat.jid, json.dumps(body)))

    def save_message(self, account, chat, message):
        body = asdict(message)
        body["my_reactions"] = sorted(message.my_reactions)
        self.db.execute("INSERT INTO messages VALUES (?,?,?,?,?) ON CONFLICT(account,peer,id) DO UPDATE SET body=excluded.body,timestamp=excluded.timestamp",
                        (account, chat.jid, message.id, message.timestamp, json.dumps(body)))
        for alias in message.aliases:
            self.db.execute("INSERT OR IGNORE INTO aliases VALUES (?,?,?,?,?)",
                            (account, chat.jid, int(message.own), alias, message.id))

    def save(self, state):
        with self.db:
            for chat in state.chats:
                self.save_chat(state.account, chat)
                for message in chat.messages:
                    self.save_message(state.account, chat, message)

    @staticmethod
    def decode_message(body):
        values = json.loads(body)
        values["my_reactions"] = set(values["my_reactions"])
        return Message(**values)

    def load_chats(self, account):
        chats = []
        for peer, body in self.db.execute("SELECT peer,body FROM chats WHERE account=? ORDER BY rowid", (account,)):
            chat = Chat(**json.loads(body))
            chat.messages = self.history(account, peer)
            chats.append(chat)
        return chats

    def history(self, account, peer, before=None, limit=200):
        where = "account=? AND peer=?"
        args = [account, peer]
        if before:
            where += " AND (timestamp,id)<(?,?)"
            args += list(before)
        rows = self.db.execute(f"SELECT body FROM messages WHERE {where} ORDER BY timestamp DESC,id DESC LIMIT ?", (*args, limit)).fetchall()
        return [self.decode_message(row[0]) for row in reversed(rows)]

    def known(self, account, peer, own, aliases):
        for alias in aliases:
            row = self.db.execute("SELECT message FROM aliases WHERE account=? AND peer=? AND own=? AND alias=?",
                                  (account, peer, int(own), alias)).fetchone()
            if row:
                return row[0]
        return None

    def delete_messages(self, account, chat, messages):
        # Keep aliases as tombstones so archive replay cannot resurrect local deletions.
        with self.db:
            for message in messages:
                self.db.execute("DELETE FROM messages WHERE account=? AND peer=? AND id=?", (account, chat.jid, message.id))

    def forget(self, account):
        with self.db:
            self.db.execute("DELETE FROM accounts WHERE jid=?", (account,))
            self.db.execute("DELETE FROM aliases WHERE account=?", (account,))
            if self.preference("active_account") == account:
                self.db.execute("DELETE FROM preferences WHERE key='active_account'")

    def close(self):
        self.db.close()
