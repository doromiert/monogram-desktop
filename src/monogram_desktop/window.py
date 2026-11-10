from gi.repository import Adw, Gtk, Gdk, Gio, GLib, Pango
from .widgets import box, button, label, margins, scrolled, avatar, ellipsis
from .fixtures import PreviewState
from .chat import Conversation
from .panels import Panels

class MonogramWindow(Adw.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app,title="Monogram",default_width=1380,default_height=900,width_request=430,height_request=480)
        self.state=PreviewState();self.big_emoji=True;self.folder="All";self.utility_mode="media"
        self.draft_timer=0;self.loading_draft=False
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
        self.navigation_bin=Adw.BreakpointBin(child=self.split,width_request=375,height_request=360)
        self.utility.set_content(self.navigation_bin)
        self.viewer_layer=Gtk.Overlay(child=self.utility)
        self.toast.set_child(self.viewer_layer)
        self.make_actions()
        breakpoint=Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 760sp"))
        breakpoint.add_setter(self.split,"collapsed",True)
        breakpoint.add_setter(self.split,"show-sidebar",False)
        self.navigation_bin.add_breakpoint(breakpoint)
        narrow=Adw.Breakpoint.new(Adw.BreakpointCondition.parse("max-width: 1240sp"))
        narrow.add_setter(self.utility,"collapsed",True)
        self.add_breakpoint(narrow)
        self.select_chat(self.state.active)
        from .accounts import Accounts
        self.accounts=Accounts(self)

    def notice(self,text): self.toast.add_toast(Adw.Toast.new(text))

    def make_actions(self):
        actions={
            "new-chat":self.panels.new_chat,"requests":self.panels.requests,"contacts":self.panels.contacts,
            "wallet":self.panels.wallet,"stories":self.panels.stories,"call":self.panels.call,
            "spaces":self.panels.spaces,"preferences":self.panels.preferences,"profile":self.panels.profile,
            "security":self.panels.security,"poll":self.panels.poll,"payment":lambda:self.panels.payment(True),
            "location":self.panels.location,"article":self.panels.article,"developer":self.panels.developer,
            "attach-file":lambda:self.choose_files(False),"attach-media":lambda:self.choose_files(True),
            "search":self.show_search,"media":self.toggle_media,"info":self.show_info,
            "saved":lambda:self.select_chat(next(c for c in self.state.chats if c.id=="saved")),
            "shortcuts":self.shortcuts,"setup":lambda:self.accounts.manager(),
            "mute":lambda:self.notice("Notifications muted for this preview conversation"),
            "archive":lambda:self.organize(self.state.active,"archived",not self.state.active.archived),
            "archives":self.archives,
        }
        for name,callback in actions.items():
            action=Gio.SimpleAction.new(name,None)
            action.connect("activate",lambda a,param,cb=callback:cb());self.add_action(action)
        notification=Gio.SimpleAction.new_stateful("notifications",GLib.VariantType.new("s"),GLib.Variant("s","all"))
        def notifications(action,value):
            action.set_state(value);self.state.active.notifications=value.get_string()
            if hasattr(self,"accounts"):self.accounts.persist()
            self.notice("Notifications: "+{"all":"All messages","mentions":"Mentions only","1h":"Muted for 1 hour","8h":"Muted for 8 hours","2d":"Muted for 2 days","forever":"Muted until turned on"}[value.get_string()])
        notification.connect("activate",notifications);self.add_action(notification)
        for action,keys in {"attach-file":["<Control>o"],"new-chat":["<Control>n"],"search":["<Control>f"],"media":["F9"],"preferences":["<Control>comma"],"shortcuts":["<Control>question"]}.items():self.get_application().set_accels_for_action("win."+action,keys)

    def make_sidebar(self):
        view=Adw.ToolbarView();view.add_css_class("chat-sidebar")
        header=Adw.HeaderBar(show_end_title_buttons=False)
        header.set_title_widget(label("Monogram","heading"))
        menu=Gio.Menu()
        for title,action in [("New Conversation…","new-chat"),("Contacts","contacts"),("Saved Messages","saved"),("Archived Chats","archives"),("Wallet","wallet"),("Stories","stories"),("Spaces and Channels","spaces"),("Your Profile","profile")]:menu.append(title,"win."+action)
        section=Gio.Menu()
        for title,action in [("Accounts","setup"),("Preferences","preferences"),("Keyboard Shortcuts","shortcuts"),("Developer Tools","developer")]:section.append(title,"win."+action)
        menu.append_section(None,section)
        primary=Gtk.MenuButton(icon_name="open-menu-symbolic",menu_model=menu,tooltip_text="Main menu")
        header.pack_end(primary)
        self.sidebar_header=header
        self.search_button=Gtk.ToggleButton(icon_name="system-search-symbolic",tooltip_text="Search chats and messages")
        self.search_button.connect("toggled",lambda b:self.set_search_mode(b.get_active()));header.pack_start(self.search_button)
        requests=Gtk.Button(tooltip_text="Requests and invitations · 2 pending")
        request_icon=Gtk.Overlay(child=Gtk.Image(icon_name="contact-new-symbolic"))
        badge=label(str(len(self.state.requests)),"request-badge",halign=Gtk.Align.END,valign=Gtk.Align.START);request_icon.add_overlay(badge)
        self.request_badge=badge
        requests.set_child(request_icon);requests.add_css_class("flat");requests.connect("clicked",lambda _:self.panels.requests())

        header.pack_end(requests);view.add_top_bar(header)
        content=box(True,6)
        self.search=Gtk.SearchEntry(placeholder_text="Search chats and messages",hexpand=True)
        self.search.connect("search-changed",lambda _:self.search_changed())
        self.search.connect("stop-search",lambda _:self.search_button.set_active(False))
        from .folders import FolderBar
        folders=FolderBar(self);folders.add_css_class("folder-bar");self.folder_bar=folders
        first=None
        for name in self.state.folder_order:
            b=Gtk.ToggleButton(label=name);b.add_css_class("flat")
            if first:b.set_group(first)
            else:first=b;b.set_active(True)
            self.connect_folder(b)
            folders.append(b)
        folder_scroll=Gtk.ScrolledWindow(child=folders,hscrollbar_policy=Gtk.PolicyType.EXTERNAL,vscrollbar_policy=Gtk.PolicyType.NEVER,propagate_natural_height=True)
        content.append(folder_scroll)
        from .stories import StoryStrip,person
        self.story_strip=StoryStrip(self)
        self.list=Gtk.ListBox(selection_mode=Gtk.SelectionMode.SINGLE)
        self.list.add_css_class("chat-list");self.list.connect("row-activated",lambda _,row:self.select_chat(row.chat))
        sidebar_body=box(True);sidebar_body.append(self.story_strip);sidebar_body.append(self.list)
        self.chat_scroll=scrolled(sidebar_body,vexpand=True);content.append(self.chat_scroll)
        self.sidebar_titles=Gtk.Stack(transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=150)
        self.sidebar_titles.add_named(label("Monogram","heading"),"title")
        mini=Gtk.Button(tooltip_text="Show stories");mini.add_css_class("flat");mini.connect("clicked",lambda _:self.reveal_stories())
        people=box(False,2)
        for name in ["Mira","Sam","Alex"]:people.append(avatar(person(name),22))
        mini.set_child(people);self.sidebar_titles.add_named(mini,"stories");header.set_title_widget(self.sidebar_titles)
        self.chat_scroll.get_vadjustment().connect("value-changed",lambda a:self.sidebar_titles.set_visible_child_name("stories" if self.story_strip.get_visible() and a.get_value()>=80 else "title"))
        view.set_content(content);self.split.set_sidebar(view)

    def reveal_stories(self):
        self.search_button.set_active(False);self.split.set_show_sidebar(True)
        adjustment=self.chat_scroll.get_vadjustment()
        self.story_reveal=Adw.TimedAnimation.new(self.chat_scroll,adjustment.get_value(),0,180,Adw.CallbackAnimationTarget.new(adjustment.set_value));self.story_reveal.play()

    def show_search(self):
        self.search_button.set_active(True);self.search.grab_focus()

    def set_search_mode(self,active):
        self.story_strip.set_visible(not active and not self.state.account)
        if active:
            self.page_before_search=self.pages.get_visible_child_name()
            self.title_button.set_visible(False);self.header_titles.set_visible_child_name("search");self.pages.set_visible_child_name("search");self.refresh_search();self.search.grab_focus()
        else:
            self.pages.set_visible_child_name(self.page_before_search)
            self.header_titles.set_visible_child_name("normal")
            self.title_button.set_visible(self.pages.get_visible_child_name()=="chat")
            self.search.set_text("")

    def search_changed(self):
        self.refresh_chats()
        if self.search_button.get_active():self.refresh_search()

    def refresh_search(self):
        query=self.search.get_text().strip().casefold()
        if not query:
            self.search_page.set_child(Adw.StatusPage(icon_name="system-search-symbolic",title="Search Chats and Messages"));return
        from .widgets import group,action_row
        content=margins(box(True,24),24);chats=group("Conversations");messages=group("Messages");count=0
        for chat in self.state.chats:
            if query in chat.name.casefold():chats.add(action_row(chat.name,chat.subtitle,callback=lambda c=chat:self.select_chat(c)));count+=1
            for message in chat.messages:
                if query not in message.text.casefold():continue
                def jump(c=chat,m=message):
                    self.select_chat(c);GLib.timeout_add(50,lambda:(self.chat.highlight(m),False)[1])
                row=action_row(message.text[:160],chat.name+" · "+message.author+" · "+message.time,callback=jump);row.set_title_lines(2);messages.add(row);count+=1
        if not count:self.search_page.set_child(Adw.StatusPage(icon_name="system-search-symbolic",title="No Results",description="Try another search."));return
        content.append(chats);content.append(messages);self.search_page.set_child(scrolled(Adw.Clamp(child=content,maximum_size=800)))

    def connect_folder(self,button):
        button.connect("toggled",lambda widget:self.set_folder(widget.get_label()) if widget.get_active() else None)
        self.context_actions(button,[("Edit folder…",lambda:self.edit_folder(button.get_label())),("Move Left",lambda:self.shift_folder(button.get_label(),-1)),("Move Right",lambda:self.shift_folder(button.get_label(),1)),("New folder…",lambda:self.edit_folder(""))])
        self.folder_bar.enable_drag(button)

    def shift_folder(self,name,direction):
        index=self.state.folder_order.index(name);target=index+direction
        if 0<=target<len(self.state.folder_order):self.state.reorder_folder(name,self.state.folder_order[target],direction>0);self.order_folder_buttons()

    def order_folder_buttons(self):
        buttons={};child=self.folder_bar.get_first_child()
        while child:buttons[child.get_label()]=child;child=child.get_next_sibling()
        previous=None
        for name in self.state.folder_order:
            self.folder_bar.reorder_child_after(buttons[name],previous);previous=buttons[name]
        if hasattr(self,"accounts"):self.accounts.persist()

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
            preview=("Draft: "+chat.draft.replace("\n"," ")) if chat.draft else ("You: " if last and last.own else "")+(last.text.replace("\n"," ") if last else "No messages yet")
            if last and last.kind=="photo":preview="Photo · "+preview
            bottom.append(ellipsis(preview,css="chat-preview",hexpand=True))
            if chat.unread or chat.unread_mark:bottom.append(label(str(chat.unread) if chat.unread else "•","unread-badge"))
            elif chat.pinned:bottom.append(Gtk.Image(icon_name="view-pin-symbolic",pixel_size=12))
            words.append(bottom);line.append(words);row.set_child(line);self.list.append(row)
            self.context_actions(row,[("Open",lambda c=chat:self.select_chat(c)),("Unpin" if chat.pinned else "Pin",lambda c=chat:self.organize(c,"pinned",not c.pinned)),("Mark read" if chat.unread or chat.unread_mark else "Mark unread",lambda c=chat:self.mark_read(c)),("Unmute" if chat.notifications!="all" else "Mute",lambda c=chat:self.organize(c,"notifications","all" if c.notifications!="all" else "forever")),("Unarchive" if chat.archived else "Archive",lambda c=chat:self.organize(c,"archived",not c.archived))])
            if chat is self.state.active:self.list.select_row(row)

    def make_content(self):
        view=Adw.ToolbarView();view.add_css_class("conversation-view")
        header=Adw.HeaderBar(show_start_title_buttons=False)
        sidebar_button=button("sidebar-show-symbolic","Show conversations",lambda:self.split.set_show_sidebar(not self.split.get_show_sidebar()))
        sidebar_button.set_visible(False)
        self.split.connect("notify::collapsed",lambda *_:sidebar_button.set_visible(self.split.get_collapsed()))
        header.pack_start(sidebar_button)
        self.title_button=Gtk.Button(valign=Gtk.Align.CENTER);self.title_button.add_css_class("flat");self.title_button.add_css_class("conversation-title")
        self.title_button.connect("clicked",lambda _:self.show_info());header.pack_start(self.title_button)
        self.header_titles=Gtk.Stack(hhomogeneous=False,transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=150,valign=Gtk.Align.CENTER,hexpand=True)
        self.section_title=label("","heading");self.header_titles.add_named(self.section_title,"normal");self.header_titles.add_named(self.search,"search");header.set_title_widget(self.header_titles)
        menu=Gio.Menu()
        for title,action in [("Conversation Security","security"),("Conversation Info","info")]:menu.append(title,"win."+action)
        notifications=Gio.Menu()
        for title,value in [("All Messages","all"),("Mentions Only","mentions")]:
            item=Gio.MenuItem.new(title,None);item.set_action_and_target_value("win.notifications",GLib.Variant("s",value));notifications.append_item(item)
        muted=Gio.Menu()
        for title,value in [("For 1 Hour","1h"),("For 8 Hours","8h"),("For 2 Days","2d"),("Until I Turn It On","forever")]:
            item=Gio.MenuItem.new(title,None);item.set_action_and_target_value("win.notifications",GLib.Variant("s",value));muted.append_item(item)
        notifications.append_section("Mute",muted);menu.append_submenu("Mute",notifications)
        for title,action in [("Shared Media","info"),("Call","call"),("Archive Conversation","archive")]:menu.append(title,"win."+action)
        header.pack_end(Gtk.MenuButton(icon_name="view-more-symbolic",menu_model=menu,tooltip_text="Conversation menu"))
        view.add_top_bar(header)
        self.pages=Gtk.Stack(hhomogeneous=False,vhomogeneous=False,transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=160)
        self.search_page=Adw.Bin();self.pages.add_named(self.search_page,"search")
        self.utility=Adw.OverlaySplitView(sidebar_position=Gtk.PackType.END,min_sidebar_width=360,max_sidebar_width=400,sidebar_width_fraction=.3,show_sidebar=False,pin_sidebar=True)
        self.utility.connect("notify::show-sidebar",self.sidebar_visibility)
        self.utility.set_sidebar_width_unit(Adw.LengthUnit.PX)
        self.chat=Conversation(self);self.chat.root.add_css_class("conversation");self.utility.set_content(self.chat.root)
        self.make_media_pane();self.pages.add_named(self.utility,"chat")
        view.set_content(self.pages);self.split.set_content(view)
        self.content_view=view;self.content_header=header

    def make_media_pane(self):
        pane=box(True,12);pane.add_css_class("utility-pane");pane.set_margin_top(12)
        toolbar=Adw.ToolbarView();toolbar.add_css_class("utility-sidebar")
        header=Adw.HeaderBar(show_start_title_buttons=False)
        header.pack_start(button("sidebar-show-symbolic","Close media pane",lambda:self.utility.set_show_sidebar(False)))
        toolbar.add_top_bar(header)
        self.media_stack=Gtk.Stack(hhomogeneous=False,vhomogeneous=False,transition_type=Gtk.StackTransitionType.CROSSFADE,transition_duration=150)
        switcher=Adw.ToggleGroup(homogeneous=False,can_shrink=False)
        self.media_toggles={}
        for name,title in [("emoji","Emoji"),("stickers","Stickers"),("gifs","GIFs"),("monero","Monero")]:
            toggle=Adw.Toggle(name=name,label=title);switcher.add(toggle);self.media_toggles[name]=toggle
        switcher.connect("notify::active-name",lambda group,*_:self.media_stack.set_visible_child_name(group.get_active_name()))
        header.set_title_widget(switcher)
        search=Gtk.SearchEntry(placeholder_text="Search",margin_start=12,margin_end=12);pane.append(search)
        self.media_widgets=[]
        for name,title,items in [
            ("emoji","Emoji",["😀","😄","🥹","😊","😍","🥰","😎","🤔","🫡","🙃","😭","😂","❤️","💜","✨","🎉","👍","👋","☕","🍀","🌿","🌸","🌻","🌈","🌙","🔥","🦊","🐈","🐻","🐸","🍋","🍊","🍓","🍕","🎨","📷","💻","🚀","🎧","🏕️","🌊","🏔️","💫","🙌","🤝","💬","🫶","😴"]),
            ("stickers","Stickers",["🦊","🐻","🐸","🐈","☕","🎉","🌸","🚀"]),
            ("gifs","GIFs",["Happy dance","Good morning","High five","You got this","Weekend mood","Well done"])
        ]:
            area=box(True,8);margins(area,12);area.set_margin_top(0)
            area.append(label("Recently used" if name=="emoji" else "Favorites","caption dim-label",xalign=0))
            columns=6 if name=="emoji" else 4 if name=="stickers" else 2
            flow=Gtk.Grid(column_homogeneous=True,column_spacing=4,row_spacing=4) if name=="gifs" else Gtk.FlowBox(selection_mode=Gtk.SelectionMode.NONE,column_spacing=4,row_spacing=4,max_children_per_line=columns,min_children_per_line=columns)
            if name=="gifs":self.gif_grid=flow
            flow.add_css_class("media-grid")
            for index,item in enumerate(items):
                b=Gtk.Button(tooltip_text=item)
                if name=="gifs":
                    from .gif_preview import GifPreview
                    b.set_child(GifPreview(index));b.add_css_class("gif-button")
                else:b.set_label(item);b.add_css_class("emoji-item" if name=="emoji" else "sticker-item");b.add_css_class("flat")
                def choose(value=item,kind=name,asset=index):
                    if kind=="emoji":self.chat.insert(value)
                    elif kind=="gifs":
                        message=self.send_message(value,"gif")
                        if message:message.gif_index=asset;self.chat.render();self.refresh_chats()
                    else:self.send_message(value,"sticker");self.chat.render();self.refresh_chats()
                b.connect("clicked",lambda _,cb=choose:cb())
                if name=="gifs":flow.attach(b,index%2,index//2,1,1);target=b
                else:flow.insert(b,-1);target=b.get_parent();target.set_focusable(False)
                self.media_widgets.append((target,item,name))
            area.append(flow);self.media_stack.add_titled(scrolled(area,vexpand=True),name,title)
        from .wallet_card import WalletCard
        money=box(True,12);margins(money,12)
        card=WalletCard("Everyday","1.2048 XMR",compact=True);card.set_content_width(320);card.set_content_height(224);card.set_halign(Gtk.Align.CENTER);card.set_hexpand(False);money.append(card)
        mode=Adw.ToggleGroup(homogeneous=True);mode.add(Adw.Toggle(name="send",label="Send"));mode.add(Adw.Toggle(name="request",label="Request"));money.append(mode)
        amount=Gtk.Entry(placeholder_text="Amount",input_purpose=Gtk.InputPurpose.NUMBER);money.append(amount)
        currency=Gtk.DropDown.new_from_strings(["XMR","USD","EUR","PLN"]);money.append(currency)
        note=Gtk.Entry(placeholder_text="Add a note");money.append(note)
        review=Gtk.Button(label="Review");review.add_css_class("suggested-action")
        from .amounts import bind
        bind(amount,review)
        def payment_preview():
            value=amount.get_text() or "0"
            code=currency.get_selected_item().get_string()
            if mode.get_active_name()=="request":
                self.send_message(value+" "+code+(" · "+note.get_text() if note.get_text() else ""),"payment");self.chat.render();self.show_chat()
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
        self.media_pane=toolbar;self.track_sidebar(toolbar);self.utility.set_sidebar(toolbar)

    def sidebar_visibility(self,*_):
        sidebar=self.utility.get_sidebar()
        if sidebar and (self.utility.get_show_sidebar() or not sidebar.get_mapped()):sidebar.set_visible(self.utility.get_show_sidebar())

    def track_sidebar(self,widget):
        # libadwaita 1.9 still allocates a hidden docked sidebar at narrow widths.
        # Hide its content after the native close animation finishes.
        widget.set_visible(self.utility.get_show_sidebar())
        widget.connect("unmap",lambda sidebar:sidebar.set_visible(False) if not self.utility.get_show_sidebar() else None)

    def filter_media(self,query):
        gif_index=0
        for widget,text,kind in self.media_widgets:
            visible=not query or query.casefold() in text.casefold();widget.set_visible(visible)
            if kind=="gifs" and visible:
                self.gif_grid.remove(widget);self.gif_grid.attach(widget,gif_index%2,gif_index//2,1,1);gif_index+=1

    def toggle_media(self):
        if self.utility_mode!="media":
            self.utility.set_sidebar(None);self.utility.set_sidebar(self.media_pane);self.media_pane.set_visible(True);self.utility_mode="media";self.utility.set_show_sidebar(True)
        else:self.utility.set_show_sidebar(not self.utility.get_show_sidebar())

    def show_info(self):
        pane=box(True,16);pane.add_css_class("utility-pane")
        header=box(False,6);margins(header,12);header.append(label("Conversation Info","heading",hexpand=True,xalign=0));header.append(button("window-close-symbolic","Close info",lambda:self.utility.set_show_sidebar(False)));pane.append(header)
        pane.remove(header)
        toolbar=Adw.ToolbarView();toolbar.add_css_class("utility-sidebar")
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
        toolbar.set_content(scrolled(pane));self.track_sidebar(toolbar);self.utility.set_sidebar(None);self.utility.set_sidebar(toolbar);toolbar.set_visible(True);self.utility_mode="info";self.utility.set_show_sidebar(True)

    def select_chat(self,chat,save_previous=True):
        self.chat.clear_selection()
        if save_previous:self.save_draft()
        self.chat.attachments.clear()
        while self.chat.chips.get_first_child():self.chat.chips.remove(self.chat.chips.get_first_child())
        self.chat.chips.set_visible(False)
        self.state.active=chat;chat.unread=0;chat.unread_mark=False
        self.loading_draft=True
        self.chat.cancel_context();self.chat.entry.get_buffer().set_text(chat.draft)
        target=next((m for m in chat.messages if m.id==chat.draft_reply),None)
        if target:self.chat.reply(target)
        self.loading_draft=False
        self.chat.render();self.refresh_chats();self.show_chat()
        if hasattr(self,"accounts"):
            self.accounts.persist()
            self.chat.history_button.set_sensitive(True);self.chat.history_button.set_label("Load Earlier Messages")
            if self.state.account and self.accounts.status=="online" and chat.jid not in self.accounts.history_pages:self.accounts.load_history(initial=True)
        if self.split.get_collapsed():self.split.set_show_sidebar(False)

    def show_chat(self):
        self.search_button.set_active(False)
        self.pages.set_visible_child_name("chat")
        chat=self.state.active
        self.lookup_action("notifications").set_state(GLib.Variant("s",getattr(chat,"notifications","all")))
        title=box(False,8);title.append(avatar(chat,28));words=box(True,0)
        subtitle=self.accounts.subtitle(chat) if self.state.account and hasattr(self,"accounts") else chat.subtitle
        words.append(label(chat.name,"chat-title",xalign=0));words.append(label(subtitle,"chat-subtitle dim-label",xalign=0));title.append(words)
        self.title_button.set_child(title);self.title_button.set_visible(True);self.section_title.set_text("")

    def show_page(self,title,widget):
        self.search_button.set_active(False)
        old=self.pages.get_child_by_name("section")
        if old:self.pages.remove(old)
        self.pages.add_named(widget,"section");self.pages.set_visible_child(widget)
        self.title_button.set_visible(False);self.section_title.set_text(title)

    def set_big_emoji(self,value):self.big_emoji=value;self.chat.render()

    def send_message(self,text,kind="text"):
        return self.accounts.send(text,kind) if self.state.account else self.state.send(text,kind)

    def draft_changed(self):
        if self.loading_draft:return
        if self.draft_timer:GLib.source_remove(self.draft_timer)
        self.draft_timer=GLib.timeout_add(750,self.save_draft)
        if self.state.account and hasattr(self,"accounts"):self.accounts.typing()

    def save_draft(self):
        if self.draft_timer:GLib.source_remove(self.draft_timer);self.draft_timer=0
        if self.loading_draft or not hasattr(self,"chat"):return False
        chat=self.state.active;chat.draft=self.chat.text()
        chat.draft_reply=self.chat.replying.id if self.chat.replying else ""
        self.state.notes[chat.id]=chat.draft
        if hasattr(self,"accounts"):self.accounts.persist()
        return False

    def organize(self,chat,key,value):
        setattr(chat,key,value)
        if hasattr(self,"accounts"):self.accounts.persist()
        self.refresh_chats()
        if key=="archived":self.notice("Conversation archived" if value else "Conversation unarchived")

    def mark_read(self,chat):
        unread=not(chat.unread or chat.unread_mark);chat.unread=0;chat.unread_mark=unread
        if hasattr(self,"accounts"):self.accounts.persist()
        self.refresh_chats()

    def archives(self):
        from .widgets import action_row,group
        content=margins(box(True,18),24);g=group("Archived Chats");content.append(g)
        for chat in self.state.chats:
            if not chat.archived:continue
            row=action_row(chat.name,chat.jid or chat.subtitle,callback=lambda c=chat:self.select_chat(c));row.add_prefix(avatar(chat,36))
            row.add_suffix(button("folder-open-symbolic","Unarchive",lambda c=chat:(self.organize(c,"archived",False),self.archives())))
            g.add(row)
        if not any(c.archived for c in self.state.chats):content.append(Adw.StatusPage(icon_name="folder-symbolic",title="No Archived Chats"))
        self.show_page("Archived Chats",scrolled(Adw.Clamp(child=content,maximum_size=800)))

    def delete_messages(self,messages):
        chat=self.state.active
        if self.state.account:self.accounts.store.delete_messages(self.state.account,chat,messages)
        ids={m.id for m in messages};chat.messages[:]=[m for m in chat.messages if m.id not in ids]
        if hasattr(self,"accounts"):self.accounts.persist()
        self.chat.clear_selection();self.chat.render(False);self.refresh_chats()

    def choose_files(self,media=False):
        if self.state.account:self.notice("Live file transfers are not available yet.");return
        owner=self.state;chat_id=self.state.active.id
        dialog=Gtk.FileDialog(title="Attach Photos or Videos" if media else "Attach Files",modal=True)
        if media:
            filt=Gtk.FileFilter(name="Photos and videos");filt.add_mime_type("image/*");filt.add_mime_type("video/*")
            filters=Gio.ListStore.new(Gtk.FileFilter);filters.append(filt);dialog.set_filters(filters)
        def finished(d,result):
            try:
                files=d.open_multiple_finish(result)
                if self.state is not owner or self.state.active.id!=chat_id:self.notice("Conversation changed. Select the files again in their destination chat.");return
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
                def run(_,cb=callback):
                    pop.popdown()
                    if pop.get_parent():pop.unparent()
                    cb()
                b.connect("clicked",run);content.append(b)
            pop.set_child(content);pop.connect("closed",lambda _:pop.unparent() if pop.get_parent() else None);pop.popup()
        click=Gtk.GestureClick(button=3);click.connect("pressed",show);parent.add_controller(click)

    def edit_folder(self,name):
        from .fixtures import FolderRules
        rule=self.state.custom_folders.get(name,FolderRules())
        content=Adw.PreferencesPage()
        details=Adw.PreferencesGroup();entry=Adw.EntryRow(title="Folder name");entry.set_text(name);details.add(entry);content.add(details)
        included=Adw.PreferencesGroup(title="Included Chats",description="Matching chats are added automatically.");content.add(included)
        types={}
        for key,title in [("contacts","Contacts"),("non_contacts","Non-Contacts"),("groups","Groups"),("channels","Channels"),("bots","Bots")]:
            row=Adw.SwitchRow(title=title,active=key in rule.types);types[key]=row;included.add(row)
        excluded=Adw.PreferencesGroup(title="Excluded Chats");content.add(excluded)
        flags={}
        for key,title in [("exclude_muted","Muted Chats"),("exclude_read","Read Chats"),("exclude_archived","Archived Chats")]:
            row=Adw.SwitchRow(title=title,active=getattr(rule,key));flags[key]=row;excluded.add(row)
        exceptions=Adw.PreferencesGroup(title="Exceptions",description="Chosen chats override the rules above.");content.add(exceptions)
        choices={}
        for key,title,selected in [("include","Always Include",rule.include),("exclude","Never Include",rule.exclude)]:
            expander=Adw.ExpanderRow(title=title);exceptions.add(expander);choices[key]={}
            for chat in self.state.chats:
                check=Gtk.CheckButton(active=chat.id in selected,valign=Gtk.Align.CENTER);choices[key][chat.id]=check
                row=Adw.ActionRow(title=chat.name,activatable_widget=check);row.add_prefix(avatar(chat,32));row.add_suffix(check);expander.add_row(row)
        for key,other in [("include","exclude"),("exclude","include")]:
            for chat_id,check in choices[key].items():check.connect("toggled",lambda b,c=choices[other][chat_id]:c.set_active(False) if b.get_active() else None)
        save=Gtk.Button(label="Save",halign=Gtk.Align.END);save.add_css_class("suggested-action")
        def commit():
            new=entry.get_text().strip()
            if not new:return
            self.state.custom_folders[new]=FolderRules(types={key for key,row in types.items() if row.get_active()},**{key:row.get_active() for key,row in flags.items()},**{key:{chat_id for chat_id,check in checks.items() if check.get_active()} for key,checks in choices.items()})
            if name and name!=new:
                self.state.custom_folders.pop(name,None)
                self.state.folder_order[self.state.folder_order.index(name)]=new
                child=self.folder_bar.get_first_child()
                while child:
                    if child.get_label()==name:child.set_label(new);break
                    child=child.get_next_sibling()
            elif not name:
                self.state.folder_order.append(new)
                b=Gtk.ToggleButton(label=new);b.add_css_class("flat");b.set_group(self.folder_bar.get_first_child());self.folder_bar.append(b)
                self.connect_folder(b)
            child=self.folder_bar.get_first_child()
            while child:
                if child.get_label()==new:
                    child.set_active(True);break
                child=child.get_next_sibling()
            self.folder=new;self.refresh_chats()
            if hasattr(self,"accounts"):self.accounts.persist()
            dialog.close()
        save.connect("clicked",lambda _:commit())
        def validate(*_):save.set_sensitive(bool(entry.get_text().strip()) and (entry.get_text().strip()==name or entry.get_text().strip() not in self.state.custom_folders))
        entry.connect("changed",validate);validate()
        dialog=self.panels.dialog("Edit Folder" if name else "New Folder",content)
        dialog.header.pack_end(save)
