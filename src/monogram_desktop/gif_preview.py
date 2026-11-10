from pathlib import Path
import gi
gi.require_version('GdkPixbuf','2.0')
from gi.repository import Gtk,GdkPixbuf,Gdk,GLib
class GifPreview(Gtk.DrawingArea):
    def __init__(self,index):
        super().__init__(content_width=110,content_height=100,hexpand=True)
        self.animation=GdkPixbuf.PixbufAnimation.new_from_file(str(Path(__file__).with_name('assets')/f'preview-{index}.gif'))
        self.iterator=self.animation.get_iter(None);self.timer=0;self.set_draw_func(self.draw)
        self.connect('map',self.start);self.connect('unmap',self.stop)
    def start(self,*_):
        if not self.timer:self.timer=GLib.timeout_add(80,self.tick)
    def stop(self,*_):
        if self.timer:GLib.source_remove(self.timer);self.timer=0
    def tick(self):self.iterator.advance(None);self.queue_draw();return True
    def draw(self,widget,c,width,height):
        pixbuf=self.iterator.get_pixbuf();c.save();c.scale(width/pixbuf.get_width(),height/pixbuf.get_height());Gdk.cairo_set_source_pixbuf(c,pixbuf,0,0);c.paint();c.restore()
