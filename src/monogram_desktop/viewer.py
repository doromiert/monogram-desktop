from pathlib import Path
from gi.repository import Gtk,Adw,Gdk,GLib,Graphene
from .widgets import box,button,label

class FlyPicture(Gtk.Box):
    def __init__(self,picture,viewer):
        super().__init__(hexpand=True,vexpand=True)
        self.picture=picture;self.viewer=viewer;self.append(picture)
    def do_snapshot(self,snapshot):
        progress=self.viewer.progress
        source=self.viewer.origin
        snapshot.save()
        if source is not None and progress<1:
            ok,target=self.compute_bounds(self.viewer)
            if ok:
                width=max(1,self.picture.get_width());height=max(1,self.picture.get_height())
                iw=min(width,height*2);ih=iw/2;ix=(width-iw)/2;iy=(height-ih)/2
                sx=source.get_width()/iw;sy=source.get_height()/ih
                snapshot.translate(Graphene.Point().init((source.get_x()-target.get_x()-ix*sx)*(1-progress),(source.get_y()-target.get_y()-iy*sy)*(1-progress)))
                snapshot.scale(progress+(1-progress)*sx,progress+(1-progress)*sy)
        self.snapshot_child(self.picture,snapshot);snapshot.restore()

class MediaViewer(Gtk.Overlay):
    """In-window viewer: Galeria's overlay, header, slide stack and edge controls."""
    def __init__(self,window,source=None):
        super().__init__(hexpand=True,vexpand=True)
        self.window=window;self.source=source;self.index=0;self.start=None;self.progress=0;self.origin=None
        if source is not None:
            ok,rect=source.compute_bounds(window.viewer_layer)
            if ok:self.origin=rect
        self.add_css_class('media-viewer')
        self.stack=Gtk.Stack(transition_duration=230,hexpand=True,vexpand=True)
        self.set_child(self.stack)
        for name in ['landscape.svg','sunset.svg','city.svg']:
            pic=Gtk.Picture.new_for_filename(str(Path(__file__).with_name(name)));pic.set_content_fit(Gtk.ContentFit.CONTAIN)
            pic.set_margin_top(70);pic.set_margin_bottom(60);pic.set_margin_start(70);pic.set_margin_end(70);self.stack.add_named(FlyPicture(pic,self),name)
        self.header=Adw.HeaderBar(show_start_title_buttons=False,show_end_title_buttons=False,valign=Gtk.Align.START)
        self.header.pack_start(button('go-previous-symbolic','Back',self.close))
        self.title=Adw.WindowTitle(title='Riverside trail',subtitle='1 of 3');self.header.set_title_widget(self.title)
        self.header.pack_end(button('view-fullscreen-symbolic','Fullscreen',lambda:window.unfullscreen() if window.is_fullscreen() else window.fullscreen()))
        self.header.pack_end(button('info-outline-symbolic','Details',lambda:window.notice('Original illustration · SVG · local preview')))
        self.add_overlay(self.header)
        self.previous=button('go-previous-symbolic','Previous media',lambda:self.move(-1),'circular')
        self.previous.set_halign(Gtk.Align.START);self.previous.set_margin_start(16);self.add_overlay(self.previous)
        self.following=button('go-next-symbolic','Next media',lambda:self.move(1),'circular')
        self.following.set_halign(Gtk.Align.END);self.following.set_margin_end(16);self.add_overlay(self.following)
        key=Gtk.EventControllerKey();key.connect('key-pressed',self.key);self.add_controller(key);self.set_focusable(True)
        swipe=Gtk.GestureSwipe();swipe.connect('swipe',lambda _,x,y:self.move(-1 if x>0 else 1) if abs(x)>abs(y) else None);self.add_controller(swipe)
        self.move(0)
    def open(self):
        self.set_opacity(0);self.grab_focus()
        target=self.stack.get_visible_child()
        def animate(value):
            self.progress=value;self.set_opacity(value);self.stack.get_visible_child().queue_draw()
        self.fly=Adw.TimedAnimation.new(self,0,1,280,Adw.CallbackAnimationTarget.new(animate))
        self.fly.play()
    def move(self,delta):
        self.index=max(0,min(2,self.index+delta));self.stack.set_transition_type(Gtk.StackTransitionType.SLIDE_LEFT if delta>0 else Gtk.StackTransitionType.SLIDE_RIGHT)
        self.stack.set_visible_child_name(['landscape.svg','sunset.svg','city.svg'][self.index]);self.title.set_title(['Riverside trail','Evening light','Around the neighborhood'][self.index]);self.title.set_subtitle(f'{self.index+1} of 3')
        self.previous.set_sensitive(self.index>0);self.following.set_sensitive(self.index<2)
    def key(self,controller,key,code,state):
        if key==Gdk.KEY_Escape:self.close();return True
        if key in [Gdk.KEY_Left,Gdk.KEY_Right]:self.move(-1 if key==Gdk.KEY_Left else 1);return True
        return False
    def close(self):self.window.viewer_layer.remove_overlay(self)
