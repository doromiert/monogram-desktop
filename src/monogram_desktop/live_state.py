"""Direct-message model for one bare XMPP account, independent of GTK and transport."""
from dataclasses import asdict
from datetime import datetime, timezone
import json

from .fixtures import Chat, Message, PreviewState, FolderRules


class LiveState(PreviewState):
    def __init__(self, account, store):
        super().__init__()
        self.account = account
        self.store = store
        self.mode = "Unencrypted"
        self.chats = store.load_chats(account)
        if not self.chats:
            self.chats = [Chat("saved", "Saved Messages", account, "", "blue", jid=account)]
        self.active = self.chats[0]
        self.notes = {chat.id: chat.draft for chat in self.chats}
        self.requests = []
        self.stories = []
        raw = store.preference("folders:" + account)
        if raw:
            data = json.loads(raw)
            self.custom_folders = {name: FolderRules(**{key: set(value) if key in ("types", "include", "exclude") else value for key, value in rule.items()}) for name, rule in data["rules"].items()}
            self.folder_order = data["order"]
        # Never automatically retry a process-interrupted write: delivery may be uncertain.
        for chat in self.chats:
            for message in chat.messages:
                if message.delivery == "sending":
                    message.delivery = "uncertain"

    def save(self):
        self.store.save(self)
        rules = {}
        for name, rule in self.custom_folders.items():
            rules[name] = {key: sorted(value) if isinstance(value, set) else value for key, value in asdict(rule).items()}
        self.store.set_preference("folders:" + self.account, json.dumps({"rules": rules, "order": self.folder_order}))

    def chat_for(self, peer, name=None):
        chat = next((c for c in self.chats if c.jid == peer), None)
        if chat is None:
            chat = Chat(peer, name or peer, "Offline", (name or peer)[0], "blue", jid=peer, contact=False)
            self.chats.append(chat)
        if name and chat.id != "saved":
            chat.name = name
        return chat

    def filtered(self, query="", folder="All"):
        return sorted(super().filtered(query, folder), key=lambda c: (c.pinned, c.messages[-1].timestamp if c.messages else ""), reverse=True)

    def send(self, text, kind="text"):
        if kind != "text" or not text.strip():
            return None
        now = datetime.now(timezone.utc)
        message = Message("You", text.strip(), now.astimezone().strftime("%H:%M"), True, encrypted=False,
                          timestamp=now.isoformat(), delivery="sending")
        message.wire_id = message.id
        message.aliases = ["wire:" + message.id, "origin:" + message.id]
        self.active.messages.append(message)
        return message

    def receive(self, data, visible=False):
        peer = data["peer"]
        chat = self.chat_for(peer)
        known = self.store.known(self.account, peer, data["own"], data["aliases"])
        if known:
            existing = next((m for m in chat.messages if m.id == known), None)
            if existing:
                existing.aliases = list(dict.fromkeys(existing.aliases + data["aliases"]))
                if existing.own and existing.delivery not in ("delivered", "failed"):
                    existing.delivery = "sent"
            return chat, existing, False
        when = datetime.fromisoformat(data["timestamp"])
        author = "You" if data["own"] else chat.name
        message = Message(author, data["text"], when.astimezone().strftime("%H:%M"), data["own"], encrypted=False,
                          timestamp=data["timestamp"], delivery="sent" if data["own"] else "received",
                          wire_id=data["wire_id"], aliases=data["aliases"], reply_id=data["reply_id"],reply_to=data.get("reply_to",""), unavailable=data["unavailable"])
        if message.reply_id:
            target = self.reply_target(chat,message.reply_id,message.reply_to)
            if target:
                message.reply = target.author + " · " + target.text[:80]
            else:
                message.reply = (message.reply_to+" · " if message.reply_to else "")+(data.get("quote") or "Original message is not loaded")
        chat.messages.append(message)
        chat.messages.sort(key=lambda m: (m.timestamp, m.id))
        if not message.own and not data["historical"] and not visible:
            chat.unread += 1
        return chat, message, True

    def reply_target(self,chat,reference,author=""):
        for message in chat.messages:
            if author and author!=(self.account if message.own else chat.jid):continue
            if reference==message.wire_id or any(prefix+reference in message.aliases for prefix in ("wire:","origin:","server:")):
                return message
        return None

    def delivery(self, peer, wire_id, status):
        chat = next((c for c in self.chats if c.jid == peer), None)
        if chat:
            message = next((m for m in chat.messages if m.own and m.wire_id == wire_id), None)
            if message and (message.delivery != "delivered" or status == "delivered"):
                message.delivery = status
                return message
        return None
