"""Native account controls and the main-thread boundary for live XMPP events."""
import sqlite3

from gi.repository import Adw, Gtk, GLib, Gio

from .fixtures import PreviewState
from .live_state import LiveState
from .storage import HistoryStore
from .widgets import box, label, button, group, action_row, scrolled
from .xmpp import XmppSession, bare_jid


class Accounts:
    def __init__(self, window):
        self.w = window
        window.accounts = self
        self.store = HistoryStore(":memory:" if window.get_application().smoke else None)
        self.preview = window.state
        self.session = None
        self.generation = 0
        self.status = "offline"
        self.status_text = "Not connected"
        self.history_pending = set()
        self.history_pages = {}
        self.typing_until = {}
        self.last_typing = 0
        self.w.big_emoji=self.store.preference("big_emoji","true")=="true"
        scheme=self.store.preference("style","system")
        Adw.StyleManager.get_default().set_color_scheme({"system":Adw.ColorScheme.DEFAULT,"light":Adw.ColorScheme.FORCE_LIGHT,"dark":Adw.ColorScheme.FORCE_DARK}.get(scheme,Adw.ColorScheme.DEFAULT))
        active = self.store.preference("active_account")
        if active and self.store.account(active):
            self.activate(LiveState(active, self.store))
        window.connect("close-request", self.closing)
        self.tick_id = GLib.timeout_add_seconds(1, self.tick)

    def persist(self):
        if not self.w.state.account:
            return True
        try:
            self.w.state.save()
            return True
        except (OSError, sqlite3.Error):
            self.w.notice("Local history could not be saved. Check free space and file permissions.")
            return False

    def activate(self, state):
        w = self.w
        w.save_draft()
        self.persist()
        self.disconnect()
        dialogs=w.get_dialogs()
        for index in reversed(range(dialogs.get_n_items())):
            dialogs.get_item(index).close()
        child=w.viewer_layer.get_first_child().get_next_sibling()
        while child:
            following=child.get_next_sibling()
            if hasattr(child,"close"):child.close()
            child=following
        w.state = state
        self.history_pages.clear()
        self.history_pending.clear()
        self.typing_until.clear()
        w.folder = state.folder_order[0]
        while w.folder_bar.get_first_child():
            w.folder_bar.remove(w.folder_bar.get_first_child())
        first = None
        for name in state.folder_order:
            b = Gtk.ToggleButton(label=name)
            b.add_css_class("flat")
            if first:
                b.set_group(first)
            else:
                first = b
            w.connect_folder(b)
            w.folder_bar.append(b)
        first.set_active(True)
        w.story_strip.refresh()
        w.story_strip.set_visible(not state.account)
        w.select_chat(state.active, save_previous=False)
        w.request_badge.set_text(str(len(state.requests)))
        w.request_badge.set_visible(bool(state.requests))
        if state.account:
            self.store.set_preference("active_account", state.account)
        else:
            self.store.set_preference("active_account", "")
        for name in ("wallet", "stories", "spaces", "profile", "poll", "payment", "location", "article", "developer", "call", "attach-file", "attach-media"):
            w.lookup_action(name).set_enabled(not bool(state.account))
        for name,toggle in w.media_toggles.items():toggle.set_enabled(name=="emoji" or not bool(state.account))
        if state.account:w.media_stack.set_visible_child_name("emoji")
        w.chat.live_banner.set_revealed(bool(state.account))
        w.chat.history_button.set_visible(bool(state.account))
        self.update_status()

    def disconnect(self):
        self.generation += 1
        if self.session:
            self.session.close()
            self.session = None
        self.status = "offline"
        self.status_text = "Disconnected"

    def connect(self, config, password, *, ssl_context=None):
        self.w.save_draft()
        self.persist()
        self.store.save_account(config)
        state = self.w.state if self.w.state.account == config["jid"] else LiveState(config["jid"], self.store)
        self.activate(state)
        self.status = "connecting"
        self.status_text = "Connecting…"
        generation = self.generation
        owner = config["jid"]
        def emit(kind, data):
            GLib.idle_add(self.event, generation, owner, kind, data)
        self.session = XmppSession(config, password, emit, ssl_context=ssl_context)
        self.update_status()

    def event(self, generation, owner, kind, data):
        if generation != self.generation or owner != self.w.state.account:
            return False
        w = self.w
        state = w.state
        if kind == "status":
            self.status, self.status_text = data["state"], data["text"]
            if self.status != "online":
                self.typing_until.clear()
                for chat in state.chats:
                    chat.subtitle = "Offline"
                    for message in chat.messages:
                        if message.delivery == "sending":
                            message.delivery = "uncertain"
            self.update_status()
            if self.status == "online":
                self.load_history(initial=True)
            elif self.status == "error":
                w.notice(self.status_text)
        elif kind == "roster":
            for contact in data["contacts"]:
                chat = state.chat_for(contact["jid"], contact["name"])
                chat.contact = contact["subscription"] in ("both", "to", "from")
        elif kind == "message":
            visible = data["peer"] == state.active.jid and w.is_active() and w.pages.get_visible_child_name() == "chat"
            chat, message, added = state.receive(data, visible)
            if chat is state.active:
                adjustment=w.chat.scroll.get_vadjustment()
                at_bottom=adjustment.get_upper()-adjustment.get_page_size()-adjustment.get_value()<48
                w.chat.render(not data["historical"] and (at_bottom or bool(message and message.own)))
            if added and not visible and not data["historical"] and not data["own"]:
                self.notify(chat, message)
        elif kind in ("sent", "receipt", "send_error"):
            status = {"sent": "sent", "receipt": "delivered", "send_error": "failed"}[kind]
            message = state.delivery(data["peer"], data["id"], status)
            if message:
                w.chat.render(False)
            if kind == "send_error":
                w.notice("Message could not be sent. You can retry from its menu.")
        elif kind == "presence":
            chat = next((c for c in state.chats if c.jid == data["peer"]), None)
            if chat:
                chat.subtitle = "Online" if data["online"] else "Offline"
                if chat is state.active and w.pages.get_visible_child_name()=="chat":
                    w.show_chat()
        elif kind == "typing":
            self.typing_until[data["peer"]] = GLib.get_monotonic_time() + 10_000_000 if data["composing"] else 0
            self.update_status()
        elif kind == "request":
            item = (data["jid"], "Presence subscription request")
            if item not in state.requests:
                state.requests.append(item)
            w.request_badge.set_text(str(len(state.requests)))
            w.request_badge.set_visible(bool(state.requests))
        elif kind == "history":
            peer = data["peer"]
            self.history_pending.discard(peer)
            if "error" not in data:
                self.history_pages[peer] = data
            if peer == state.active.jid:
                w.chat.history_button.set_sensitive(True)
                w.chat.history_button.set_label("Load Earlier Messages" if "error" not in data else "Retry Server History")
                w.chat.history_button.set_tooltip_text(data.get("error", "Load older local and server messages"))
            if data.get("error"):
                w.notice(data["error"])
        elif kind == "warning":
            w.notice(data["text"])
        saved=self.persist()
        if saved and kind=="message" and data.get("receipt_request") and self.preference("delivery_receipts",False) and self.status=="online":
            self.session.call("ack_receipt",data["sender"],data["wire_id"])
        w.refresh_chats()
        return False

    def send(self, text, kind="text"):
        if kind != "text":
            self.w.notice("Live attachments and rich messages are not available yet.")
            return None
        if self.status != "online" or not self.session:
            self.w.notice("Connect your account before sending. Your draft is kept.")
            return None
        message = self.w.state.send(text)
        if message is None:
            return None
        reply = self.w.chat.replying
        if reply:
            message.reply = reply.author + " · " + reply.text[:80]
            message.reply_id = reply.wire_id
            message.reply_to=self.w.state.account if reply.own else self.w.state.active.jid
        if not self.persist():
            self.w.state.active.messages.remove(message)
            return None
        self.dispatch(message)
        return message

    def dispatch(self, message):
        chat = self.w.state.active
        reply = self.w.state.reply_target(chat,message.reply_id,message.reply_to) if message.reply_id else None
        reply_to = message.reply_to or (self.w.state.account if reply and reply.own else chat.jid)
        if not self.session or not self.session.call("send_text", chat.jid, message.text, message.wire_id, message.reply_id, reply_to,reply.text if reply else ""):
            message.delivery = "failed"
            self.persist()

    def retry(self, message):
        if self.status != "online":
            self.w.notice("Reconnect before retrying.")
            return
        dialog = Adw.AlertDialog(heading="Retry Message?", body="The recipient may already have received it. Retrying can create a duplicate on clients that do not deduplicate message IDs.")
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("send", "Retry")
        def confirmed(_, response):
            if response == "send" and message in self.w.state.active.messages:
                message.delivery = "sending"
                if self.persist():
                    self.dispatch(message)
        dialog.connect("response", confirmed)
        dialog.present(self.w)

    def load_history(self, initial=False):
        state = self.w.state
        if not state.account:
            return
        chat = state.active
        if chat.messages and not initial:
            first = chat.messages[0]
            older = self.store.history(state.account, chat.jid, before=(first.timestamp, first.id), limit=100)
            chat.messages[:0] = older
            if older:
                self.w.chat.render(False)
                return
        page = {} if initial else self.history_pages.get(chat.jid, {})
        if page.get("complete"):
            self.w.notice("Beginning of available history")
            return
        if self.status != "online" or chat.jid in self.history_pending:
            return
        self.history_pending.add(chat.jid)
        self.w.chat.history_button.set_sensitive(False)
        self.w.chat.history_button.set_label("Loading History…")
        self.session.call("history", chat.jid, page.get("before", ""))

    def typing(self):
        if self.status != "online" or not self.session or not self.w.state.account or not self.preference("typing",True):
            return
        now = GLib.get_monotonic_time()
        if now - self.last_typing < 5_000_000:
            return
        self.last_typing = now
        peer = self.w.state.active.jid
        self.session.call("send_typing", peer, bool(self.w.chat.text()))

    def respond_request(self, item, allow):
        if self.status != "online" or not self.session:
            self.w.notice("Reconnect to respond to this request.")
            return False
        return self.session.call("authorize",item[0],allow)

    def update_status(self):
        w = self.w
        if not w.state.account:
            w.chat.live_banner.set_revealed(False)
            return
        text = self.status_text + " · Messages are not end-to-end encrypted"
        w.chat.live_banner.set_title(text)
        w.chat.live_banner.set_button_label("Account")
        w.chat.live_banner.set_revealed(True)
        if w.pages.get_visible_child_name() == "chat":
            w.show_chat()

    def subtitle(self, chat):
        if self.typing_until.get(chat.jid, 0) > GLib.get_monotonic_time():
            return "Typing…"
        return chat.subtitle

    def tick(self):
        expired = [peer for peer, until in self.typing_until.items() if until <= GLib.get_monotonic_time()]
        for peer in expired:
            del self.typing_until[peer]
        if expired:
            self.update_status()
        return True

    def notify(self, chat, message):
        if chat.notifications != "all" or not self.preference("notifications",True):
            return
        notification = Gio.Notification.new(chat.name)
        notification.set_body(message.text[:200] if self.preference("message_previews",False) else "New message")
        self.w.get_application().send_notification("message-" + message.id, notification)

    def preference(self,key,default=False):
        return self.store.preference(key+":"+self.w.state.account,"true" if default else "false")=="true"

    def preferences(self):
        owner=self.w.state.account
        dialog=Adw.PreferencesDialog(title="Preferences",search_enabled=True)
        page=Adw.PreferencesPage(title="General",icon_name="preferences-system-symbolic")
        appearance=group("Appearance");page.add(appearance)
        theme=Adw.ComboRow(title="Style",model=Gtk.StringList.new(["System","Light","Dark"]))
        schemes=[Adw.ColorScheme.DEFAULT,Adw.ColorScheme.FORCE_LIGHT,Adw.ColorScheme.FORCE_DARK]
        values=["system","light","dark"];theme.set_selected(values.index(self.store.preference("style","system")))
        def style(row,*_):
            Adw.StyleManager.get_default().set_color_scheme(schemes[row.get_selected()]);self.store.set_preference("style",values[row.get_selected()])
        theme.connect("notify::selected",style);appearance.add(theme)
        emoji=Adw.SwitchRow(title="Large emoji",active=self.w.big_emoji)
        emoji.connect("notify::active",lambda row,*_:(self.w.set_big_emoji(row.get_active()),self.store.set_preference("big_emoji","true" if row.get_active() else "false")));appearance.add(emoji)
        privacy=group("Privacy",owner);page.add(privacy)
        for key,title,subtitle,default in [("typing","Share typing status","Let the other person know when you are composing",True),("delivery_receipts","Send delivery receipts","Confirm receipt by this client; this does not send read receipts",False),("notifications","Desktop notifications","Notify when another conversation receives a message",True),("message_previews","Message previews","Show message text in desktop notifications",False)]:
            row=Adw.SwitchRow(title=title,subtitle=subtitle,active=self.preference(key,default))
            def changed(widget,*_,key=key):self.store.set_preference(key+":"+owner,"true" if widget.get_active() else "false")
            row.connect("notify::active",changed);privacy.add(row)
        accounts=group("Account");page.add(accounts)
        accounts.add(action_row("Accounts","Connect, disconnect and remove local account data",callback=self.manager))
        accounts.add(action_row("Encryption","Review the encryption status of this build",callback=self.w.panels.security))
        dialog.add(page);self.w.panels.last_dialog=dialog;dialog.present(self.w)

    def manager(self):
        page = Adw.PreferencesPage()
        accounts = group("XMPP Accounts", "Passwords are held in memory for the current connection and are never saved.")
        page.add(accounts)
        for config in self.store.accounts():
            jid = config["jid"]
            row = Adw.ActionRow(title=jid, subtitle=self.status_text if self.w.state.account == jid else "Saved account", use_markup=False)
            controls = box(False, 6, valign=Gtk.Align.CENTER)
            connect = Gtk.Button(label="Connect" if self.w.state.account != jid or self.status != "online" else "Reconnect")
            connect.connect("clicked", lambda _, c=config: (dialog.close(), self.login(c)))
            controls.append(connect)
            controls.append(button("user-trash-symbolic", "Remove Account", lambda j=jid: self.remove(j, dialog)))
            row.add_suffix(controls)
            accounts.add(row)
        options = group("Connection")
        page.add(options)
        options.add(action_row("Add Account…", callback=lambda: (dialog.close(), self.login())))
        if self.w.state.account:
            options.add(action_row("Disconnect", "Keep history and drafts on this computer", callback=lambda: (self.w.save_draft(), self.persist(), self.disconnect(), self.update_status(), dialog.close())))
        options.add(action_row("Open Offline Demo", "Fictional data, separate from your accounts", callback=lambda: (dialog.close(), self.activate(self.preview))))
        dialog = self.w.panels.dialog("Accounts", page, 560, 480)

    def new_chat(self):
        page=Adw.PreferencesPage();g=group("Direct Conversation","Opening a conversation does not send a message or subscribe to presence.")
        address=Adw.EntryRow(title="XMPP address");g.add(address);page.add(g)
        dialog=self.w.panels.dialog("New Conversation",page,480,280)
        open_button=Gtk.Button(label="Open");open_button.add_css_class("suggested-action");dialog.header.pack_end(open_button)
        def open_chat(*_):
            try:peer=bare_jid(address.get_text())
            except ValueError:
                self.w.notice("Enter a valid XMPP address, without a resource.");return
            self.w.select_chat(self.w.state.chat_for(peer));self.persist();dialog.close()
        open_button.connect("clicked",open_chat)
        address.connect("changed",lambda row:open_button.set_sensitive(bool(row.get_text().strip())))
        open_button.set_sensitive(False)

    def login(self, existing=None):
        config = existing or {"jid": "", "host": "", "port": 5222, "direct_tls": False}
        page = Adw.PreferencesPage()
        account = group("Sign In", "Direct text messaging over verified TLS. End-to-end encryption is not available in this desktop build yet.")
        jid = Adw.EntryRow(title="XMPP address")
        jid.set_text(config["jid"])
        password = Adw.PasswordEntryRow(title="Password")
        account.add(jid)
        account.add(password)
        page.add(account)
        advanced = group("Connection")
        host = Adw.EntryRow(title="Server override", text=config["host"])
        port = Adw.EntryRow(title="Port", text=str(config["port"]), input_purpose=Gtk.InputPurpose.DIGITS)
        direct = Adw.SwitchRow(title="Direct TLS", subtitle="Leave off for STARTTLS", active=config["direct_tls"])
        advanced.add(host)
        advanced.add(port)
        advanced.add(direct)
        page.add(advanced)
        advanced.set_description("Leave server override empty to discover the service using DNS. Certificates are verified for your XMPP domain.")
        error = label("", "error", wrap=True, xalign=0)
        content = box(True)
        content.append(page)
        content.append(error)
        error.set_margin_start(24)
        error.set_margin_end(24)
        dialog = self.w.panels.dialog("Connect Account", scrolled(content), 500, 600)
        dialog.connect("closed",lambda _:password.set_text(""))
        connect = Gtk.Button(label="Connect")
        connect.add_css_class("suggested-action")
        dialog.header.pack_end(connect)
        def validate(*_):
            connect.set_sensitive(bool(jid.get_text().strip() and password.get_text()))
        def submit(*_):
            try:
                address = bare_jid(jid.get_text())
                server = host.get_text().strip()
                if any(c.isspace() for c in server) or "/" in server:
                    raise ValueError("Enter only a server hostname or IP address.")
                if not port.get_text().isascii() or not port.get_text().isdigit() or not 1 <= int(port.get_text()) <= 65535:
                    raise ValueError("Port must be between 1 and 65535.")
                chosen = {"jid": address, "host": server, "port": int(port.get_text()), "direct_tls": direct.get_active()}
                if not server and chosen["port"] != (5223 if chosen["direct_tls"] else 5222):
                    chosen["host"] = address.split("@", 1)[1]
                secret = password.get_text()
                password.set_text("")
                self.connect(chosen, secret)
                dialog.close()
            except (ValueError, OSError, sqlite3.Error) as exc:
                error.set_text(str(exc) if isinstance(exc, ValueError) else "Local account storage is unavailable.")
        direct.connect("notify::active", lambda r, *_: port.set_text("5223" if r.get_active() else "5222"))
        jid.connect("changed", validate)
        password.connect("changed", validate)
        connect.connect("clicked", submit)
        validate()

    def remove(self, jid, parent):
        dialog = Adw.AlertDialog(heading="Remove Account?", body="This removes its history and drafts from this computer. Messages stored by the server are unaffected.")
        dialog.add_response("cancel", "Cancel")
        dialog.add_response("remove", "Remove")
        dialog.set_response_appearance("remove", Adw.ResponseAppearance.DESTRUCTIVE)
        def confirmed(_, response):
            if response != "remove":
                return
            if self.w.state.account == jid:
                self.activate(self.preview)
            self.store.forget(jid)
            self.store.set_preference("folders:" + jid, "")
            parent.close()
            self.manager()
        dialog.connect("response", confirmed)
        dialog.present(self.w)

    def closing(self, *_):
        self.w.save_draft()
        self.persist()
        self.disconnect()
        if self.tick_id:
            GLib.source_remove(self.tick_id)
            self.tick_id = 0
        return False
