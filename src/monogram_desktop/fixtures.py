"""Fictional fixtures and transient state. No network, credentials, or disk history."""
from dataclasses import dataclass, field
from datetime import datetime
from uuid import uuid4

@dataclass
class Message:
    author: str
    text: str
    time: str = "10:42"
    own: bool = False
    kind: str = "text"
    encrypted: bool = True
    reply: str = ""
    reactions: list = field(default_factory=list)
    my_reactions: set = field(default_factory=set)
    pinned: bool = False
    id: str = field(default_factory=lambda: uuid4().hex)
    timestamp: str = ""
    delivery: str = "local"
    reply_id: str = ""
    reply_to: str = ""
    wire_id: str = ""
    aliases: list = field(default_factory=list)
    options: list = field(default_factory=list)
    gif_index: int = 0
    unavailable: bool = False

@dataclass
class Chat:
    id: str
    name: str
    subtitle: str
    avatar: str
    color: str
    unread: int = 0
    group: bool = False
    messages: list = field(default_factory=list)
    contact: bool = True
    bot: bool = False
    channel: bool = False
    archived: bool = False
    notifications: str = "all"
    unread_mentions: int = 0
    unread_mark: bool = False
    jid: str = ""
    pinned: bool = False
    draft: str = ""
    draft_reply: str = ""

CHAT_TYPES={"contacts","non_contacts","groups","channels","bots"}

@dataclass
class FolderRules:
    types: set = field(default_factory=set)
    exclude_muted: bool = False
    exclude_read: bool = False
    exclude_archived: bool = False
    include: set = field(default_factory=set)
    exclude: set = field(default_factory=set)

    def matches(self,chat):
        # Same override order as Android MessagesController.DialogFilter.
        if chat.id in self.exclude:return False
        if chat.id in self.include:return True
        if self.exclude_archived and chat.archived:return False
        if not chat.unread_mentions:
            if self.exclude_muted and chat.notifications not in ("all","mentions"):return False
            if self.exclude_read and not chat.unread and not chat.unread_mark:return False
        kind="channels" if chat.channel else "groups" if chat.group else "bots" if chat.bot else "contacts" if chat.contact else "non_contacts"
        return kind in self.types

@dataclass
class Story:
    owner: str
    asset: str
    caption: str = ""
    seen: bool = False
    own: bool = False
    liked: bool = False

CHATS = [
    Chat("design", "Design room", "8 members · 3 online", "D", "purple", 3, True, [
        Message("Alex", "Morning! I put together a few ideas for the desktop layout.", "10:31"),
        Message("You", "Nice. More room for the conversation, fewer things floating over it.", "10:32", True),
        Message("Sam", "A little inspiration from our weekend walk.", "10:34", kind="photo", reactions=["♡ 3", "✨ 2"]),
        Message("Alex", "Where should the next meetup be?", "10:36", kind="poll"),
        Message("You", "The café by the park gets my vote ☕", "10:38", True, reply="Alex · Where should the next meetup be?"),
        Message("Sam", "Also, the updated notes are here.", "10:40", kind="file"),
        Message("Alex", "This already feels at home on desktop.", "10:42", reactions=["❤️ 2"]),
    ]),
    Chat("mira", "Mira", "Online", "M", "pink", 1, False, [Message("Mira", "Found a lovely spot for Saturday!", "10:25", kind="photo"), Message("You", "That light is beautiful.", "10:28", True), Message("Mira", "Let's take the camera this time 📷", "10:29")]),
    Chat("saved", "Saved Messages", "Your personal space", "", "blue", messages=[Message("You", "A home for links, notes, and things to come back to.", "Yesterday", True), Message("You", "Desktop ideas\n\n• Keep the native window controls\n• Use the file portal\n• Make everything work with a keyboard", "09:12", True, kind="article")]),
    Chat("weekend", "Weekend plans", "5 members", "W", "green", 2, True, [Message("Robin", "Anyone up for a walk on Saturday?", "09:18"), Message("You", "Absolutely 🌿", "09:21", True), Message("Jules", "I've shared the meeting point.", "09:22", kind="location")]),
    Chat("alex", "Alex", "Last seen recently", "A", "orange", messages=[Message("Alex", "Sent you the source files. No rush!", "Yesterday", kind="file"), Message("You", "Thanks, I'll take a look later.", "Yesterday", True)]),
    Chat("opensource", "Open source café", "24 members · 6 online", "O", "teal", 12, True, [Message("Casey", "Today's tiny victory: a clean build on the first try.", "09:04"), Message("You", "Teach us your ways", "09:07", True), Message("Sam", "🎉", "09:08", kind="sticker")]),
    Chat("robin", "Robin", "Last seen yesterday", "R", "red", messages=[Message("Robin", "See you next week!", "Yesterday")]),
]

def demo_chats():
    import copy
    return copy.deepcopy(CHATS)

class PreviewState:
    def __init__(self):
        self.account = ""
        self.chats = demo_chats()
        self.active = self.chats[0]
        self.mode = "PQ OMEMO2"
        self.wallets = ["Everyday", "Savings"]
        self.wallet_index = 0
        self.notes = {}
        self.custom_folders={"All":FolderRules(CHAT_TYPES.copy(),exclude_archived=True),"Unread":FolderRules(CHAT_TYPES.copy(),exclude_read=True,exclude_archived=True),"Groups":FolderRules({"groups"},exclude_archived=True)}
        self.folder_order=list(self.custom_folders)
        self.stories=[Story("Mira","landscape.svg","A little time outdoors."),Story("Mira","sunset.svg","Worth staying for sunset."),Story("Sam","city.svg","The long way home."),Story("Alex","landscape.svg","Weekend plans.")]
        self.requests = [("Jules", "Contact request · jules@example.org"), ("Photography club", "Room invitation · 12 members")]

    def filtered(self, query="", folder="All"):
        query = query.casefold()
        rule=self.custom_folders.get(folder,FolderRules(CHAT_TYPES))
        return [c for c in self.chats if rule.matches(c) and
                (not query or query in c.name.casefold() or any(query in m.text.casefold() for m in c.messages))]

    def reorder_folder(self,name,target,after=False):
        if name==target:return
        self.folder_order.remove(name)
        self.folder_order.insert(self.folder_order.index(target)+int(after),name)

    def send(self, text, kind="text"):
        if not text.strip():
            return None
        message = Message("You", text.strip(), datetime.now().strftime("%H:%M"), True, kind, self.mode != "Unencrypted")
        self.active.messages.append(message)
        return message
