import gi
gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gtk, Pango

def label(text, css=None, **kw):
    result = Gtk.Label(label=text, **kw)
    if css:
        for name in css.split(): result.add_css_class(name)
    return result

def button(icon, tooltip, callback=None, css="flat"):
    result = Gtk.Button(icon_name=icon, tooltip_text=tooltip, valign=Gtk.Align.CENTER)
    for name in css.split(): result.add_css_class(name)
    if callback: result.connect("clicked", lambda _: callback())
    return result

def box(vertical=False, spacing=0, **kw):
    return Gtk.Box(orientation=Gtk.Orientation.VERTICAL if vertical else Gtk.Orientation.HORIZONTAL, spacing=spacing, **kw)

def margins(widget, value):
    for side in ("top", "bottom", "start", "end"): getattr(widget, "set_margin_" + side)(value)
    return widget

def clear(widget):
    while widget.get_first_child(): widget.remove(widget.get_first_child())

def avatar(chat, size=42):
    widget = Adw.Avatar(size=size, text=chat.name, show_initials=True)
    widget.add_css_class("avatar-" + chat.color)
    if chat.id == "saved":
        widget.set_show_initials(False)
        widget.set_icon_name("user-bookmarks-symbolic")
    return widget

def scrolled(child, **kw):
    return Gtk.ScrolledWindow(child=child, hscrollbar_policy=Gtk.PolicyType.NEVER, **kw)

def ellipsis(text, **kw):
    return label(text, ellipsize=Pango.EllipsizeMode.END, xalign=0, **kw)

class AspectPicture(Gtk.Picture):
    """A timeline photo may shrink horizontally, but keeps its measured aspect height."""
    def do_get_request_mode(self):return Gtk.SizeRequestMode.HEIGHT_FOR_WIDTH
    def do_measure(self,orientation,for_size):
        if orientation==Gtk.Orientation.HORIZONTAL:return 0,420,-1,-1
        paintable=self.get_paintable();ratio=paintable.get_intrinsic_aspect_ratio() if paintable else 2
        height=round((for_size if for_size>=0 else 420)/(ratio or 2))
        return (height if for_size>=0 else 0),height,-1,-1

def group(title, description=None):
    return Adw.PreferencesGroup(title=title, description=description)

def action_row(title, subtitle="", icon=None, callback=None):
    row = Adw.ActionRow(title=title, subtitle=subtitle,use_markup=False)
    if icon: row.add_prefix(Gtk.Image(icon_name=icon))
    if callback:
        row.set_activatable(True)
        row.add_suffix(Gtk.Image(icon_name="go-next-symbolic"))
        row.connect("activated", lambda _: callback())
    return row
