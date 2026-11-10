from pathlib import Path
from gi.repository import Adw, Gtk, Gdk, Gio, GLib, Graphene, Pango
from .widgets import box, label, button, margins, avatar, scrolled,AspectPicture

class Conversation:
    def __init__(self, window):
        self.window = window
        self.root = box(True)
        self.live_banner=Adw.Banner(title="Messages are not end-to-end encrypted",revealed=False)
        self.live_banner.connect("button-clicked",lambda _:self.window.accounts.manager());self.root.append(self.live_banner)
        self.history_button=Gtk.Button(label="Load Earlier Messages",visible=False)
        self.history_button.add_css_class("flat");self.history_button.connect("clicked",lambda _:self.window.accounts.load_history());self.root.append(self.history_button)
        self.scroll = Gtk.ScrolledWindow(vexpand=True, hscrollbar_policy=Gtk.PolicyType.NEVER)
        self.messages = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE,valign=Gtk.Align.START)
        self.messages.add_css_class("messages");self.messages.add_css_class("message-list")
        self.rows={};self.anchor=None;self.rendering=False
        self.messages.connect("selected-rows-changed",lambda _:self.selection_changed())
        selection_keys=Gtk.EventControllerKey();selection_keys.connect("key-pressed",self.selection_key);self.messages.add_controller(selection_keys)
        self.scroll.set_child(self.messages)
        self.root.append(self.scroll)
        self.selection_bar=Gtk.ActionBar(revealed=False)
        self.selection_count=label("", "heading");self.selection_bar.pack_start(self.selection_count)
        self.selection_bar.pack_start(button("window-close-symbolic","Clear selection",self.clear_selection))
        for icon,title,callback in [("edit-select-all-symbolic","Select all messages",self.select_all),("edit-copy-symbolic","Copy selected messages",self.copy_selected),("mail-forward-symbolic","Forward selected messages",lambda:self.window.panels.forward_many(self.selected_messages())),("user-trash-symbolic","Delete selected messages",self.delete_selected)]:self.selection_bar.pack_end(button(icon,title,callback))
        self.root.append(self.selection_bar)
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
        composer = box(False, 2)
        composer.add_css_class("composer")
        attach = Gtk.MenuButton(icon_name="mail-attachment-symbolic", tooltip_text="Attach")
        attach.add_css_class("flat")
        attach.set_valign(Gtk.Align.CENTER)
        menu = Gio.Menu()
        for name, action in [("Photo or Video…", "win.attach-media"), ("File…", "win.attach-file"), ("Poll…", "win.poll"), ("Payment…", "win.payment"), ("Location…", "win.location"), ("Article…", "win.article")]: menu.append(name, action)
        attach.set_menu_model(menu)
        composer.append(attach)
        self.entry = Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR, accepts_tab=False, hexpand=True)
        self.entry.set_top_margin(4)
        self.entry.set_bottom_margin(4)
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
        self.entry.get_buffer().connect("changed", lambda _:(self.update_send(),self.window.draft_changed()))
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
        self.window.draft_changed()

    def reply(self, message):
        self.replying = message; self.editing = None
        self.context_label.set_text("Reply to " + message.author + " · " + message.text[:60])
        self.context.set_reveal_child(True)
        self.window.draft_changed()
        self.entry.grab_focus()

    def edit(self, message):
        if self.window.state.account:self.window.notice("Live message corrections are not available yet.");return
        bubble=self.bubbles[id(message)]
        while bubble.get_first_child():bubble.remove(bubble.get_first_child())
        editor=Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR);editor.get_buffer().set_text(message.text)
        bubble.append(scrolled(editor,min_content_height=60))
        actions=box(False,6,halign=Gtk.Align.END)
        cancel=Gtk.Button(label="Cancel");cancel.connect("clicked",lambda _:self.render(False));actions.append(cancel)
        save=Gtk.Button(label="Save");save.add_css_class("suggested-action")
        def commit():
            buf=editor.get_buffer();message.text=buf.get_text(buf.get_start_iter(),buf.get_end_iter(),True);self.render(False)
        save.connect("clicked",lambda _:commit());actions.append(save);bubble.append(actions);editor.grab_focus()

    def send(self):
        text = self.text().strip()
        if not text and not self.attachments: return
        if self.editing:
            self.editing.text = text
        else:
            if text:
                msg = self.window.send_message(text)
                if msg is None:return
                if self.replying: msg.reply = self.replying.author + " · " + self.replying.text[:80]
            for name in self.attachments: self.window.send_message(name, "file")
        self.entry.get_buffer().set_text("")
        self.attachments.clear();self.chips.set_visible(False)
        while self.chips.get_first_child(): self.chips.remove(self.chips.get_first_child())
        self.cancel_context()
        self.window.save_draft()
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

    def render(self,scroll_to_bottom=True):
        selected={id(m) for m in self.selected_messages()};self.rendering=True;self.rows.clear()
        self.bubbles.clear()
        while self.messages.get_first_child(): self.messages.remove(self.messages.get_first_child())
        chat = self.window.state.active
        previous_day=None
        for message in chat.messages:
            day=message.timestamp[:10] if message.timestamp else "Today"
            if day!=previous_day:
                separator=Gtk.ListBoxRow(child=label(day,"date-pill",halign=Gtk.Align.CENTER),selectable=False,activatable=False)
                self.messages.append(separator);previous_day=day
            row=Gtk.ListBoxRow(child=self.message_widget(message),activatable=False);row.message=message;row.add_css_class("message-row")
            self.rows[id(message)]=row;self.messages.append(row)
            if id(message) in selected:self.messages.select_row(row)
            click=Gtk.GestureClick(button=1,propagation_phase=Gtk.PropagationPhase.CAPTURE)
            def choose(g,n,x,y,m=message,r=row):
                state=g.get_current_event_state()
                if state&(Gdk.ModifierType.CONTROL_MASK|Gdk.ModifierType.SHIFT_MASK) or self.messages.get_selection_mode()==Gtk.SelectionMode.MULTIPLE:
                    g.set_state(Gtk.EventSequenceState.CLAIMED);self.select_message(m,bool(state&Gdk.ModifierType.SHIFT_MASK))
            click.connect("pressed",choose);row.add_controller(click)
        self.rendering=False;self.selection_changed()
        if scroll_to_bottom:GLib.idle_add(self.scroll_bottom)

    def selected_messages(self):return [row.message for row in self.messages.get_selected_rows() if hasattr(row,"message")]

    def selection_changed(self):
        if self.rendering:return
        count=len(self.selected_messages());self.selection_bar.set_revealed(count>0);self.selection_count.set_text(f"{count} selected")
        if not count:self.messages.set_selection_mode(Gtk.SelectionMode.NONE)

    def select_message(self,message,extend=False):
        self.messages.set_selection_mode(Gtk.SelectionMode.MULTIPLE)
        row=self.rows[id(message)]
        if extend and self.anchor is not None:
            items=self.window.state.active.messages;lo,hi=sorted([items.index(self.anchor),items.index(message)])
            for item in items[lo:hi+1]:self.messages.select_row(self.rows[id(item)])
        else:
            self.messages.unselect_row(row) if row.is_selected() else self.messages.select_row(row)
            self.anchor=message
        row.grab_focus()

    def selection_key(self,controller,key,code,state):
        if not self.selected_messages():return False
        if state&Gdk.ModifierType.CONTROL_MASK and key in (Gdk.KEY_c,Gdk.KEY_a):
            self.copy_selected() if key==Gdk.KEY_c else self.select_all();return True
        if key==Gdk.KEY_Escape:self.clear_selection();return True
        if key==Gdk.KEY_Delete:self.delete_selected();return True
        return False

    def clear_selection(self):self.messages.unselect_all();self.anchor=None

    def select_all(self):self.messages.set_selection_mode(Gtk.SelectionMode.MULTIPLE);self.messages.select_all()

    def copy_selected(self):self.window.get_clipboard().set("\n\n".join(m.author+" · "+m.time+"\n"+m.text for m in self.selected_messages()))

    def delete_selected(self):
        selected=self.selected_messages()
        if not selected:return
        dialog=Adw.AlertDialog(heading=f"Delete {len(selected)} messages?",body="This removes them from this computer. Server history and other devices are unaffected." if self.window.state.account else "This removes them from the local preview.")
        dialog.add_response("cancel","Cancel");dialog.add_response("delete","Delete");dialog.set_response_appearance("delete",Adw.ResponseAppearance.DESTRUCTIVE)
        def delete(_,response):
            if response=="delete":
                self.window.delete_messages(selected)
        dialog.connect("response",delete);dialog.present(self.window)

    def react(self,message,emoji):
        if self.window.state.account:self.window.notice("Live reactions are not available yet.");return
        mine=emoji in message.my_reactions;change=-1 if mine else 1
        if mine:message.my_reactions.remove(emoji)
        else:message.my_reactions.add(emoji)
        for index,value in enumerate(message.reactions):
            if value.startswith(emoji+" "):
                count=int(value.rsplit(" ",1)[1])+change
                if count:message.reactions[index]=emoji+" "+str(count)
                else:message.reactions.pop(index)
                break
        else:message.reactions.append(emoji+" 1")
        self.render(False)

    def scroll_bottom(self):
        adjustment = self.scroll.get_vadjustment()
        adjustment.set_value(adjustment.get_upper() - adjustment.get_page_size())
        return False

    def highlight(self,message):
        widget=self.bubbles.get(id(message))
        if widget:
            ok,rect=widget.compute_bounds(self.messages)
            if ok:self.scroll.get_vadjustment().set_value(max(0,rect.get_y()-16))
            widget.add_css_class("reply-target");GLib.timeout_add(1200,lambda:(widget.remove_css_class("reply-target"),False)[1])

    def message_widget(self, message):
        row = box(False, 8,valign=Gtk.Align.START)
        row.set_halign(Gtk.Align.END if message.own else Gtk.Align.START)
        if self.window.state.active.group and not message.own:
            a = Gtk.Box(width_request=26, valign=Gtk.Align.END)
            who = type("Person", (), {"name": message.author, "id": "", "color": "purple"})
            a.append(avatar(who, 26)); row.append(a)
        bubble = box(True, 0,valign=Gtk.Align.START)
        bubble.add_css_class("message-bubble")
        if message.own: bubble.add_css_class("own")
        self.bubbles[id(message)]=bubble
        heading=box(False,6,margin_bottom=6)
        heading.append(label(message.author,"message-author",xalign=0,hexpand=True,valign=Gtk.Align.CENTER))
        meta = box(False,4,valign=Gtk.Align.CENTER);meta.add_css_class("message-meta")
        if message.encrypted:meta.append(Gtk.Image(icon_name="channel-secure-symbolic",pixel_size=11,tooltip_text="Encrypted fixture"))
        meta.append(label(message.time))
        if message.own:
            descriptions={"sending":"Sending","sent":"Sent to stream; awaiting delivery receipt","delivered":"Delivered to a recipient client, not a read receipt","failed":"Send failed; open the message menu to retry","uncertain":"Delivery is uncertain; retry may duplicate it","local":"Stored in this preview"}
            status=message.delivery
            icon="dialog-warning-symbolic" if status in ("failed","uncertain") else "content-loading-symbolic" if status=="sending" else "object-select-symbolic"
            meta.append(Gtk.Image(icon_name=icon,pixel_size=12,tooltip_text=descriptions.get(status,status)))
        heading.append(meta)
        if message.pinned:heading.prepend(Gtk.Image(icon_name="view-pin-symbolic",pixel_size=11))
        bubble.append(heading)
        if message.reply:
            reply=Gtk.Button();reply.add_css_class("message-reply")
            reply.set_child(label(message.reply,wrap=True,max_width_chars=40,xalign=0))
            def jump():
                if message.reply_id:
                    state=self.window.state
                    candidate=state.reply_target(state.active,message.reply_id,message.reply_to) if state.account else next((m for m in state.active.messages if m.wire_id==message.reply_id or m.id==message.reply_id),None)
                    if candidate:self.highlight(candidate)
                    else:self.window.notice("Original message is not loaded. Load earlier messages to find it.")
                    return
                target=message.reply.split(" · ",1)[-1]
                for candidate in self.window.state.active.messages:
                    if candidate.text.startswith(target):
                        self.highlight(candidate)
                        break
            reply.connect("clicked",lambda _:jump());bubble.append(reply)
        if message.kind == "photo":
            image = AspectPicture();image.set_filename(str(Path(__file__).with_name("landscape.svg")))
            image.set_content_fit(Gtk.ContentFit.CONTAIN);image.set_can_shrink(True)
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
            bubble.append(label(message.text, css, wrap=True,wrap_mode=Pango.WrapMode.WORD_CHAR, selectable=True, max_width_chars=52, xalign=0))
        if message.reactions:
            reactions = box(False, 4)
            for reaction in message.reactions:
                emoji=reaction.rsplit(" ",1)[0]
                b = Gtk.ToggleButton(label=reaction,active=emoji in message.my_reactions);b.add_css_class("reaction")
                b.connect("toggled",lambda widget,e=emoji:self.react(message,e));reactions.append(b)
            bubble.append(reactions)
        row.append(bubble)
        gesture = Gtk.GestureClick(button=3,propagation_phase=Gtk.PropagationPhase.CAPTURE)
        def menu(g,n,x,y):
            g.set_state(Gtk.EventSequenceState.CLAIMED);self.context_menu(bubble,message,x,y)
        gesture.connect("pressed",menu)
        bubble.add_controller(gesture)
        return row

    def poll(self, bubble, message):
        bubble.append(label(message.text, "heading", wrap=True, xalign=0))
        bubble.append(label("Public poll · choose one", "caption dim-label", xalign=0))
        first = None
        for title, count in message.options or [("The café by the park", 5), ("Somewhere with a view", 3), ("Let's stay in", 1)]:
            b = Gtk.ToggleButton()
            answer=box(False,16);answer.append(label(title,xalign=0,hexpand=True,wrap=True));answer.append(label(str(count),xalign=1));b.set_child(answer)
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
        popover.add_css_class("message-menu")
        def run(callback):
            popover.popdown()
            if popover.get_parent():popover.unparent()
            callback()
        content=box(True,8,width_request=260)
        reactions=box(False,0,homogeneous=True);reactions.add_css_class("message-menu-panel")
        for emoji in ["👍","❤️","😂","😮","😢","🔥"]:
            b=Gtk.Button(label=emoji);b.add_css_class("flat");b.add_css_class("reaction-choice");b.connect("clicked",lambda _,e=emoji:run(lambda:self.react(message,e)));reactions.append(b)
        if not self.window.state.account:content.append(reactions)
        items = box(True,2,halign=Gtk.Align.CENTER,width_request=208);items.add_css_class("message-menu-panel");content.append(items)
        def action(icon,text,callback):
            b=Gtk.Button();b.add_css_class("flat");line=box(False,12);line.append(Gtk.Image(icon_name=icon));line.append(label(text,xalign=0,hexpand=True));b.set_child(line)
            b.connect("clicked",lambda _:run(callback));items.append(b)
        if message.own and not self.window.state.account:action("document-edit-symbolic","Edit",lambda:self.edit(message))
        if message.own and message.delivery in ("failed","uncertain"):action("view-refresh-symbolic","Retry",lambda:self.window.accounts.retry(message))
        def pin():
            message.pinned=not message.pinned;self.render(False)
            if self.window.state.account:self.window.accounts.persist()
        action("view-pin-symbolic",("Unpin" if message.pinned else "Pin")+(" Locally" if self.window.state.account else ""),pin)
        action("object-select-symbolic","Select",lambda:self.select_message(message))
        if not message.own and not self.window.state.account:action("dialog-warning-symbolic","Report",lambda:self.window.panels.report_message(message))
        action("info-outline-symbolic","Message Details",lambda:self.window.panels.message_details(message))
        tools=box(False,0,homogeneous=True,halign=Gtk.Align.CENTER,width_request=208);tools.add_css_class("message-menu-panel");content.append(tools)
        for icon,title,callback in [("mail-reply-sender-symbolic","Reply",lambda:self.reply(message)),("user-trash-symbolic","Delete Locally" if self.window.state.account else "Delete",lambda:self.window.delete_messages([message])),("edit-copy-symbolic","Copy",lambda:self.window.get_clipboard().set(message.text)),("mail-forward-symbolic","Forward",lambda:self.window.panels.forward(message))]:tools.append(button(icon,title,lambda cb=callback:run(cb)))
        popover.set_child(content)
        popover.connect("closed", lambda _: popover.unparent() if popover.get_parent() else None)
        popover.popup()
