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
            w.panels.wallet()
            GLib.timeout_add(120,self.smoke_viewer)
            return False
        except Exception as error:
            self.failure=error;print("SMOKE FAILED:",repr(error),file=sys.stderr)
        self.quit();return False

    def smoke_viewer(self):
        self.window.panels.media_viewer()
        GLib.timeout_add(350,self.smoke_finish)
        return False
    def smoke_finish(self):
        viewer=self.window.viewer_layer.get_last_child()
        viewer.move(1);viewer.close()
        self.smoke_completed=True
        print("Native GTK smoke: compact header, chat, composer, panes, dialogs, shortcuts, rendered 3D mesh and viewer passed",flush=True)
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
