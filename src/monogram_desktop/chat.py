from pathlib import Path
from gi.repository import Gtk, Gdk, Gio, GLib, Graphene, Pango
from .widgets import box, label, button, margins, avatar, scrolled

class Conversation:
    def __init__(self, window):
        self.window = window
        self.root = box(True)
        self.scroll = Gtk.ScrolledWindow(vexpand=True, hscrollbar_policy=Gtk.PolicyType.NEVER)
        self.messages = box(True, 13)
        self.messages.add_css_class("messages")
        self.scroll.set_child(self.messages)
        self.root.append(self.scroll)
        self.make_composer()
        self.bubbles={}
        self.replying = None
        self.editing = None
        self.attachments = []

    def make_composer(self):
        shell = box(True, 0)
        shell.add_css_class("composer-shell")
        self.root.append(shell)
        self.context = Gtk.Revealer()
        self.context_row = box(False, 8)
        self.context_label = label("", xalign=0, hexpand=True)
        self.context_row.append(self.context_label)
        self.context_row.append(button("window-close-symbolic", "Cancel reply or edit", self.cancel_context))
        self.context.set_child(self.context_row)
        shell.append(self.context)
        self.chips = box(False, 6)
        self.chips.set_visible(False);self.chips.set_margin_bottom(8)
        shell.append(self.chips)
        composer = box(False, 4)
        composer.add_css_class("composer")
        attach = Gtk.MenuButton(icon_name="mail-attachment-symbolic", tooltip_text="Attach")
        attach.add_css_class("flat")
        menu = Gio.Menu()
        for name, action in [("Photo or Video…", "win.attach-media"), ("File…", "win.attach-file"), ("Poll…", "win.poll"), ("Payment…", "win.payment"), ("Location…", "win.location"), ("Article…", "win.article")]: menu.append(name, action)
        attach.set_menu_model(menu)
        composer.append(attach)
        self.entry = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR, accepts_tab=False, hexpand=True)
        self.entry.set_top_margin(0)
        self.entry.set_bottom_margin(0)
        self.entry.set_tooltip_text("Message · Enter to send, Shift+Enter for a new line")
        self.entry.update_property([Gtk.AccessibleProperty.LABEL], ["Message"])
        input_scroll = Gtk.ScrolledWindow(child=self.entry, min_content_height=24, max_content_height=96, propagate_natural_height=True, hexpand=True)
        input_scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.NEVER)
        self.input_scroll=input_scroll
        composer.append(input_scroll)
        keys = Gtk.EventControllerKey()
        keys.connect("key-pressed", self.key)
        self.entry.add_controller(keys)
        composer.append(button("face-smile-symbolic", "Emoji, stickers and GIFs · F9", self.window.toggle_media))
        self.send_button = button("mail-send-symbolic", "Send locally", self.send, "suggested-action send")
        plane=Gtk.Image.new_from_file(str(Path(__file__).with_name("paper-plane-symbolic.svg")));plane.set_pixel_size(20);self.send_button.set_child(plane)
        composer.set_hexpand(True)
        line=box(False,8);line.append(composer);line.append(self.send_button);shell.append(line)
        self.entry.get_buffer().connect("changed", lambda _: self.update_send())
        self.update_send()

    def text(self):
        b = self.entry.get_buffer()
        return b.get_text(b.get_start_iter(), b.get_end_iter(), True)

    def update_send(self):
        lines=self.entry.get_buffer().get_line_count()
        self.input_scroll.set_min_content_height(min(96,max(28,lines*20)))
        self.input_scroll.set_policy(Gtk.PolicyType.NEVER,Gtk.PolicyType.AUTOMATIC if lines>4 else Gtk.PolicyType.NEVER)
        self.send_button.set_sensitive(bool(self.text().strip() or getattr(self, "attachments", [])))

    def key(self, controller, keyval, code, state):
        if keyval in (Gdk.KEY_Return, Gdk.KEY_KP_Enter) and not state & Gdk.ModifierType.SHIFT_MASK:
            self.send(); return True
        if keyval == Gdk.KEY_Escape:
            self.cancel_context(); return True
        return False

    def insert(self, text):
        self.entry.get_buffer().insert_at_cursor(text)
        self.entry.grab_focus()

    def cancel_context(self):
        self.replying = self.editing = None
        self.context.set_reveal_child(False)

    def reply(self, message):
        self.replying = message; self.editing = None
        self.context_label.set_text("Reply to " + message.author + " · " + message.text[:60])
        self.context.set_reveal_child(True)
        self.entry.grab_focus()

    def edit(self, message):
        bubble=self.bubbles[id(message)]
        while bubble.get_first_child():bubble.remove(bubble.get_first_child())
        editor=Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR);editor.get_buffer().set_text(message.text)
        bubble.append(scrolled(editor,min_content_height=60))
        actions=box(False,6,halign=Gtk.Align.END)
        cancel=Gtk.Button(label="Cancel");cancel.connect("clicked",lambda _:self.render());actions.append(cancel)
        save=Gtk.Button(label="Save");save.add_css_class("suggested-action")
        def commit():
            buf=editor.get_buffer();message.text=buf.get_text(buf.get_start_iter(),buf.get_end_iter(),True);self.render()
        save.connect("clicked",lambda _:commit());actions.append(save);bubble.append(actions);editor.grab_focus()

    def send(self):
        text = self.text().strip()
        if not text and not self.attachments: return
        if self.editing:
            self.editing.text = text
        else:
            if text:
                msg = self.window.state.send(text)
                if self.replying: msg.reply = self.replying.author + " · " + self.replying.text[:80]
            for name in self.attachments: self.window.state.send(name, "file")
        self.entry.get_buffer().set_text("")
        self.attachments.clear();self.chips.set_visible(False)
        while self.chips.get_first_child(): self.chips.remove(self.chips.get_first_child())
        self.cancel_context()
        self.render()
        self.window.refresh_chats()

    def add_files(self, files):
        self.chips.set_visible(True)
        for f in files:
            name = f.get_basename()
            self.attachments.append(name)
            chip = box(False, 4)
            chip.add_css_class("file-chip")
            chip.append(label(name))
            def remove(widget=chip, filename=name):
                self.chips.remove(widget)
                self.attachments.remove(filename)
                self.update_send()
            chip.append(button("window-close-symbolic", "Remove " + name, remove))
            self.chips.append(chip)
        self.update_send()

    def render(self):
        self.bubbles.clear()
        while self.messages.get_first_child(): self.messages.remove(self.messages.get_first_child())
        self.messages.append(label("Today", "date-pill", halign=Gtk.Align.CENTER))
        chat = self.window.state.active
        for message in chat.messages: self.messages.append(self.message_widget(message))
        GLib.idle_add(self.scroll_bottom)

    def scroll_bottom(self):
        adjustment = self.scroll.get_vadjustment()
        adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())
        return False

    def message_widget(self, message):
        row = box(False, 8)
        row.set_halign(Gtk.Align.END if message.own else Gtk.Align.START)
        if self.window.state.active.group and not message.own:
            a = Gtk.Box(width_request=26, valign=Gtk.Align.END)
            who = type("Person", (), {"name": message.author, "id": "", "color": "purple"})
            a.append(avatar(who, 26)); row.append(a)
        bubble = box(True, 0)
        bubble.add_css_class("message-bubble")
        if message.own: bubble.add_css_class("own")
        self.bubbles[id(message)]=bubble
        heading=box(False,6)
        heading.append(label(message.author,"message-author",xalign=0))
        if message.encrypted:heading.append(Gtk.Image(icon_name="channel-secure-symbolic",pixel_size=11,tooltip_text="Encrypted fixture"))
        bubble.append(heading)
        if message.reply:
            reply=Gtk.Button();reply.add_css_class("message-reply")
            reply.set_child(label(message.reply,wrap=True,max_width_chars=40,xalign=0))
            def jump():
                target=message.reply.split(" · ",1)[-1]
                for candidate in self.window.state.active.messages:
                    if candidate.text.startswith(target):
                        widget=self.bubbles.get(id(candidate))
                        if widget:
                            widget.grab_focus();ok,rect=widget.compute_bounds(self.messages)
                            if ok:self.scroll.get_vadjustment().set_value(max(0,rect.get_y()-16))
                            widget.add_css_class("reply-target");GLib.timeout_add(1200,lambda:(widget.remove_css_class("reply-target"),False)[1])
                        break
            reply.connect("clicked",lambda _:jump());bubble.append(reply)
        if message.kind == "photo":
            image = Gtk.Picture.new_for_filename(str(Path(__file__).with_name("landscape.svg")))
            image.set_content_fit(Gtk.ContentFit.COVER)
            image.set_size_request(330, 164)
            image.add_css_class("photo-tile")
            image.set_margin_bottom(9);bubble.append(image)
            click = Gtk.GestureClick(); click.connect("released", lambda *_: self.window.panels.media_viewer(image))
            image.add_controller(click)
        if message.kind == "gif":
            from .gif_preview import GifPreview
            preview=GifPreview(getattr(message,"gif_index",0));preview.set_size_request(220,156);bubble.append(preview)
        if message.kind == "poll": self.poll(bubble,message)
        elif message.kind == "file":
            file = box(False, 10)
            file.append(Gtk.Image(icon_name="application-pdf-symbolic", pixel_size=32))
            words = box(True, 2); words.append(label(message.text if message.own else "Desktop notes.pdf", "heading", xalign=0)); words.append(label("284 KB · PDF document", "caption dim-label", xalign=0)); file.append(words)
            bubble.append(file)
            if not message.own: bubble.append(label(message.text, wrap=True, max_width_chars=48, xalign=0))
        elif message.kind == "sticker": bubble.append(label(message.text, "large-emoji"))
        elif message.kind == "payment":
            card = box(True, 5);card.add_css_class("payment-card")
            card.append(label("MONERO REQUEST", "caption"));card.append(label(message.text, "title-1"));card.append(label("Payment preview · no funds involved", "caption"));bubble.append(card)
        elif message.kind == "location":
            tile = box(True, 8);tile.append(Gtk.Image(icon_name="mark-location-symbolic", pixel_size=40));tile.append(label("Riverside Park", "heading"));tile.append(label("Meeting point · location preview", "caption dim-label"));margins(tile,16);bubble.append(tile)
        else:
            css = "large-emoji" if len(message.text) < 5 and self.window.big_emoji and any(ord(c) > 0x2000 for c in message.text) else "message-text"
            bubble.append(label(message.text, css, wrap=True, selectable=True, max_width_chars=52, xalign=0))
        meta = box(False, 4, halign=Gtk.Align.END)
        meta.add_css_class("message-meta")

        meta.append(label(message.time))
        if message.own: meta.append(Gtk.Image(icon_name="object-select-symbolic", pixel_size=12, tooltip_text="Stored in this preview"))
        bubble.append(meta)
        if message.reactions:
            reactions = box(False, 4)
            for reaction in message.reactions:
                b = Gtk.ToggleButton(label=reaction);b.add_css_class("reaction")
                b.connect("toggled",lambda widget,r=reaction:widget.set_label(r+" +1" if widget.get_active() else r));reactions.append(b)
            bubble.append(reactions)
        row.append(bubble)
        gesture = Gtk.GestureClick(button=3)
        gesture.connect("pressed", lambda g, n, x, y: self.context_menu(bubble, message, x, y))
        bubble.add_controller(gesture)
        return row

    def poll(self, bubble, message):
        bubble.append(label(message.text, "heading", wrap=True, xalign=0))
        bubble.append(label("Public poll · choose one", "caption dim-label", xalign=0))
        first = None
        for title, count in getattr(message,"options",[("The café by the park", 5), ("Somewhere with a view", 3), ("Let's stay in", 1)]):
            b = Gtk.ToggleButton(label=f"{title}    {count}")
            b.add_css_class("poll-answer")
            b.set_margin_top(6)
            if first: b.set_group(first)
            else: first = b
            b.connect("toggled", lambda widget: self.window.notice("Vote updated locally") if widget.get_active() else None)
            bubble.append(b)
        bubble.append(label("9 votes", "caption dim-label", xalign=0, margin_top=6))

    def context_menu(self, parent, message, x, y):
        popover = Gtk.Popover(has_arrow=False)
        popover.set_parent(parent)
        rect = Gdk.Rectangle();rect.x=int(x);rect.y=int(y);rect.width=1;rect.height=1;popover.set_pointing_to(rect)
        items = box(True, 2);margins(items,4)
        def action(text, callback):
            b = Gtk.Button(label=text, halign=Gtk.Align.FILL);b.add_css_class("flat")
            b.connect("clicked", lambda _: (popover.popdown(), callback()));items.append(b)
        action("Reply", lambda: self.reply(message))
        if message.own: action("Edit", lambda: self.edit(message))
        action("Copy", lambda: self.window.get_clipboard().set(message.text))
        action("Forward…", lambda: self.window.panels.forward(message))
        action("Message Details", lambda: self.window.panels.message_details(message))
        action("Delete", lambda: (self.window.state.active.messages.remove(message), self.render()))
        popover.set_child(items)
        popover.connect("closed", lambda _: popover.unparent())
        popover.popup()
