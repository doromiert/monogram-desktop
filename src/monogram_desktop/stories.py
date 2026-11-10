"""Mobile DialogStoriesCell/StoryViewer flow over fictional, in-memory stories."""
from pathlib import Path
from gi.repository import Gtk, Adw, Gdk, GLib
from .widgets import box,button,label,avatar


def person(name):
    return type("Person",(),{"id":name,"name":name,"color":{"Mira":"pink","Sam":"orange","Alex":"purple"}.get(name,"blue")})


class StoryStrip(Gtk.ScrolledWindow):
    def __init__(self,window):
        super().__init__(hscrollbar_policy=Gtk.PolicyType.NEVER,vscrollbar_policy=Gtk.PolicyType.NEVER)
        self.window=window;self.set_size_request(-1,88)
        self.items=box(False,8,margin_start=12,margin_end=12,margin_top=4,margin_bottom=6);self.set_child(self.items)
        self.refresh()

    def refresh(self):
        while self.items.get_first_child():self.items.remove(self.items.get_first_child())
        owners=["You"]+list(dict.fromkeys(s.owner for s in self.window.state.stories if not s.own))
        for name in owners:
            stories=[s for s in self.window.state.stories if s.owner==name]
            b=Gtk.Button(tooltip_text="Add Story" if name=="You" and not stories else name+"’s stories");b.add_css_class("story-button")
            content=box(True,4);icon=avatar(person(name),44)
            icon.add_css_class("story-unread" if any(not s.seen for s in stories) else "story-seen")
            visual=Gtk.Overlay(child=icon)
            if name=="You":
                add=Gtk.Image(icon_name="list-add-symbolic",halign=Gtk.Align.END,valign=Gtk.Align.END);add.add_css_class("story-add");visual.add_overlay(add)
            b.set_child(visual);content.append(b);content.append(label("My Story" if name=="You" and stories else name,"caption"))
            b.connect("clicked",lambda _,owner=name:self.open(owner));self.items.append(content)
            if name=="You":self.window.context_actions(b,[("Add Story…",self.window.panels.story_editor)])

    def open(self,owner):
        if owner=="You" and not any(s.own for s in self.window.state.stories):self.window.panels.story_editor();return
        stories=self.window.state.stories
        index=next(i for i,s in enumerate(stories) if s.owner==owner and not s.seen) if any(s.owner==owner and not s.seen for s in stories) else next(i for i,s in enumerate(stories) if s.owner==owner)
        viewer=StoryViewer(self.window,index);self.window.viewer_layer.add_overlay(viewer);viewer.open()


