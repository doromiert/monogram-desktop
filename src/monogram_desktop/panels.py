from gi.repository import Adw, Gtk, GLib, Gio
from .widgets import box, button, label, margins, scrolled, group, action_row, avatar
from .fixtures import Chat

class Panels:
    def __init__(self, window): self.w = window

    def dialog(self, title, child, width=520, height=560):
        d = Adw.Dialog(content_width=width, content_height=height, title=title)
        view = Adw.ToolbarView()
        header = Adw.HeaderBar(title_widget=Adw.WindowTitle(title=title))
        d.header=header
        view.add_top_bar(header)
        view.set_content(child)
        d.set_child(view)
        self.last_dialog=d
        d.present(self.w)
        return d

    def body(self): return margins(box(True, 18), 24)

    def new_chat(self):
        content = self.body()
        entry = Gtk.SearchEntry(placeholder_text="Name or XMPP address")
        content.append(entry)
        contacts = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE);contacts.add_css_class("boxed-list")
        for chat in self.w.state.chats:
            if not chat.group and chat.id != "saved":
                row = action_row(chat.name, chat.name.lower()+"@example.org", callback=lambda c=chat: (d.close(), self.w.select_chat(c)))
                row.add_prefix(avatar(chat, 36));contacts.append(row)
        content.append(contacts)
        content.append(Gtk.Separator())
        b = Gtk.Button(label="New Group");b.connect("clicked", lambda _: (d.close(), self.new_group()));content.append(b)
        b = Gtk.Button(label="Join a Room or MIX Channel");b.connect("clicked", lambda _: (d.close(), self.spaces()));content.append(b)
        d = self.dialog("New Conversation", scrolled(content), 480)

    def new_group(self):
        content = self.body();g = group("Group details")
        name = Adw.EntryRow(title="Group name");g.add(name);content.append(g)
        content.append(label("Choose people", "heading", xalign=0))
        for person in ["Mira", "Alex", "Robin"]: content.append(Gtk.CheckButton(label=person))
        def create():
            title=name.get_text().strip() or "New group"
            chat=Chat("group-"+str(len(self.w.state.chats)),title,"3 members",title[0],"purple",group=True)
            self.w.state.chats.insert(0,chat);d.close();self.w.select_chat(chat)
        b=Gtk.Button(label="Create Preview Group");b.add_css_class("suggested-action");b.connect("clicked",lambda _:create());content.append(b)
        d=self.dialog("New Group",content)

    def requests(self):
        content = self.body()
        content.append(label("Requests and Invitations", "title-2", xalign=0))
        content.append(label("Choose who can contact you and see your presence.", wrap=True, xalign=0))
        for name, sub in list(self.w.state.requests):
            row = action_row(name, sub)
            g = Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE);g.add_css_class("boxed-list");g.append(row)
            actions=box(False,6,valign=Gtk.Align.CENTER)
            for text in ["Decline","Accept"]:
                b=Gtk.Button(label=text,valign=Gtk.Align.CENTER)
                if text=="Accept":b.add_css_class("suggested-action")
                def respond(widget=g,verb=text,item=(name,sub)):
                    content.remove(widget)
                    if item in self.w.state.requests:self.w.state.requests.remove(item)
                    self.w.request_badge.set_text(str(len(self.w.state.requests)))
                    self.w.request_badge.set_visible(bool(self.w.state.requests))
                    self.w.notice("Request accepted locally" if verb=="Accept" else "Request declined locally")
                b.connect("clicked",lambda _,callback=respond:callback())
                actions.append(b)
            row.add_suffix(actions);content.append(g)
        self.dialog("Requests",scrolled(content))

    def contacts(self):
        content=self.body();g=group("Contacts")
        for chat in self.w.state.chats:
            if not chat.group and chat.id!="saved":
                row=action_row(chat.name,chat.subtitle,callback=lambda c=chat:self.w.select_chat(c));row.add_prefix(avatar(chat));g.add(row)
        content.append(g)
        self.w.show_page("Contacts",scrolled(content))

    def security(self):
        content=self.body();g=group("Encryption", "Preview controls only. No real keys are loaded or changed.")
        mode=Adw.ComboRow(title="Encryption mode",model=Gtk.StringList.new(["PQ OMEMO2","Legacy OMEMO","Unencrypted"]))
        mode.set_selected(["PQ OMEMO2","Legacy OMEMO","Unencrypted"].index(self.w.state.mode))
        def changed(row,*_):
            self.w.state.mode=row.get_selected_item().get_string();self.w.notice("Preview encryption: "+self.w.state.mode)
        mode.connect("notify::selected",changed);g.add(mode)
        g.add(Adw.SwitchRow(title="Trust new fingerprints automatically",subtitle="Applies only to this fictional conversation",active=False));content.append(g)
        devices=group("Devices and fingerprints")
        for who,status in [("Mira · Laptop","Verified"),("Alex · Phone","Needs review"),("Your account · Other device","Needs review")]:
            row=action_row(who,status,callback=lambda n=who:self.fingerprint(n));devices.add(row)
        content.append(devices)
        content.append(label("Encryption indicators appear beside each message time. Unencrypted messages have no lock.", "caption dim-label",wrap=True,xalign=0))
        self.dialog("Conversation Security",scrolled(content),560)

    def fingerprint(self, name):
        dialog=Adw.AlertDialog(heading=name,body="Example fingerprint\n\nA12B C34D E56F 7890\n1234 5678 90AB CDEF\n\nCompare the real fingerprint through a trusted channel before approving a device.")
        dialog.add_response("cancel","Cancel");dialog.add_response("block","Block");dialog.add_response("trust","Trust")
        dialog.set_response_appearance("block",Adw.ResponseAppearance.DESTRUCTIVE)
        dialog.set_response_appearance("trust",Adw.ResponseAppearance.SUGGESTED)
        dialog.connect("response",lambda _,response:self.w.notice("Preview device decision: "+response) if response!="cancel" else None)
        dialog.present(self.w)

    def preferences(self):
        d=Adw.PreferencesDialog(title="Preferences",search_enabled=True)
        page=Adw.PreferencesPage(title="General",icon_name="preferences-system-symbolic")
        appearance=group("Appearance")
        theme=Adw.ComboRow(title="Style",model=Gtk.StringList.new(["System","Light","Dark"]))
        theme.connect("notify::selected",lambda r,*_:Adw.StyleManager.get_default().set_color_scheme([Adw.ColorScheme.DEFAULT,Adw.ColorScheme.FORCE_LIGHT,Adw.ColorScheme.FORCE_DARK][r.get_selected()]))
        appearance.add(theme)
        emojis=Adw.SwitchRow(title="Large emoji",subtitle="Enlarge messages containing only emoji",active=self.w.big_emoji)
        emojis.connect("notify::active",lambda r,*_:self.w.set_big_emoji(r.get_active()));appearance.add(emojis)
        appearance.add(Adw.SwitchRow(title="Use system emoji",active=True))
        appearance.add(action_row("UI icon packs","Import and manage ZIP packs",callback=lambda:self.w.choose_files(False)))
        page.add(appearance)
        notice=group("Notifications")
        notice.add(Adw.ComboRow(title="Default notifications",model=Gtk.StringList.new(["All messages","Mentions only","Muted"])))
        notice.add(Adw.SwitchRow(title="Message previews",active=True));page.add(notice)
        d.add(page)
        privacy=Adw.PreferencesPage(title="Privacy",icon_name="channel-secure-symbolic")
        for title,rows in [("Privacy and Security",["Send read receipts","Share typing status","Sync read state between devices","Show presence to contacts"]),("Media",["Load link previews","Load location previews","Download photos automatically"]),("Storage",["Keep local conversation history","Reduce background activity"])]:
            g=group(title)
            for text in rows:g.add(Adw.SwitchRow(title=text,active=True))
            privacy.add(g)
        privacy.add(group("Local preview", "Settings remain in memory for this prototype. No real data is deleted or synchronized."))
        d.add(privacy)
        account=Adw.PreferencesPage(title="Account",icon_name="avatar-default-symbolic")
        g=group("Preview account")
        for title,sub,callback in [("Profile","Name, avatar and profile fields",self.profile),("Connection","Server, reconnect and proxy",self.setup),("Devices and fingerprints","Review or block a device",self.security),("Contact access","Presence requests and authorizations",self.contact_access),("Developer Tools","Service discovery and ad-hoc commands",self.developer)]:g.add(action_row(title,sub,callback=callback))
        account.add(g);d.add(account)
        more=Adw.PreferencesPage(title="Chat",icon_name="mail-message-new-symbolic")
        for title,rows in [("Chat behavior",["Reply on double click","Show message details","Pause media when minimized","Remember drafts"]),("Messages",["Render Markdown articles","Show partial spoilers","Translate selected text"]),("Animation",["Animate stickers","Animate emoji","Play GIFs automatically"])]:
            g=group(title)
            for text in rows:g.add(Adw.SwitchRow(title=text,active=True))
            more.add(g)
        self.last_dialog=d
        d.add(more);d.present(self.w)

    def profile(self):
        content=self.body();a=Adw.Avatar(size=88,text="Preview account",show_initials=True);a.set_halign(Gtk.Align.CENTER);content.append(a)
        content.append(label("Preview account", "title-2"))
        g=group("Profile")
        for title,value in [("Display name","Alex Morgan"),("About","Making things that feel at home."),("Website","https://example.org")]:
            row=Adw.EntryRow(title=title);row.set_text(value);g.add(row)
        content.append(g)
        b=Gtk.Button(label="Save Preview Profile");b.add_css_class("suggested-action");b.connect("clicked",lambda _:(d.close(),self.w.notice("Profile updated locally")));content.append(b)
        d=self.dialog("Your Profile",scrolled(content))

    def contact_access(self):
        content=self.body();g=group("Presence and profile access")
        g.add(Adw.SwitchRow(title="Allow this contact to see your presence",active=True))
        g.add(Adw.SwitchRow(title="Request their presence",active=False));content.append(g)
        for name in ["Send Contact Request","Revoke Authorization"]:
            b=Gtk.Button(label=name);b.connect("clicked",lambda _,n=name:self.w.notice(n+" · local preview"));content.append(b)
        self.dialog("Control Access",content)

    def setup(self):
        content=self.body();content.append(label("Connect your account", "title-2"))
        content.append(label("This is an offline form preview. It will not attempt to sign in.",wrap=True))
        g=group("XMPP account");g.add(Adw.EntryRow(title="XMPP address"));g.add(Adw.PasswordEntryRow(title="Password"));content.append(g)
        b=Gtk.Button(label="Proxy Settings");b.connect("clicked",lambda _:self.simple_form("Proxy",["Host","Port","Username"]));content.append(b)
        b=Gtk.Button(label="Preview Connection");b.add_css_class("suggested-action");b.connect("clicked",lambda _:self.w.notice("Offline prototype · no connection attempted"));content.append(b)
        self.dialog("Account Setup",content)

    def simple_form(self,title,fields):
        content=self.body();g=group(title)
        for f in fields:g.add(Adw.EntryRow(title=f))
        content.append(g);content.append(label("Local form preview", "caption dim-label"))
        return self.dialog(title,content)

    def developer(self):
        content=self.body();g=group("example.org")
        for text in ["Service Discovery","Ad-hoc Commands","Public Key Nodes","Message Diagnostics"]:g.add(action_row(text,"Fictional service data",callback=lambda t=text:self.simple_form(t,["Service address"])))
        content.append(g);self.dialog("Developer Tools",content)

    def poll(self):
        content=self.body();g=group("Question");q=Adw.EntryRow(title="Ask a question");g.add(q);content.append(g)
        answers=group("Options");fields=[]
        for title in ["First option","Second option","Third option"]:
            field=Adw.EntryRow(title=title);fields.append(field);answers.add(field)
        content.append(answers);content.append(Gtk.CheckButton(label="Allow multiple answers"))
        def send():
            message=self.w.state.send(q.get_text() or "Where should the next meetup be?","poll")
            options=[(entry.get_text(),0) for entry in fields if entry.get_text().strip()]
            if len(options)>=2:message.options=options
            self.w.chat.render();d.close()
        b=Gtk.Button(label="Create Preview Poll");b.add_css_class("suggested-action");b.connect("clicked",lambda _:send());content.append(b)
        d=self.dialog("New Poll",content)

    def article(self):
        content=self.body();content.append(label("Article", "title-2",xalign=0))
        editor=Gtk.TextView(wrap_mode=Gtk.WrapMode.WORD_CHAR,vexpand=True)
        editor.get_buffer().set_text("# A little room to write\n\nUse Markdown for headings, lists, tables and quotes.\n\n| Item | Count |\n| --- | --- |\n| Ideas | 3 |")
        content.append(scrolled(editor,min_content_height=280))
        def send():
            b=editor.get_buffer();self.w.state.send(b.get_text(b.get_start_iter(),b.get_end_iter(),True),"article");self.w.chat.render();d.close()
        b=Gtk.Button(label="Add Preview Article");b.add_css_class("suggested-action");b.connect("clicked",lambda _:send());content.append(b)
        d=self.dialog("Write an Article",content,640)

    def location(self):
        content=self.body();content.append(Gtk.Image(icon_name="mark-location-symbolic",pixel_size=72));content.append(label("Riverside Park", "title-2"));content.append(label("Meeting point preview", "dim-label"))
        b=Gtk.Button(label="Add Preview Location");b.add_css_class("suggested-action");b.connect("clicked",lambda _:(self.w.state.send("Riverside Park","location"),self.w.chat.render(),d.close()));content.append(b)
        d=self.dialog("Share Location",content)

    def payment(self,request=False):
        content=self.body();g=group("Request Monero" if request else "Send Monero", "Preview only · no real wallet or funds")
        amount=Adw.EntryRow(title="Amount",input_purpose=Gtk.InputPurpose.NUMBER);g.add(amount)
        currency=Adw.ComboRow(title="Currency",model=Gtk.StringList.new(["XMR","USD","EUR","PLN"]));g.add(currency)
        g.add(Adw.EntryRow(title="Note"))
        if not request:g.add(Adw.EntryRow(title="Receiving address"))
        content.append(g)
        content.append(Adw.SwitchRow(title="Remember address",active=False))
        def done():
            if request:self.w.state.send((amount.get_text() or "25")+" "+currency.get_selected_item().get_string(),"payment");self.w.chat.render();self.w.show_chat()
            else:self.w.notice("Payment review preview · nothing was sent")
            d.close()
        b=Gtk.Button(label="Create Preview Request" if request else "Review Preview Payment");b.add_css_class("suggested-action");b.connect("clicked",lambda _:done());content.append(b)
        from .amounts import bind
        bind(amount,b)
        d=self.dialog("Request Payment" if request else "Send Payment",content)

    def wallet(self):
        content=self.body()
        title=box(False,8);title.append(label("Your wallets", "title-2",hexpand=True,xalign=0));title.append(button("list-add-symbolic","Add or create another wallet",self.add_wallet));content.append(title)
        from .wallet_card import WalletCard
        switch=box(False,4,halign=Gtk.Align.FILL)
        card_stack=Gtk.Stack(transition_duration=320,hexpand=True)
        cards={};self.wallet_stack=card_stack
        def fill():
            index=self.w.state.wallet_index
            if index not in cards:
                cards[index]=WalletCard(self.w.state.wallets[index],"1.2048 XMR" if index==0 else "0.4659 XMR",self.wallet_qr)
                card_stack.add_child(cards[index])
            card_stack.set_visible_child(cards[index])
        def change(direction):
            self.w.state.wallet_index=(self.w.state.wallet_index+direction)%len(self.w.state.wallets)
            card_stack.set_transition_type(Gtk.StackTransitionType.SLIDE_LEFT if direction>0 else Gtk.StackTransitionType.SLIDE_RIGHT);fill()
        switch.append(button("go-previous-symbolic","Previous wallet",lambda:change(-1)))
        switch.append(card_stack)
        switch.append(button("go-next-symbolic","Next wallet",lambda:change(1)))
        fill();content.append(switch)
        actions=box(False,12,homogeneous=True)
        for text,icon,cb in [("Send","mail-send-symbolic",lambda:self.payment()),("Request","insert-link-symbolic",lambda:self.payment(True)),("Wallets","view-grid-symbolic",self.manage_wallets)]:
            b=Gtk.Button(label=text);b.connect("clicked",lambda _,f=cb:f());actions.append(b)
        content.append(actions)
        g=group("Activity")
        for title,sub,value in [("Received","Mira · Today, 09:42","+0.0248 XMR"),("Sent","Everyday → Savings · Yesterday","−0.1000 XMR")]:
            row=action_row(title,sub);row.add_suffix(label(value,"heading"));g.add(row)
        content.append(g);content.append(label("Offline wallet preview. No keys, scanning or transactions.","caption dim-label",wrap=True))
        self.w.utility.set_show_sidebar(False)
        self.w.show_page("Wallet",scrolled(Adw.Clamp(child=content,maximum_size=640,tightening_threshold=540)))

    def wallet_qr(self,name):
        import qrcode,cairo
        qr=qrcode.QRCode(border=4);qr.add_data("Monogram Desktop wallet preview: "+name+" (no receiving address)");qr.make(fit=True)
        matrix=qr.get_matrix()
        content=self.body()
        canvas=Gtk.DrawingArea(content_width=280,content_height=280,halign=Gtk.Align.CENTER)
        def draw(area,c,width,height):
            cell=max(1,int(min(width,height)/len(matrix)));side=cell*len(matrix)
            c.set_source_rgb(1,1,1);c.rectangle((width-side)/2,(height-side)/2,side,side);c.fill()
            c.translate((width-side)/2,(height-side)/2);c.set_antialias(cairo.ANTIALIAS_NONE);c.set_source_rgb(0,0,0)
            for y,row in enumerate(matrix):
                for x,filled in enumerate(row):
                    if filled:c.rectangle(x*cell,y*cell,cell,cell)
            c.fill()
        canvas.set_draw_func(draw);canvas.update_property([Gtk.AccessibleProperty.LABEL],["Scannable QR preview for "+name]);content.append(canvas)
        content.append(label(name,"title-2"))
        content.append(label("Offline preview · no receiving address", "dim-label",wrap=True))
        self.dialog("Receive Monero",content,360,410)

    def add_wallet(self):
        content=self.body();g=group("Another wallet");name=Adw.EntryRow(title="Wallet name");g.add(name);content.append(g)
        def add():
            self.w.state.wallets.append(name.get_text().strip() or "New wallet");self.w.state.wallet_index=len(self.w.state.wallets)-1;d.close();self.wallet()
        for title in ["Create Preview Wallet","Restore Preview Wallet"]:
            b=Gtk.Button(label=title);b.connect("clicked",lambda _:add());content.append(b)
        d=self.dialog("Add or Create Another Wallet",content)

    def manage_wallets(self):
        content=self.body();g=group("Wallets", "Remove any preview wallet directly; no selection required.")
        for name in list(self.w.state.wallets):
            row=action_row(name,"Fictional wallet")
            def delete(n=name,r=row):
                if len(self.w.state.wallets)==1:self.w.notice("Keep one wallet in this preview");return
                self.w.state.wallets.remove(n);self.w.state.wallet_index=0;g.remove(r);self.wallet()
            row.add_suffix(button("user-trash-symbolic","Remove "+name,delete));g.add(row)
        content.append(g);self.dialog("Manage Wallets",content)

    def spaces(self):
        content=self.body();g=group("Spaces and Channels")
        for title,sub in [("Open source café","Community space · 4 channels"),("Design room","MUC · 8 members"),("Photography","MIX channel · 12 participants")]:g.add(action_row(title,sub,icon="system-users-symbolic",callback=lambda:self.w.select_chat(self.w.state.chats[0])))
        content.append(g)
        b=Gtk.Button(label="Create Space or Channel");b.connect("clicked",lambda _:self.simple_form("Create Channel",["Service","Channel name"]));content.append(b)
        self.dialog("Spaces and Channels",content)

    def stories(self):
        self.w.reveal_stories()

    def story_editor(self):
        chooser=Gtk.FileDialog(title="Add Story",modal=True)
        filt=Gtk.FileFilter(name="Photos");filt.add_mime_type("image/*");filters=Gio.ListStore.new(Gtk.FileFilter);filters.append(filt);chooser.set_filters(filters)
        def chosen(dialog,result):
            try:file=dialog.open_finish(result)
            except GLib.Error as error:
                if not error.matches(Gtk.DialogError.quark(),Gtk.DialogError.DISMISSED):self.w.notice("Photo selection unavailable")
                return
            if not file.get_path():self.w.notice("This preview needs a local photo");return
            content=Adw.PreferencesPage()
            photo=Gtk.Picture.new_for_file(file);photo.set_size_request(280,200);photo.set_content_fit(Gtk.ContentFit.CONTAIN);photo.set_can_shrink(True)
            preview=group("Photo");preview.add(photo);content.add(preview)
            details=group("Story");caption=Adw.EntryRow(title="Caption");details.add(caption)
            details.add(Adw.ComboRow(title="Expires after",model=Gtk.StringList.new(["24 hours"])))
            details.add(Adw.ComboRow(title="Visible to",model=Gtk.StringList.new(["Contacts","Everyone"])))
            details.set_description("Local preview. Nothing will be published.");content.add(details)
            d=self.dialog("New Story",content,480,600)
            post=Gtk.Button(label="Post Preview");post.add_css_class("suggested-action");d.header.pack_end(post)
            def save():
                from .fixtures import Story
                self.w.state.stories.insert(0,Story("You",file.get_path(),caption.get_text(),own=True));self.w.story_strip.refresh();d.close();self.w.reveal_stories()
            post.connect("clicked",lambda _:save())
        chooser.open(self.w,None,chosen)

    def call(self):
        content=self.body();content.append(avatar(self.w.state.active,88));content.append(label(self.w.state.active.name,"title-1"));content.append(label("Call preview · no call will be placed","dim-label"))
        controls=box(False,16,halign=Gtk.Align.CENTER)
        for icon,title in [("microphone-sensitivity-high-symbolic","Microphone"),("camera-web-symbolic","Camera"),("audio-volume-high-symbolic","Speaker")]:
            b=Gtk.ToggleButton(icon_name=icon,tooltip_text=title,active=True);b.add_css_class("circular");controls.append(b)
        content.append(controls)
        b=Gtk.Button(label="Close Preview");b.add_css_class("destructive-action");b.connect("clicked",lambda _:d.close());content.append(b)
        d=self.dialog("Group Call" if self.w.state.active.group else "Call",content)

    def media_viewer(self,source=None):
        from .viewer import MediaViewer
        viewer=MediaViewer(self.w,source)
        self.w.viewer_layer.add_overlay(viewer)
        viewer.open()

    def forward(self,message):
        self.forward_many([message])

    def forward_many(self,messages):
        if not messages:return
        content=self.body();g=group("Choose a conversation")
        for chat in self.w.state.chats:
            def send(c=chat):
                import copy
                self.w.select_chat(c)
                for message in messages:
                    sent=self.w.state.send("↪ Forwarded from "+message.author+"\n\n"+message.text,message.kind)
                    for key in ("options","gif_index"):
                        if hasattr(message,key):setattr(sent,key,copy.deepcopy(getattr(message,key)))
                self.w.chat.render();self.w.refresh_chats();d.close()
            g.add(action_row(chat.name,callback=send))
        content.append(g);d=self.dialog("Forward Message",scrolled(content))

    def message_details(self,message):
        content=self.body();g=group("Message")
        for title,sub in [("Sender",message.author),("Time",message.time),("Encryption","Encrypted fixture" if message.encrypted else "Unencrypted fixture"),("Delivery","Stored only in this in-memory preview")]:g.add(action_row(title,sub))
        content.append(g);self.dialog("Message Details",content)

    def report_message(self,message):
        dialog=Adw.AlertDialog(heading="Report Message",body="Choose a reason. This preview will not send a report.")
        for name,title in [("cancel","Cancel"),("spam","Spam"),("abuse","Abuse"),("other","Other")]:dialog.add_response(name,title)
        dialog.connect("response",lambda _,reason:setattr(message,"report_reason",reason) if reason!="cancel" else None);dialog.present(self.w)
