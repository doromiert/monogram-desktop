"""Slixmpp transport on its own asyncio thread; no GTK or database access here."""
import asyncio
from datetime import datetime, timezone
import ssl
import threading
from uuid import uuid4
from xml.etree import ElementTree as ET

from slixmpp import ClientXMPP, JID
from slixmpp.exceptions import IqError, IqTimeout


def bare_jid(value):
    value = value.strip()
    jid = JID(value)
    if not jid.user or not jid.domain or jid.resource or len(value) > 3071:
        raise ValueError("Enter an XMPP address such as name@example.org, without a resource.")
    return jid.bare


def reply_content(xml, body):
    """XEP-0461/0428, following mobile ReplyReference's code-point range checks."""
    replies=xml.findall("{urn:xmpp:reply:0}reply")
    if len(replies)!=1 or not 0<len(replies[0].get("id",""))<=4096:
        return "","","",body
    reply=replies[0];author=reply.get("to","")
    try:
        if author and not JID(author).user:return "","","",body
        author=JID(author).bare if author else ""
    except ValueError:
        return "","","",body
    ranges=[]
    try:
        for fallback in xml.findall("{urn:xmpp:fallback:0}fallback"):
            if fallback.get("for")!="urn:xmpp:reply:0":continue
            if not len(fallback):ranges.append((0,len(body)))
            for part in fallback.findall("{urn:xmpp:fallback:0}body"):
                if part.get("{http://www.w3.org/XML/1998/namespace}lang") is not None:continue
                start,end=part.get("start"),part.get("end")
                a,b=(0,len(body)) if start is None and end is None else (int(start),int(end))
                if not 0<=a<=b<=len(body):raise ValueError("Invalid fallback range")
                ranges.append((a,b))
            if len(ranges)>64:raise ValueError("Too many fallback ranges")
        quote=[];clean=[];position=0
        for a,b in sorted(ranges):
            if a>position:clean.append(body[position:a])
            if b>max(a,position):quote.append(body[max(a,position):b])
            position=max(position,b)
        clean.append(body[position:])
        return reply.get("id"),author,"".join(quote).strip(),"".join(clean)
    except (TypeError,ValueError):
        # Invalid ranges retain all visible content, while keeping a valid reference.
        return reply.get("id"),author,"",body


def normalize_message(stanza, account, *, archive_id="", stamp=None):
    """One path for live text, authenticated carbons and query-bound MAM results."""
    if not hasattr(stanza,"xml") or stanza["type"] not in ("chat", "normal", ""):
        return None
    sender, recipient = stanza["from"].bare, stanza["to"].bare
    own = sender == account
    peer = recipient if own else sender
    if not peer or not JID(peer).user or not sender or not recipient or (not own and recipient != account):
        return None
    xml = stanza.xml
    # Never show a fallback body as decrypted content or claim an encryption mode.
    encrypted = any(child.tag in ("{eu.siacs.conversations.axolotl}encrypted", "{urn:xmpp:omemo:2}encrypted", "{urn:xmpp:eme:0}encryption") for child in xml)
    body = "Encrypted message — this desktop build cannot decrypt OMEMO yet." if encrypted else str(stanza["body"])
    if not body:
        return None
    aliases = []
    if stanza["id"]:
        aliases.append("wire:" + stanza["id"])
    origin = xml.find("{urn:xmpp:sid:0}origin-id")
    if origin is not None and origin.get("id"):
        aliases.append("origin:" + origin.get("id"))
    for sid in xml.findall("{urn:xmpp:sid:0}stanza-id"):
        if sid.get("by") == account and sid.get("id"):
            aliases.append("server:" + sid.get("id"))
    if archive_id:
        aliases.append("archive:" + archive_id)
    delayed = xml.find("{urn:xmpp:delay}delay")
    raw_time = stamp or (delayed.get("stamp") if delayed is not None else None)
    try:
        when = datetime.fromisoformat(str(raw_time).replace("Z", "+00:00")) if raw_time else datetime.now(timezone.utc)
        if when.tzinfo is None:
            when = when.replace(tzinfo=timezone.utc)
    except ValueError:
        when = datetime.now(timezone.utc)
    reply_id,reply_to,quote,body=reply_content(xml,body) if not encrypted else ("","","",body)
    return {"peer": peer, "own": own, "text": body, "aliases": aliases,
            "wire_id": stanza["id"], "timestamp": when.astimezone(timezone.utc).isoformat(),
            "reply_id": reply_id,"reply_to":reply_to,"quote":quote,
            "unavailable": encrypted, "historical": bool(archive_id or raw_time)}