class StoryViewer(Gtk.Overlay):
    def __init__(self,window,index):
        super().__init__(hexpand=True,vexpand=True)
        self.window=window;self.index=index;self.elapsed=0.;self.last=0;self.holding=False;self.timer=0;self.closing=False
        self.add_css_class("story-viewer");self.set_focusable(True)
        self.stage=Gtk.Overlay();self.stage.add_css_class("story-stage")
        frame=Gtk.AspectFrame(ratio=9/16,obey_child=False,child=self.stage,hexpand=True,vexpand=True)
        self.set_child(frame)
        self.picture=Gtk.Picture(can_shrink=True,content_fit=Gtk.ContentFit.COVER,hexpand=True,vexpand=True);self.stage.set_child(self.picture)
        top=box(True,10,valign=Gtk.Align.START)
        top.add_css_class("story-top")
        self.segments=box(False,4,homogeneous=True);top.append(self.segments)
        controls=box(False,8)
        self.owner_icon=Adw.Avatar(size=32,show_initials=True);controls.append(self.owner_icon)
        names=box(True,1,hexpand=True);self.owner=label("","heading",xalign=0);names.append(self.owner);names.append(label("Today","caption",xalign=0));controls.append(names)
        self.pause=Gtk.ToggleButton(icon_name="media-playback-pause-symbolic",tooltip_text="Pause story");self.pause.add_css_class("flat");controls.append(self.pause)
        controls.append(button("window-close-symbolic","Close story",self.close));top.append(controls);self.stage.add_overlay(top)
        bottom=box(True,12,valign=Gtk.Align.END);bottom.add_css_class("story-bottom")
        self.caption=label("",wrap=True,xalign=0);bottom.append(self.caption)
        actions=box(False,8)
        self.reply=Gtk.Entry(placeholder_text="Reply privately…",hexpand=True);self.reply.connect("activate",lambda _:self.send_reply());actions.append(self.reply)
        self.like=Gtk.ToggleButton(label="♡",tooltip_text="Like story");self.like.add_css_class("flat")
        self.like.connect("toggled",lambda b:(b.set_label("♥" if b.get_active() else "♡"),setattr(self.current(),"liked",b.get_active())));actions.append(self.like)
        send=button("mail-send-symbolic","Send story reply",self.send_reply)
        plane=Gtk.Image.new_from_file(str(Path(__file__).with_name("paper-plane-symbolic.svg")));send.set_child(plane);actions.append(send)
        bottom.append(actions);self.stage.add_overlay(bottom)
        gesture=Gtk.GestureClick();gesture.connect("pressed",self.pressed);gesture.connect("released",self.released);self.picture.add_controller(gesture)
        keys=Gtk.EventControllerKey();keys.connect("key-pressed",self.key);self.add_controller(keys)
        self.previous=button("go-previous-symbolic","Previous story",lambda:self.move(-1),"circular");self.previous.set_halign(Gtk.Align.START);self.previous.set_margin_start(16);self.add_overlay(self.previous)
        following=button("go-next-symbolic","Next story",lambda:self.move(1),"circular");following.set_halign(Gtk.Align.END);following.set_margin_end(16);self.add_overlay(following)
        self.load()

    def current(self):return self.window.state.stories[self.index]

    def open(self):
        self.set_opacity(0);self.grab_focus()
        self.animation=Adw.TimedAnimation.new(self,0,1,200,Adw.CallbackAnimationTarget.new(self.set_opacity));self.animation.play()
        self.last=GLib.get_monotonic_time();self.timer=GLib.timeout_add(50,self.tick)

    def load(self):
        story=self.current();story.seen=True;self.elapsed=0.
        self.owner.set_text(story.owner);self.owner_icon.set_text(story.owner)
        self.picture.set_filename(str(Path(story.asset) if Path(story.asset).is_absolute() else Path(__file__).with_name(story.asset)))
        self.caption.set_text(story.caption);self.like.set_active(story.liked)
        self.reply.set_text("");self.reply.set_visible(not story.own)
        while self.segments.get_first_child():self.segments.remove(self.segments.get_first_child())
        self.bars=[]
        for item in self.window.state.stories:
            if item.owner==story.owner:
                bar=Gtk.ProgressBar(hexpand=True);self.bars.append((item,bar));self.segments.append(bar)
        self.previous.set_sensitive(self.index>0);self.progress();self.window.story_strip.refresh()

    def progress(self):
        found=False
        for item,bar in self.bars:
            if item is self.current():bar.set_fraction(min(1,self.elapsed/5));found=True
            else:bar.set_fraction(0 if found else 1)

    def tick(self):
        now=GLib.get_monotonic_time();delta=min(.1,(now-self.last)/1e6);self.last=now
        if not self.holding and not self.pause.get_active() and not self.reply.get_state_flags()&Gtk.StateFlags.FOCUS_WITHIN and self.window.is_active():
            self.elapsed+=delta;self.progress()
            if self.elapsed>=5:self.move(1)
        return not self.closing

    def move(self,direction):
        next=self.index+direction
        if next>=len(self.window.state.stories):self.close();return
        self.index=max(0,next);self.load()

    def pressed(self,gesture,count,x,y):self.holding=True;self.pressed_at=GLib.get_monotonic_time()
    def released(self,gesture,count,x,y):
        self.holding=False
        if GLib.get_monotonic_time()-self.pressed_at<250000:self.move(-1 if x<self.picture.get_width()/3 else 1)

    def key(self,controller,key,code,state):
        if key==Gdk.KEY_Escape:self.close();return True
        if self.reply.get_state_flags()&Gtk.StateFlags.FOCUS_WITHIN:return False
        if key in (Gdk.KEY_Left,Gdk.KEY_Right):self.move(-1 if key==Gdk.KEY_Left else 1);return True
        if key==Gdk.KEY_space:self.pause.set_active(not self.pause.get_active());return True
        return False

    def send_reply(self):
        if not self.reply.get_text().strip():return
        from .fixtures import Chat
        owner=self.current().owner
        chat=next((c for c in self.window.state.chats if c.name==owner),None)
        if chat is None:
            chat=Chat(owner.lower(),owner,"Last seen recently",owner[0],"blue");self.window.state.chats.append(chat)
        self.window.select_chat(chat);self.window.state.send(self.reply.get_text());self.window.chat.render();self.window.refresh_chats();self.close()

    def close(self):
        if self.closing:return
        self.closing=True
        if self.timer:GLib.source_remove(self.timer);self.timer=0
        self.animation=Adw.TimedAnimation.new(self,self.get_opacity(),0,180,Adw.CallbackAnimationTarget.new(self.set_opacity))
        self.animation.connect("done",lambda _:self.window.viewer_layer.remove_overlay(self));self.animation.play()
