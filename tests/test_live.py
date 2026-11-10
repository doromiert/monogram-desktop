import asyncio
from datetime import datetime, timezone
from pathlib import Path
import tempfile
import unittest
from xml.etree import ElementTree as ET
from slixmpp import JID

from monogram_desktop.live_state import LiveState
from monogram_desktop.storage import HistoryStore
from monogram_desktop.xmpp import XmppClient, bare_jid, normalize_message,reply_content


def config(jid="alice@example.org"):
    return {"jid": jid, "host": "", "port": 5222, "direct_tls": False}


class StoreTests(unittest.TestCase):
    def test_accounts_drafts_history_and_reopen_are_isolated(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.sqlite3"
            store = HistoryStore(path)
            alice, other = config(), config("other@example.org")
            store.save_account({**alice, "password": "must-not-be-saved"})
            store.save_account(other)
            one = LiveState(alice["jid"], store)
            chat = one.chat_for("bob@example.org")
            one.active = chat
            message = one.send("persisted text")
            chat.draft = "unsent draft"
            chat.draft_reply = message.id
            chat.archived = chat.pinned = chat.unread_mark = True
            one.save()
            # Updating endpoint details must never cascade-delete conversation data.
            store.save_account({**alice, "port": 5223})
            self.assertNotIn("password", store.account(alice["jid"]))
            store.close()
            store = HistoryStore(path)
            loaded = LiveState(alice["jid"], store).chat_for(chat.jid)
            self.assertEqual(loaded.draft, "unsent draft")
            self.assertEqual(loaded.draft_reply, message.id)
            self.assertEqual(loaded.messages[0].delivery, "uncertain")
            self.assertTrue(loaded.archived and loaded.pinned and loaded.unread_mark)
            self.assertEqual(LiveState(other["jid"], store).chat_for(chat.jid).messages, [])
            store.forget(alice["jid"])
            self.assertEqual(store.load_chats(alice["jid"]), [])
            self.assertIsNotNone(store.account(other["jid"]))
            store.close()
            self.assertNotIn(b"must-not-be-saved", path.read_bytes())
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_live_carbon_archive_dedup_tombstones_and_receipt_sender(self):
        store = HistoryStore(":memory:")
        store.save_account(config())
        state = LiveState("alice@example.org", store)
        data = {"peer": "bob@example.org", "own": False, "text": "hello", "aliases": ["wire:abc"], "wire_id": "abc",
                "timestamp": datetime.now(timezone.utc).isoformat(), "reply_id": "", "unavailable": False, "historical": False}
        chat, message, added = state.receive(data)
        state.save()
        self.assertTrue(added)
        self.assertEqual(chat.unread, 1)
        again = {**data, "aliases": ["wire:abc", "archive:def"], "historical": True}
        self.assertFalse(state.receive(again)[2])
        state.save()
        self.assertEqual(len(chat.messages), 1)
        store.delete_messages(state.account, chat, [message])
        chat.messages.clear()
        self.assertFalse(state.receive(again)[2])
        state.active = chat
        outgoing = state.send("reply")
        state.save()
        self.assertIsNone(state.delivery("forged@example.org", outgoing.wire_id, "delivered"))
        state.delivery(chat.jid, outgoing.wire_id, "delivered")
        state.delivery(chat.jid, outgoing.wire_id, "sent")
        self.assertEqual(outgoing.delivery, "delivered")
        store.close()

    def test_bounded_history_pages_without_overlap(self):
        store = HistoryStore(":memory:")
        store.save_account(config())
        state = LiveState("alice@example.org", store)
        for number in range(225):
            message = state.send(str(number))
            message.timestamp = f"2026-11-10T10:{number // 60:02}:{number % 60:02}+00:00"
        state.save()
        latest = store.history(state.account, state.active.jid)
        older = store.history(state.account, state.active.jid, (latest[0].timestamp, latest[0].id))
        self.assertEqual(len(latest), 200)
        self.assertEqual(len(older), 25)
        self.assertFalse({m.id for m in latest} & {m.id for m in older})


class StanzaTests(unittest.TestCase):
    def setUp(self):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        self.events = []
        self.client = XmppClient(config(), "disposable", lambda kind, data: self.events.append((kind, data)))
        self.client.boundjid = JID("alice@example.org/test")

    def tearDown(self):
        self.loop.close()

    def stanza(self, xml):
        return self.client.Message(xml=ET.fromstring(xml))

    def test_jid_and_foreign_carbon_rejection(self):
        for address in ("example.org", "a@example.org/resource", "", "bad address"):
            with self.assertRaises(ValueError):
                bare_jid(address)
        self.assertEqual(bare_jid("Alice@Example.org"), "alice@example.org")
        forged = self.stanza("<message xmlns='jabber:client' from='evil@example.org' to='alice@example.org'><received xmlns='urn:xmpp:carbons:2'><forwarded xmlns='urn:xmpp:forward:0'><message xmlns='jabber:client' from='bob@example.org' to='alice@example.org' type='chat'><body>forged</body></message></forwarded></received></message>")
        self.client.message(forged)
        self.client.carbon(forged, "carbon_received")
        self.assertFalse(self.events)

    def test_encryption_fallback_and_stable_ids(self):
        stanza = self.stanza("<message xmlns='jabber:client' type='chat' from='bob@example.org/r' to='alice@example.org' id='one'><body>fallback must not masquerade as decrypted</body><encrypted xmlns='urn:xmpp:omemo:2'/><stanza-id xmlns='urn:xmpp:sid:0' by='evil@example.org' id='forged'/><origin-id xmlns='urn:xmpp:sid:0' id='origin'/></message>")
        data = normalize_message(stanza, "alice@example.org")
        self.assertTrue(data["unavailable"])
        self.assertNotIn("fallback", data["text"])
        self.assertEqual(data["aliases"], ["wire:one", "origin:origin"])
        unrelated = self.stanza("<message xmlns='jabber:client' type='chat' from='bob@example.org' to='other@example.org'><body>foreign account</body></message>")
        self.assertIsNone(normalize_message(unrelated, "alice@example.org"))

    def test_reply_fallback_counts_codepoints_and_retains_malformed_content(self):
        body="> 🌿 hello\nactual reply"
        xml=ET.fromstring("<message><reply xmlns='urn:xmpp:reply:0' id='reference' to='bob@example.org/r'/><fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'><body start='0' end='10'/></fallback></message>")
        reference,author,quote,clean=reply_content(xml,body)
        self.assertEqual((reference,author,quote,clean),("reference","bob@example.org","> 🌿 hello","actual reply"))
        part=xml.find("{urn:xmpp:fallback:0}fallback/{urn:xmpp:fallback:0}body")
        for start,end in (("-1","10"),("4","2"),("0","10000"),(None,"5")):
            part.attrib.clear()
            if start is not None:part.set("start",start)
            part.set("end",end)
            self.assertEqual(reply_content(xml,body)[3],body)
        part.attrib.clear();part.set("{http://www.w3.org/XML/1998/namespace}lang","fr")
        self.assertEqual(reply_content(xml,body)[3],body)


if __name__ == "__main__":
    unittest.main()