class XmppClient(ClientXMPP):
    def __init__(self, config, password, emit, *, ssl_context=None):
        super().__init__(config["jid"] + "/Monogram-" + uuid4().hex[:8], password)
        self.config = config
        self.emit = emit
        self.online = False
        self.stopping = False
        self.fatal = False
        self.backoff = 1
        self.retry_task = None
        self.ready_timeout = None
        self.enable_plaintext = False
        self.enable_direct_tls = bool(config["direct_tls"])
        self.enable_starttls = not self.enable_direct_tls
        self.ssl_context = ssl_context or ssl.create_default_context()
        self.ssl_context.minimum_version = ssl.TLSVersion.TLSv1_2
        self.ssl_context.check_hostname = True
        self.ssl_context.verify_mode = ssl.CERT_REQUIRED
        for plugin in ("xep_0030", "xep_0199", "xep_0085", "xep_0280", "xep_0313", "xep_0359"):
            self.register_plugin(plugin)
        self.register_plugin("xep_0184", {"auto_ack": False})
        self.add_event_handler("session_start", self.started)
        self.add_event_handler("disconnected", self.lost)
        self.add_event_handler("failed_auth", lambda _: self.fail("Authentication failed. Check your address and password."))
        self.add_event_handler("failed_all_auth",lambda _:self.fail("The server did not offer a usable authentication method over verified TLS."))
        self.add_event_handler("ssl_invalid_chain", lambda _: self.fail("The server certificate could not be verified. No credentials were sent."))
        self.add_event_handler("connection_failed", self.connection_failure)
        self.add_event_handler("message", self.message)
        self.add_event_handler("carbon_received", lambda m: self.carbon(m, "carbon_received"))
        self.add_event_handler("carbon_sent", lambda m: self.carbon(m, "carbon_sent"))
        self.add_event_handler("receipt_received", self.receipt)
        self.add_event_handler("roster_update", lambda _: self.roster_changed())
        self.add_event_handler("changed_status", self.presence_changed)
        self.add_event_handler("presence_subscribe", lambda p: self.emit("request", {"jid": p["from"].bare}))
        for name in ("composing", "paused", "active", "inactive", "gone"):
            self.add_event_handler("chatstate_" + name, lambda m, state=name: self.typing(m, state))

    def fail(self, text):
        if self.fatal or self.stopping:
            return
        self.fatal = True
        self.online = False
        if self.ready_timeout:
            self.ready_timeout.cancel()
        self.credentials.clear()
        self.cancel_connection_attempt()
        self.emit("status", {"state": "error", "text": text})
        self.disconnect(wait=0, ignore_send_queue=True)

    def connection_failure(self, error):
        if isinstance(error, ssl.SSLCertVerificationError):
            self.fail("The server certificate could not be verified. Check the server configuration.")
        elif not self.fatal and not self.stopping:
            self.emit("status", {"state": "connecting", "text": "Server unavailable. Retrying…"})

    async def started(self, _):
        if self.ready_timeout:
            self.ready_timeout.cancel()
        if not self.transport or not self.transport.get_extra_info("ssl_object"):
            self.fail("A verified TLS connection is required.")
            return
        self.online = True
        self.backoff = 1
        self.emit("status", {"state": "online", "text": "Connected · TLS"})
        try:
            await self.get_roster(timeout=15)
            self.roster_changed()
        except (IqError, IqTimeout):
            self.emit("warning", {"text": "Connected, but the contact list could not be loaded."})
        if not self.online or self.stopping:
            return
        self.send_presence()
        try:
            await self.plugin["xep_0280"].enable(timeout=10)
        except (IqError, IqTimeout):
            pass  # Carbons are optional; ordinary messaging still works.

    def roster_changed(self):
        contacts = []
        for jid in self.client_roster.keys():
            item = self.client_roster[jid]
            contacts.append({"jid": str(jid), "name": item["name"] or str(jid), "subscription": item["subscription"]})
        self.emit("roster", {"contacts": contacts})

    def presence_changed(self, stanza):
        peer = stanza["from"].bare
        # Aggregate resources; one unavailable resource must not hide another.
        resources = self.client_roster[peer].resources
        self.emit("presence", {"peer": peer, "online": bool(resources)})

    def typing(self, stanza, state):
        if stanza.xml.find("{urn:xmpp:delay}delay") is None and stanza["from"].bare != self.boundjid.bare:
            self.emit("typing", {"peer": stanza["from"].bare, "composing": state == "composing"})

    def receipt(self, stanza):
        self.emit("receipt", {"peer": stanza["from"].bare, "id": str(stanza["receipt"])})

    def message(self, stanza):
        if stanza["type"] == "error":
            self.emit("send_error", {"peer": stanza["from"].bare, "id": stanza["id"], "text": str(stanza["error"]["condition"])})
            return
        # Wrapper messages belong exclusively to their validated handlers.
        if any(stanza.xml.find(tag) is not None for tag in ("{urn:xmpp:carbons:2}received", "{urn:xmpp:carbons:2}sent", "{urn:xmpp:mam:2}result")):
            return
        data = normalize_message(stanza, self.boundjid.bare)
        if data:
            data["receipt_request"] = bool(stanza["request_receipt"] and stanza["id"] and not data["historical"] and not data["own"])
            data["sender"] = str(stanza["from"])
            self.emit("message", data)

    def carbon(self, stanza, direction):
        if str(stanza["from"]) != self.boundjid.bare:
            return
        forwarded = stanza[direction]
        data = normalize_message(forwarded, self.boundjid.bare)
        if data:
            self.emit("message", data)

    async def history(self, peer, before=""):
        if not self.online:
            self.emit("history", {"peer": peer, "error": "Connect to load server history."})
            return
        try:
            result = await self.plugin["xep_0313"].retrieve(with_jid=JID(peer), rsm={"max": 50, "before": before}, timeout=20)
            rows = result["mam"]["results"]
            for row in rows:
                if row["from"].bare not in ("", self.boundjid.bare):
                    continue
                wrapper = row["mam_result"]
                data = normalize_message(wrapper["forwarded"]["stanza"], self.boundjid.bare,
                                         archive_id=wrapper["id"], stamp=wrapper["forwarded"]["delay"]["stamp"])
                if data:
                    self.emit("message", data)
            self.emit("history", {"peer": peer, "before": str(result["mam_fin"]["rsm"]["first"]),
                                  "complete": str(result["mam_fin"]["complete"]).lower() in ("true","1") or not rows})
        except (IqError, IqTimeout):
            self.emit("history", {"peer": peer, "error": "Server history is unavailable or not supported."})

    def send_text(self, peer, text, message_id, reply_id="", reply_to="", quoted_text=""):
        if not self.online or self.stopping or self.fatal:
            self.emit("send_error", {"peer": peer, "id": message_id, "text": "Not connected. Retry after reconnecting."})
            return
        stanza = self.make_message(mto=peer, mbody=text, mtype="chat")
        stanza["id"] = message_id
        stanza["origin_id"]["id"] = message_id
        stanza["request_receipt"] = True
        if reply_id:
            ET.SubElement(stanza.xml, "{urn:xmpp:reply:0}reply", {"id": reply_id, "to": reply_to})
            if quoted_text:
                quoted_text=quoted_text[:512]+("…" if len(quoted_text)>512 else "")
                prefix="> "+quoted_text.replace("\n","\n> ")+"\n"
                stanza["body"]=prefix+text
                fallback=ET.SubElement(stanza.xml,"{urn:xmpp:fallback:0}fallback",{"for":"urn:xmpp:reply:0"})
                ET.SubElement(fallback,"{urn:xmpp:fallback:0}body",{"start":"0","end":str(len(prefix))})
        stanza.send()
        self.emit("sent", {"peer": peer, "id": message_id})

    def send_typing(self, peer, composing):
        if self.online:
            stanza = self.make_message(mto=peer, mtype="chat")
            stanza["chat_state"] = "composing" if composing else "paused"
            stanza.send()

    def authorize(self, peer, allow):
        if self.online:
            self.send_presence(pto=peer,ptype="subscribed" if allow else "unsubscribed")

    def ack_receipt(self, peer, message_id):
        if self.online:
            stanza=self.make_message(mto=peer,mtype="chat")
            stanza["receipt"]=message_id
            stanza.send()

    def lost(self, _):
        self.online = False
        if self.stopping or self.fatal:
            return
        self.emit("status", {"state": "connecting", "text": "Connection lost. Reconnecting…"})
        if self.retry_task is None or self.retry_task.done():
            async def reconnect():
                await asyncio.sleep(self.backoff)
                self.backoff = min(60, self.backoff * 2)
                if not self.stopping and not self.fatal:
                    self.open_connection()
            self.retry_task = self.loop.create_task(reconnect())

    def open_connection(self):
        if self.ready_timeout:
            self.ready_timeout.cancel()
        self.ready_timeout = self.loop.call_later(45, lambda: self.fail("Connection timed out. Check the server and reconnect."))
        host = self.config["host"]
        self.connect(host=host or None, port=self.config["port"] if host else None)


