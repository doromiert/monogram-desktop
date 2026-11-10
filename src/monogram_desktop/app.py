import os
import sys
from pathlib import Path
import gi
gi.require_version("Gtk","4.0")
gi.require_version("Adw","1")
from gi.repository import Adw, Gtk, Gdk, Gio, GLib
from .window import MonogramWindow

class MonogramApplication(Adw.Application):
    def __init__(self,smoke=False):
        super().__init__(application_id="com.negzero.MonogramDesktop",flags=Gio.ApplicationFlags.NON_UNIQUE if smoke else Gio.ApplicationFlags.DEFAULT_FLAGS)
        self.smoke=smoke;self.failure=None;self.smoke_completed=False
    def do_activate(self):
        if self.get_active_window():self.get_active_window().present();return
        provider=Gtk.CssProvider();provider.load_from_path(str(Path(__file__).with_name("style.css")))
        Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(),provider,Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        if "--preview-light" in sys.argv:Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_LIGHT)
        if "--preview-dark" in sys.argv:Adw.StyleManager.get_default().set_color_scheme(Adw.ColorScheme.FORCE_DARK)
        self.window=MonogramWindow(self)
        if "--show-media" in sys.argv:self.window.toggle_media()
        self.window.present()
        if "--preview-wallet" in sys.argv:self.window.panels.wallet()
        if "--preview-gifs" in sys.argv:self.window.media_stack.set_visible_child_name("gifs")
        if "--preview-viewer" in sys.argv:self.window.panels.media_viewer()
        if self.smoke:GLib.timeout_add(500,self.smoke_test)
    def smoke_test(self):
        try:
            w=self.window
            assert w.content_header.get_height()<=54, w.content_header.get_height()
            assert w.get_width()>=1000
            assert not w.search_button.get_active()
            w.show_search();assert w.header_titles.get_visible_child_name()=="search";w.search_button.set_active(False)
            photo=w.chat.bubbles[id(w.state.active.messages[2])]
            assert 0<photo.get_height()<350,photo.get_height()
            assert photo.get_first_child().get_next_sibling().get_height()>100
            heading=photo.get_first_child();_,author=heading.get_first_child().compute_bounds(heading);_,meta=heading.get_last_child().compute_bounds(heading)
            assert abs(author.get_y()+author.get_height()/2-meta.get_y()-meta.get_height()/2)<=1
            from .amounts import bind
            amount=Gtk.Entry();review=Gtk.Button();bind(amount,review)
            amount.set_text("words");assert amount.get_text()=="" and not review.get_sensitive()
            amount.set_text("0.25");assert review.get_sensitive()
            amount.get_buffer().insert_text(4,"x",1);assert amount.get_text()=="0.25"
            w.chat.select_message(w.state.active.messages[0]);w.chat.select_message(w.state.active.messages[2],True)
            assert len(w.chat.selected_messages())==3 and w.chat.selection_bar.get_revealed();w.chat.clear_selection()
            w.toggle_media();assert w.utility.get_show_sidebar()
            w.chat.insert("Preview message");w.chat.send();assert w.state.active.messages[-1].text=="Preview message"
            w.panels.wallet();assert w.pages.get_visible_child_name()=="section"
            w.select_chat(w.state.chats[1]);assert w.pages.get_visible_child_name()=="chat"
            w.show_info();assert w.utility_mode=="info"
            w.toggle_media();assert w.utility_mode=="media"
            w.set_folder("Groups");assert all(c.group for c in w.state.filtered(folder="Groups"))
            for open_dialog in [w.panels.preferences,w.panels.new_chat,w.panels.requests,w.panels.profile,w.panels.security,w.panels.poll,w.panels.article,w.panels.payment,w.panels.manage_wallets,w.panels.add_wallet,w.panels.setup,w.panels.spaces,w.panels.call]:
                open_dialog();w.panels.last_dialog.close()
            w.shortcuts();assert isinstance(w.panels.last_dialog,Adw.ShortcutsDialog);w.panels.last_dialog.close()
            w.edit_folder("Unread");w.panels.last_dialog.close()
            w.shift_folder("Groups",-1);w.shift_folder("Groups",-1)
            assert w.state.folder_order[0]=="Groups"
            w.story_strip.open("Mira")
            story=w.viewer_layer.get_last_child();assert story.current().owner=="Mira" and story.current().seen
            story.pause.set_active(True);story.move(1);story.close()
            w.panels.wallet()
            GLib.timeout_add(120,self.smoke_viewer)
            return False
        except Exception as error:
            self.failure=error;print("SMOKE FAILED:",repr(error),file=sys.stderr)
        self.quit();return False

    def smoke_viewer(self):
        card=self.window.panels.wallet_stack.get_visible_child()
        assert card.area.ready and card.area.frames>0,card.area.get_error()
        card.qr_button.emit("clicked");assert self.window.panels.last_dialog.get_title()=="Receive Monero" and not card.get_active();self.window.panels.last_dialog.close()
        card.set_active(True);assert card.balance=="1.204812345678"
        self.window.panels.media_viewer()
        GLib.timeout_add(350,self.smoke_finish)
        return False
    def smoke_finish(self):
        viewer=self.window.viewer_layer.get_last_child()
        viewer.move(1);viewer.move(-1);viewer.close();self.closing_viewer=viewer
        GLib.timeout_add(300,self.smoke_closed)
        return False
    def smoke_closed(self):
        assert self.closing_viewer.get_parent() is None
        self.window.set_default_size(680,820);GLib.timeout_add(200,self.smoke_narrow)
        return False
    def smoke_narrow(self):
        assert self.window.get_width()<=760 and self.window.split.get_collapsed()
        self.window.set_default_size(1120,820);GLib.timeout_add(200,self.smoke_medium)
        return False
    def smoke_medium(self):
        assert not self.window.split.get_collapsed() and self.window.utility.get_collapsed()
        self.smoke_completed=True
        print("Native GTK smoke: headers, photo bounds, amount validation, folders/drag, story flow, QR/precision, OpenGL, viewer close and responsive layouts passed",flush=True)
        self.quit();return False

def main():
    smoke="--smoke-test" in sys.argv
    app=MonogramApplication(smoke)
    if smoke:
        def exception(kind,value,traceback):
            import traceback as tb
            tb.print_exception(kind,value,traceback);app.failure=value;app.quit()
        sys.excepthook=exception
    app.run([sys.argv[0]])
    if app.failure:raise app.failure
    if smoke and not app.smoke_completed:raise RuntimeError("Native smoke never completed")
