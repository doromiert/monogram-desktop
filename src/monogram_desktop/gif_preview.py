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
    def do_get_request_mode(self):return Gtk.SizeRequestMode.HEIGHT_FOR_WIDTH
    def do_measure(self,orientation,for_size):
        if orientation==Gtk.Orientation.HORIZONTAL:return 80,160,-1,-1
        pixbuf=self.iterator.get_pixbuf() if hasattr(self,'iterator') else None
        ratio=pixbuf.get_height()/pixbuf.get_width() if pixbuf else .78
        height=round((for_size if for_size>=0 else 160)*ratio)
        return (height if for_size>=0 else 0),height,-1,-1
    def start(self,*_):
        if not self.timer:self.timer=GLib.timeout_add(80,self.tick)
    def stop(self,*_):
        if self.timer:GLib.source_remove(self.timer);self.timer=0
    def tick(self):self.iterator.advance(None);self.queue_draw();return True
    def draw(self,widget,c,width,height):
        import math
        pixbuf=self.iterator.get_pixbuf();c.save()
        for x,y,start in [(width-10,10,-90),(width-10,height-10,0),(10,height-10,90),(10,10,180)]:c.arc(x,y,10,math.radians(start),math.radians(start+90))
        c.close_path();c.clip();scale=max(width/pixbuf.get_width(),height/pixbuf.get_height());c.translate((width-pixbuf.get_width()*scale)/2,(height-pixbuf.get_height()*scale)/2);c.scale(scale,scale);Gdk.cairo_set_source_pixbuf(c,pixbuf,0,0);c.paint();c.restore()
