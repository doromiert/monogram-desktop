"""Projected 3D card with the original user-edited Monogram Monero mesh."""
import math,struct
from pathlib import Path
import cairo
from gi.repository import Gtk

class WalletCard(Gtk.DrawingArea):
    def __init__(self,name,balance):
        super().__init__(content_width=500,content_height=290,hexpand=True)
        self.name=name;self.balance=balance;self.rx=-.07;self.ry=.10
        raw=Path(__file__).with_name('assets').joinpath('monero.bin').read_bytes()
        magic,count,stride=struct.unpack('<4sII',raw[:12]);assert magic==b'XMR2' and stride==11
        data=struct.unpack('<'+str(count*stride)+'f',raw[12:]);self.vertices=[data[i:i+stride] for i in range(0,len(data),stride)]
        self.set_draw_func(self.draw)
        motion=Gtk.EventControllerMotion();motion.connect('motion',self.motion);motion.connect('leave',lambda *_:self.reset());self.add_controller(motion)
    def motion(self,event,x,y):
        self.ry=(x/max(1,self.get_width())-.5)*.35;self.rx=(y/max(1,self.get_height())-.5)*-.25;self.queue_draw()
    def reset(self):self.rx=-.07;self.ry=.10;self.queue_draw()
    def project(self,x,y,z,w,h):
        cy,sy=math.cos(self.ry),math.sin(self.ry);cx,sx=math.cos(self.rx),math.sin(self.rx)
        x,z=x*cy+z*sy,-x*sy+z*cy;y,z=y*cx-z*sx,y*sx+z*cx
        k=900/(900+z);return w/2+x*k,h/2+y*k,z
    def draw(self,area,c,w,h):
        scale=min((w-40)/500,(h-20)/290);c.translate(w/2,h/2);c.scale(scale,scale);c.translate(-250,-145);w,h=500,290
        # Card face and thickness are separately projected, so pointer tilt changes the silhouette.
        corners=[]
        for x,y,angle in [(218,-105,-90),(218,105,0),(-218,105,90),(-218,-105,180)]:
            for step in range(0,91,10):
                a=math.radians(angle+step);corners.append((x+20*math.cos(a),y+20*math.sin(a)))
        def polygon(z):
            pts=[self.project(x,y,z,w,h) for x,y in corners];c.move_to(*pts[0][:2])
            for p in pts[1:]:c.line_to(*p[:2])
            c.close_path()
        polygon(10);c.set_source_rgb(.52,.19,.02);c.fill()
        polygon(0);gradient=cairo.LinearGradient(0,0,500,290);gradient.add_color_stop_rgb(0,1,.57,.10);gradient.add_color_stop_rgb(1,.91,.26,.015);c.set_source(gradient);c.fill_preserve();c.save();c.clip()
        for r in range(20,720,10):
            c.set_source_rgba(1,.92,.65,.065);c.set_line_width(.7);c.arc(-100,380,r,0,math.tau);c.stroke()
        c.restore()
        def text(x,y,value,size,alpha=1):
            px,py,_=self.project(x,y,-1,w,h);c.set_source_rgba(1,1,1,alpha);c.select_font_face('Sans',cairo.FONT_SLANT_NORMAL,cairo.FONT_WEIGHT_BOLD);c.set_font_size(size);c.move_to(px,py);c.show_text(value)
        text(-204,-83,self.name,14);text(-204,-34,self.balance,34);text(-204,3,'≈ €248.60',14,.8)
        text(-204,84,'84g…wB7',12,.85);text(150,88,'XMR',13,.9)
        # Original mesh uses x/z for its front face; remap it into card coordinates.
        faces=[]
        for i in range(0,len(self.vertices),3):
            tri=self.vertices[i:i+3];points=[]
            for v in tri:points.append(self.project(163+v[0]*42,-44-v[1]*42,-8+v[2]*42,w,h))
            faces.append((sum(p[2] for p in points),points,tri[0]))
        for _,pts,v in sorted(faces,reverse=True):
            light=.60+.4*abs(v[5]);c.set_source_rgb(*(max(0,min(1,col*light)) for col in v[6:9]))
            c.move_to(*pts[0][:2]);c.line_to(*pts[1][:2]);c.line_to(*pts[2][:2]);c.close_path();c.fill()