class XmppSession:
    """A disposable session; callbacks contain values and never reference GTK objects."""
    def __init__(self, config, password, emit, *, ssl_context=None):
        self.config = config
        self.emit = emit
        self.client = None
        self.loop = None
        self.closed = threading.Event()
        self.thread = threading.Thread(target=self.run, args=(password, ssl_context), daemon=True, name="monogram-xmpp")
        self.thread.start()

    def run(self, password, ssl_context):
        self.loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self.loop)
        try:
            self.client = XmppClient(self.config, password, self.emit, ssl_context=ssl_context)
            password = None
            if self.closed.is_set():
                return
            self.client.open_connection()
            self.loop.run_forever()
        except Exception:
            # Never forward arbitrary exception strings, XML or credentials to the UI.
            self.emit("status", {"state": "error", "text": "The connection could not be started. Check the account settings."})
        finally:
            if self.client:
                self.client.credentials.clear()
                if self.client.transport:
                    self.client.transport.abort()
            tasks = asyncio.all_tasks(self.loop)
            for task in tasks:
                task.cancel()
            self.loop.run_until_complete(asyncio.gather(*tasks, return_exceptions=True))
            self.loop.close()

    def call(self, method, *args):
        loop = self.loop
        if loop and not loop.is_closed() and self.client and not self.closed.is_set():
            def invoke():
                result = getattr(self.client, method)(*args)
                if asyncio.iscoroutine(result):
                    loop.create_task(result)
            loop.call_soon_threadsafe(invoke)
            return True
        return False

    def close(self):
        self.closed.set()
        loop = self.loop
        if loop and not loop.is_closed():
            def stop():
                if self.client:
                    self.client.stopping = True
                    self.client.cancel_connection_attempt()
                    self.client.disconnect(wait=0, ignore_send_queue=True)
                loop.call_later(.1, loop.stop)
            loop.call_soon_threadsafe(stop)
