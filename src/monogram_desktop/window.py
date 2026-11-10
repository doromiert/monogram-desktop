from gi.repository import Adw, Gtk, Gdk, Gio, GLib, Pango
from .widgets import box, button, label, margins, scrolled, avatar, ellipsis
from .fixtures import PreviewState
from .chat import Conversation
from .panels import Panels

class MonogramWindow(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app,title="Monogram",default_width=1380,default_height=900)
        self.state=PreviewState();self.big_emoji=True;self.folder="All";self.utility_mode="media"
        self.panels=Panels(self)
        self.toast=Adw.ToastOverlay();self.set_content(self.toast)
        # Nautilus uses one ToolbarView per side of its outer OverlaySplitView.
        self.split=Adw.OverlaySplitView(min_sidebar_width=290,max_sidebar_width=360,sidebar_width_fraction=.25)
        self.split.set_sidebar_width_unit(Adw.LengthUnit.PX)
        self.toast.set_child(self.split)
        self.make_sidebar()
        self.make_content()
        self.utility.set_content(None)
        self.pages.remove(self.utility)
        self.pages.add_named(self.chat.root,"chat")
        self.toast.set_child(None)
        self.utility.set_content(self.split)
        self.viewer_layer=Gtk.Overlay(child=self.utility)
        self.toast.set_child(self.viewer_layer)
        self.make_actions()
        breakpoint=Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 760sp"))
        breakpoint.add_setter(self.split,"collapsed",True)
        breakpoint.add_setter(self.split,"show-sidebar",False)
        breakpoint.add_setter(self.utility,"collapsed",True)
        self.add_breakpoint(breakpoint)
        narrow=Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 1120sp"))
        narrow.add_setter(self.utility,"collapsed",True)
        self.add_breakpoint(narrow)
        self.select_chat(self.state.active)

    def notice(self,text): self.toast.add_toast(Adw.Toast.new(text))

    def make_actions(self):
        actions={
            "new-chat":self.panels.new_chat,"requests":self.panels.requests,"contacts":self.panels.contacts,
            "wallet":self.panels.wallet,"stories":self.panels.stories,"call":self.panels.call,
            "spaces":self.panels.spaces,"preferences":self.panels.preferences,"profile":self.panels.profile,
            "security":self.panels.security,"poll":self.panels.poll,"payment":lambda:self.panels.payment(True),
            "location":self.panels.location,"article":self.panels.article,"developer":self.panels.developer,
            "attach-file":lambda:self.choose_files(False),"attach-media":lambda:self.choose_files(True),
            "search":lambda:self.search.grab_focus(),"media":self.toggle_media,"info":self.show_info,
            "saved":lambda:self.select_chat(next(c for c in self.state.chats if c.id=="saved")),
            "shortcuts":self.shortcuts,"setup":self.panels.setup,
            "mute":lambda:self.notice("Notifications muted for this preview conversation"),
            "archive":lambda:self.notice("Archive management is planned for backend integration"),
        }
        for name,callback in actions.items():
            action=Gio.SimpleAction.new(name,None)
            action.connect("activate",lambda a,param,cb=callback:cb());self.add_action(action)
        for action,keys in {"attach-file":["<Control>o"],"new-chat":["<Control>n"],"search":["<Control>f"],"media":["F9"],"preferences":["<Control>comma"],"shortcuts":["<Control>question"]}.items():self.get_application().set_accels_for_action("win."+action,keys)

    def make_sidebar(self):
        view=Adw.ToolbarView();view.add_css_class("chat-sidebar")
        header=Adw.HeaderBar(show_end_title_buttons=False)
        header.set_title_widget(label("Monogram","heading"))
        menu=Gio.Menu()
        for title,action in [("New Conversation…","new-chat"),("Contacts","contacts"),("Saved Messages","saved"),("Wallet","wallet"),("Stories","stories"),("Spaces and Channels","spaces"),("Your Profile","profile")]:menu.append(title,"win."+action)
        section=Gio.Menu()
        for title,action in [("Preferences","preferences"),("Keyboard Shortcuts","shortcuts"),("Account Setup Preview","setup"),("Developer Tools","developer")]:section.append(title,"win."+action)
        menu.append_section(None,section)
        primary=Gtk.MenuButton(icon_name="open-menu-symbolic",menu_model=menu,tooltip_text="Main menu")
        header.pack_end(primary)
        header.pack_start(button("system-search-symbolic","Search conversations",lambda:self.search.grab_focus()))
        requests=Gtk.Button(tooltip_text="Requests and invitations · 2 pending")
        request_icon=Gtk.Overlay(child=Gtk.Image(icon_name="contact-new-symbolic"))
        badge=label(str(len(self.state.requests)),"request-badge",halign=Gtk.Align.END,valign=Gtk.Align.START);request_icon.add_overlay(badge)
        self.request_badge=badge
        requests.set_child(request_icon);requests.add_css_class("flat");requests.connect("clicked",lambda _:self.panels.requests())

        header.pack_end(requests);view.add_top_bar(header)
        content=box(True,6)
        self.search=Gtk.SearchEntry(placeholder_text="Search chats and messages",margin_start=12,margin_end=12,margin_top=6,margin_bottom=6)
        self.search.connect("search-changed",lambda _:self.refresh_chats());content.append(self.search)
        folders=box(False,4);folders.add_css_class("folder-bar");self.folder_bar=folders
        first=None
        for name in ["All","Unread","Groups"]:
            b=Gtk.ToggleButton(label=name);b.add_css_class("flat")
            if first:b.set_group(first)
            else:first=b;b.set_active(True)
            b.connect("toggled",lambda widget,n=name:self.set_folder(n) if widget.get_active() else None)
            self.context_actions(b,[("Edit folder…",lambda n=name:self.edit_folder(n)),("New folder…",lambda:self.edit_folder(""))])
            folders.append(b)
        content.append(folders)
        monero=Gtk.Button(label="Monero",icon_name="wallet-symbolic",halign=Gtk.Align.FILL,margin_start=12,margin_end=12)
        monero.set_child(label("◈   Monero",xalign=0,margin_start=8));monero.add_css_class("flat")
        monero.connect("clicked",lambda _:self.panels.wallet());content.append(monero)
        self.list=Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.list.add_css_class("chat-list");self.list.connect("row-activated",lambda _,row:self.select_chat(row.chat))
        content.append(scrolled(self.list,vexpand=True))
        footer=box(False,10);footer.add_css_class("sidebar-footer")
        who=type("Person",(),{"id":"","name":"Alex Morgan","color":"purple"})
        footer.append(avatar(who,32))
        words=box(True,2,hexpand=True);words.append(label("Alex Morgan","heading",xalign=0));words.append(label("Offline prototype","caption dim-label",xalign=0));footer.append(words)

        content.append(footer);view.set_content(content);self.split.set_sidebar(view)

    def set_folder(self,name):self.folder=name;self.refresh_chats()

    def refresh_chats(self):
        while self.list.get_first_child():self.list.remove(self.list.get_first_child())
        for chat in self.state.filtered(self.search.get_text(),self.folder):
            row=Gtk.ListBoxRow();row.chat=chat
            line=box(False,11);line.append(avatar(chat,44))
            words=box(True,5,hexpand=True)
            top=box(False,8);top.append(ellipsis(chat.name,css="chat-name",hexpand=True))
            top.append(label(chat.messages[-1].time if chat.messages else "Now","chat-time"));words.append(top)
            bottom=box(False,6)
            last=chat.messages[-1] if chat.messages else None
            preview=("You: " if last and last.own else "")+(last.text.replace("\n"," ") if last else "No messages yet")
            if last and last.kind=="photo":preview="Photo · "+preview
            bottom.append(ellipsis(preview,css="chat-preview",hexpand=True))
            if chat.unread:bottom.append(label(str(chat.unread),"unread-badge"))
            elif chat.id=="saved":bottom.append(Gtk.Image(icon_name="view-pin-symbolic",pixel_size=12))
            words.append(bottom);line.append(words);row.set_child(line);self.list.append(row)
            self.context_actions(row,[("Open",lambda c=chat:self.select_chat(c)),("Mark unread",lambda c=chat:(setattr(c,"unread",1),self.refresh_chats())),("Mute",lambda:self.notice("Muted locally")),("Archive",lambda c=chat:(self.state.chats.remove(c),self.refresh_chats()))])
            if chat is self.state.active:self.list.select_row(row)

    def make_content(self):
        view=Adw.ToolbarView()
        header=Adw.HeaderBar(show_start_title_buttons=False)
        sidebar_button=button("sidebar-show-symbolic","Show conversations",lambda:self.split.set_show_sidebar(not self.split.get_show_sidebar()))
        sidebar_button.set_visible(False)
        self.split.connect("notify::collapsed",lambda *_:sidebar_button.set_visible(self.split.get_collapsed()))
        header.pack_start(sidebar_button)
        self.title_button=Gtk.Button(valign=Gtk.Align.CENTER);self.title_button.add_css_class("flat")
        self.title_button.connect("clicked",lambda _:self.show_info());header.pack_start(self.title_button);header.set_title_widget(Gtk.Box())
        self.call_button=button("call-start-symbolic","Call",self.panels.call);header.pack_end(self.call_button)
        header.pack_end(button("face-smile-symbolic","Emoji, stickers and GIFs · F9",self.toggle_media))
        header.pack_end(button("system-search-symbolic","Search this conversation",lambda:self.search.grab_focus()))
        menu=Gio.Menu()
        for title,action in [("Conversation Security","security"),("Conversation Info","info"),("Mute","mute"),("Shared Media","info"),("Archive Conversation","archive")]:menu.append(title,"win."+action)
        header.pack_end(Gtk.MenuButton(icon_name="view-more-symbolic",menu_model=menu,tooltip_text="Conversation menu"))
        view.add_top_bar(header)
        self.pages=Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=160)
        self.utility=Adw.OverlaySplitView(sidebar_position=Gtk.PackType.END,min_sidebar_width=280,max_sidebar_width=320,sidebar_width_fraction=.3,show_sidebar=False)
        self.utility.set_sidebar_width_unit(Adw.LengthUnit.PX)
        self.chat=Conversation(self);self.chat.root.add_css_class("conversation");self.utility.set_content(self.chat.root)
        self.make_media_pane();self.pages.add_named(self.utility,"chat")
        view.set_content(self.pages);self.split.set_content(view)
        self.content_view=view;self.content_header=header

    def make_media_pane(self):
        pane=box(True,8);pane.add_css_class("utility-pane")
        toolbar=Adw.ToolbarView()
        header=Adw.HeaderBar(show_start_title_buttons=False)
        header.set_title_widget(label("Media","heading"))
        header.pack_end(button("window-close-symbolic","Close media pane",lambda:self.utility.set_show_sidebar(False)))
        toolbar.add_top_bar(header)
        self.media_stack=Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=150)
        switcher=Adw.ToggleGroup(homogeneous=True,can_shrink=True,margin_start=12,margin_end=12)
        for name,title in [("emoji","Emoji"),("stickers","Stickers"),("gifs","GIFs"),("monero","Monero")]:switcher.add(Adw.Toggle(name=name,label=title))
        switcher.connect("notify::active-name",lambda group,*_:self.media_stack.set_visible_child_name(group.get_active_name()))
        pane.append(switcher)
        search=Gtk.SearchEntry(placeholder_text="Search",margin_start=12,margin_end=12);pane.append(search)
        self.media_widgets=[]
        for name,title,items in [
            ("emoji","Emoji",["😀","😄","🥹","😊","😍","🥰","😎","🤔","🫡","🙃","😭","😂","❤️","💜","✨","🎉","👍","👋","☕","🍀","🌿","🌸","🌻","🌈","🌙","🔥","🦊","🐈","🐻","🐸","🍋","🍊","🍓","🍕","🎨","📷","💻","🚀","🎧","🏕️","🌊","🏔️","💫","🙌","🤝","💬","🫶","😴"]),
            ("stickers","Stickers",["🦊","🐻","🐸","🐈","☕","🎉","🌸","🚀"]),
            ("gifs","GIFs",["Happy dance","Good morning","High five","You got this","Weekend mood","Well done"])
        ]:
            area=box(True,10);margins(area,12)
            area.append(label("Recently used" if name=="emoji" else "Favorites","caption dim-label",xalign=0))
            flow=Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE,column_spacing=4,row_spacing=4,max_children_per_line=5 if name=="emoji" else 2,min_children_per_line=2)
            for index,item in enumerate(items):
                b=Gtk.Button(tooltip_text=item)
                if name=="gifs":
                    from .gif_preview import GifPreview
                    b.set_child(GifPreview(index));b.add_css_class("flat");b.set_size_request(110,100)
                else:b.set_label(item);b.add_css_class("emoji-item" if name=="emoji" else "sticker-item");b.add_css_class("flat")
                def choose(value=item,kind=name,asset=index):
                    if kind=="emoji":self.chat.insert(value)
                    elif kind=="gifs":
                        message=self.state.send(value,"gif");message.gif_index=asset;self.chat.render();self.refresh_chats()
                    else:self.state.send(value,"sticker");self.chat.render();self.refresh_chats()
                b.connect("clicked",lambda _,cb=choose:cb());flow.insert(b,-1);self.media_widgets.append((b,item,name))
            area.append(flow);self.media_stack.add_titled(scrolled(area,vexpand=True),name,title)
        from .wallet_card import WalletCard
        money=box(True,12);margins(money,12)
        card=WalletCard("Everyday","1.2048 XMR");card.set_content_width(220);card.set_content_height(170);money.append(card)
        mode=Adw.ToggleGroup(homogeneous=True);mode.add(Adw.Toggle(name="send",label="Send"));mode.add(Adw.Toggle(name="request",label="Request"));money.append(mode)
        amount=Gtk.Entry(placeholder_text="Amount",input_purpose=Gtk.InputPurpose.NUMBER);money.append(amount)
        currency=Gtk.DropDown.new_from_strings(["XMR","USD","EUR","PLN"]);money.append(currency)
        note=Gtk.Entry(placeholder_text="Add a note");money.append(note)
        review=Gtk.Button(label="Review");review.add_css_class("suggested-action")
        def payment_preview():
            value=amount.get_text() or "0"
            code=currency.get_selected_item().get_string()
            if mode.get_active_name()=="request":
                self.state.send(value+" "+code+(" · "+note.get_text() if note.get_text() else ""),"payment");self.chat.render();self.show_chat()
            else:
                d=Adw.AlertDialog(heading="Payment Review",body=value+" "+code+"\n"+note.get_text()+"\n\nOffline preview. No funds will be sent.");d.add_response("close","Close");d.present(self)
        review.connect("clicked",lambda _:payment_preview());money.append(review)
        self.media_stack.add_titled(scrolled(money,vexpand=True),"monero","Monero")
        def changed(stack,*_):
            name=stack.get_visible_child_name()
            if name:switcher.set_active_name(name)
            search.set_visible(name!="monero")
        self.media_stack.connect("notify::visible-child-name",changed)
        search.connect("search-changed",lambda entry:self.filter_media(entry.get_text()))
        pane.append(self.media_stack);self.media_stack.set_vexpand(True)
        pane.append(label("Local preview collection","caption dim-label",margin_bottom=12))
        toolbar.set_content(pane)
        self.media_pane=toolbar;self.utility.set_sidebar(toolbar)

    def filter_media(self,query):
        for widget,text,kind in self.media_widgets:widget.get_parent().set_visible(not query or query.casefold() in text.casefold())

    def toggle_media(self):
        if self.utility_mode!="media":
            self.utility.set_sidebar(None);self.utility.set_sidebar(self.media_pane);self.utility_mode="media";self.utility.set_show_sidebar(True)
        else:self.utility.set_show_sidebar(not self.utility.get_show_sidebar())

    def show_info(self):
        pane=box(True,16);pane.add_css_class("utility-pane")
        header=box(False,6);margins(header,12);header.append(label("Conversation Info","heading",hexpand=True,xalign=0));header.append(button("window-close-symbolic","Close info",lambda:self.utility.set_show_sidebar(False)));pane.append(header)
        pane.remove(header)
        toolbar=Adw.ToolbarView()
        native_header=Adw.HeaderBar(show_start_title_buttons=False,title_widget=label("Conversation Info","heading"))
        native_header.pack_start(button("window-close-symbolic","Close info",lambda:self.utility.set_show_sidebar(False)));toolbar.add_top_bar(native_header)
        pane.append(avatar(self.state.active,76));pane.append(label(self.state.active.name,"title-2"));pane.append(label(self.state.active.subtitle,"dim-label"))
        choices=Gtk.ListBox(selection_mode=Gtk.SelectionMode.NONE);choices.add_css_class("boxed-list");margins(choices,12)
        from .widgets import action_row
        for text,icon,cb in [("Shared Media","image-x-generic-symbolic",self.panels.media_viewer),("Conversation Security","channel-secure-symbolic",self.panels.security),("Control Access","changes-prevent-symbolic",self.panels.contact_access)]:choices.append(action_row(text,icon=icon,callback=cb))
        pane.append(choices)
        if self.state.active.group:
            pane.append(label("Members","heading",xalign=0,margin_start=16))
            for name in ["Alex · Owner","Sam","Mira","Robin"]:pane.append(label(name,xalign=0,margin_start=20))
        toolbar.set_content(scrolled(pane));self.utility.set_sidebar(None);self.utility.set_sidebar(toolbar);self.utility_mode="info";self.utility.set_show_sidebar(True)

    def select_chat(self,chat):
        self.state.notes[self.state.active.id]=self.chat.text()
        if chat is not self.state.active:
            self.chat.attachments.clear()
            while self.chat.chips.get_first_child():self.chat.chips.remove(self.chat.chips.get_first_child())
        self.state.active=chat;chat.unread=0
        title=box(False,10);title.append(avatar(chat,32));words=box(True,1)
        words.append(label(chat.name,"heading",xalign=0));words.append(label(chat.subtitle,"caption dim-label",xalign=0));title.append(words);self.title_button.set_child(title)
        self.chat.cancel_context();self.chat.entry.get_buffer().set_text(self.state.notes.get(chat.id,""))
        self.chat.render();self.refresh_chats();self.show_chat()
        if self.split.get_collapsed():self.split.set_show_sidebar(False)

    def show_chat(self):
        self.pages.set_visible_child_name("chat")
        chat=self.state.active
        title=box(False,10);title.append(avatar(chat,32));words=box(True,1)
        words.append(label(chat.name,"heading",xalign=0));words.append(label(chat.subtitle,"caption dim-label",xalign=0));title.append(words)
        self.title_button.set_child(title);self.title_button.set_visible(True);self.content_header.set_title_widget(Gtk.Box())

    def show_page(self,title,widget):
        old=self.pages.get_child_by_name("section")
        if old:self.pages.remove(old)
        self.pages.add_named(widget,"section");self.pages.set_visible_child(widget)
        self.title_button.set_visible(False);self.content_header.set_title_widget(label(title,"heading"))

    def set_big_emoji(self,value):self.big_emoji=value;self.chat.render()

    def choose_files(self,media=False):
        dialog=Gtk.FileDialog(title="Attach Photos or Videos" if media else "Attach Files",modal=True)
        if media:
            filt=Gtk.FileFilter(name="Photos and videos");filt.add_mime_type("image/*");filt.add_mime_type("video/*")
            filters=Gio.ListStore.new(Gtk.FileFilter);filters.append(filt);dialog.set_filters(filters)
        def finished(d,result):
            try:
                files=d.open_multiple_finish(result)
                self.chat.add_files([files.get_item(i) for i in range(files.get_n_items())]);self.show_chat();self.chat.entry.grab_focus()
            except GLib.Error as error:
                if not error.matches(Gtk.DialogError.quark(),Gtk.DialogError.DISMISSED):self.notice("File selection cancelled or unavailable")
        dialog.open_multiple(self,None,finished)

    def shortcuts(self):
        dialog=Adw.ShortcutsDialog()
        for title,items in [("Conversations",[("New conversation","<Control>n"),("Search","<Control>f"),("Attach files","<Control>o")]),("View",[("Media sidebar","F9"),("Preferences","<Control>comma")]),("Messages",[("Send","Return"),("New line","<Shift>Return")])]:
            section=Adw.ShortcutsSection(title=title)
            for name,key in items:section.add(Adw.ShortcutsItem(title=name,accelerator=key))
            dialog.add(section)
        self.panels.last_dialog=dialog;dialog.present(self)

    def context_actions(self,parent,items):
        def show(gesture,n,x,y):
            pop=Gtk.Popover(has_arrow=False);pop.set_parent(parent)
            rect=Gdk.Rectangle();rect.x=int(x);rect.y=int(y);rect.width=1;rect.height=1;pop.set_pointing_to(rect)
            content=box(True,2)
            for title,callback in items:
                b=Gtk.Button(label=title);b.add_css_class("flat")
                b.connect("clicked",lambda _,cb=callback:(pop.popdown(),cb()));content.append(b)
            pop.set_child(content);pop.connect("closed",lambda _:pop.unparent());pop.popup()
        click=Gtk.GestureClick(button=3);click.connect("pressed",show);parent.add_controller(click)

    def edit_folder(self,name):
        content=self.panels.body();entry=Adw.EntryRow(title="Folder name");entry.set_text(name);content.append(entry)
        selected={c.id for c in self.state.filtered(folder=name)} if name else set()
        choices=[]
        for chat in self.state.chats:
            check=Gtk.CheckButton(label=chat.name,active=chat.id in selected);choices.append((chat.id,check));content.append(check)
        save=Gtk.Button(label="Save",halign=Gtk.Align.END);save.add_css_class("suggested-action")
        def commit():
            new=entry.get_text().strip()
            if not new:return
            self.state.custom_folders[new]={key for key,check in choices if check.get_active()}
            if name and name!=new:
                child=self.folder_bar.get_first_child()
                while child:
                    if child.get_label()==name:child.set_label(new);break
                    child=child.get_next_sibling()
            elif not name:
                b=Gtk.ToggleButton(label=new);b.add_css_class("flat");b.set_group(self.folder_bar.get_first_child());self.folder_bar.append(b)
                self.context_actions(b,[("Edit folder…",lambda:self.edit_folder(new))])
            child=self.folder_bar.get_first_child()
            while child:
                if child.get_label()==new:
                    child.connect("toggled",lambda w:self.set_folder(new) if w.get_active() else None);child.set_active(True);break
                child=child.get_next_sibling()
            self.folder=new;self.refresh_chats();dialog.close()
        save.connect("clicked",lambda _:commit());content.append(save)
        dialog=self.panels.dialog("Edit Folder" if name else "New Folder",scrolled(content))
