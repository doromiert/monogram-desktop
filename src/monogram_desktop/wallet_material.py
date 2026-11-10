"""Monogram Android's wallet geometry, camera and seeded surface artwork.

Ported from WalletCardGLRenderer, ObjLoader and WalletCardFlecks; see assets/NOTICE.md.
"""
import math
import struct
from pathlib import Path
import cairo
import numpy as np

ASSETS = Path(__file__).with_name("assets") / "wallet"


def mesh(name):
    data = (ASSETS / name).read_bytes()
    offset = 0

    def values(fmt, count):
        nonlocal offset
        result = struct.unpack_from(">" + str(count) + fmt, data, offset)
        offset += struct.calcsize(fmt) * count
        return result

    positions, uvs, normals = [values("f", values("i", 1)[0]) for _ in range(3)]
    count = values("i", 1)[0]
    result = []
    for _ in range(count):
        p, _, n = values("i", 3)
        result.append((*positions[p*3:p*3+3], *normals[n*3:n*3+3]))
    assert offset == len(data) and count % 3 == 0
    return np.array(result, dtype=np.float32)


def rotation(x, y):
    cx, sx, cy, sy = math.cos(x), math.sin(x), math.cos(y), math.sin(y)
    return np.array([[1, 0, 0, 0], [0, cx, -sx, 0], [0, sx, cx, 0], [0, 0, 0, 1]], dtype=np.float32) @ np.array(
        [[cy, 0, sy, 0], [0, 1, 0, 0], [-sy, 0, cy, 0], [0, 0, 0, 1]], dtype=np.float32)


def perspective(fov, aspect, near=.1, far=20):
    f = 1 / math.tan(math.radians(fov) / 2)
    return np.array([[f/aspect, 0, 0, 0], [0, f, 0, 0],
                     [0, 0, (far+near)/(near-far), 2*far*near/(near-far)], [0, 0, -1, 0]], dtype=np.float32)


def matrices(width, height, x, y, padding=24):
    # Original model faces along X; its visible front has a -X normal.
    model = rotation(-x, -y) @ rotation(0, math.pi/2)
    translate = np.eye(4, dtype=np.float32); translate[0, 3] = -.02
    model = model @ translate @ np.diag([.965, .965, .965, 1])
    view = np.eye(4, dtype=np.float32); view[2, 3] = -6.7
    # The GTK sidebar can be wider than the card; always fit both dimensions.
    visible_width = min(max(1,width-padding*2),max(1,height-padding*2)*1.64)
    visible_height = visible_width/1.64
    fov = math.degrees(2*math.atan(math.tan(math.radians(10.4/2))*height/max(1, visible_height)))
    mv = view @ model
    return mv, perspective(fov, width/max(1, height)) @ mv


def rounded(c, x, y, width, height, radius):
    c.new_sub_path()
    for px, py, start in [(x+width-radius, y+radius, -90), (x+width-radius, y+height-radius, 0),
                           (x+radius, y+height-radius, 90), (x+radius, y+radius, 180)]:
        c.arc(px, py, radius, math.radians(start), math.radians(start+90))
    c.close_path()


class JavaRandom:
    """java.util.Random sequence, preserving the source card's star positions."""
    def __init__(self, seed): self.seed = (seed ^ 0x5DEECE66D) & ((1 << 48)-1)
    def random(self):
        self.seed = (self.seed*0x5DEECE66D+11) & ((1 << 48)-1)
        return (self.seed >> 24) / (1 << 24)


def stars():
    rng = JavaRandom(0x57414C4C45544C)
    centers, output, regions = [], [], []
    for point in range(16):
        distance = -1
        for _ in range(1 if point == 0 else 32):
            x, y = rng.random()*512, rng.random()*312.5
            nearest = min((min(abs(x-a), 512-abs(x-a))**2 + min(abs(y-b), 312.5-abs(y-b))**2
                           for a, b in centers), default=float("inf"))
            if nearest > distance: center = x, y; distance = nearest
        centers.append(center); x, y = center
        radius, angle, tilt = 1+rng.random()*6, rng.random()*math.tau, .05+rng.random()*.45
        dx, dy = math.cos(angle), math.sin(angle)
        gx, gy = -dy, dx
        colors = []
        for sign in (1, -1):
            nx, ny = dx*tilt+sign*gx*.6, dy*tilt+sign*gy*.6
            nz = math.sqrt(max(0, 1-nx*nx-ny*ny))
            colors.append(tuple(math.floor((v*.5+.5)*255+.5)/255 for v in (nx, ny, nz)))
        for iy in (-1, 0, 1):
            for ix in (-1, 0, 1):
                wx, wy = x+ix*512, y+iy*312.5
                output.append((wx, wy, radius, gx, gy, colors))
                if wx+radius > 0 and wx-radius < 512 and wy+radius > 0 and wy-radius < 512:
                    region = tuple(max(0, min(464, math.floor((v-24)/8+.5)*8)) for v in (wx, wy))
                    if region not in regions: regions.append(region)
    return output, regions


def flecks_artwork():
    points, regions = stars()
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, 512, 512); c = cairo.Context(surface)
    for x, y, radius, gx, gy, colors in points:
        inner = radius*.15
        c.move_to(x-radius, y)
        for px, py in [(-inner,-inner), (0,-radius), (inner,-inner), (radius,0), (inner,inner), (0,radius), (-inner,inner)]: c.line_to(x+px, y+py)
        c.close_path()
        gradient = cairo.LinearGradient(x-gx*radius, y-gy*radius, x+gx*radius, y+gy*radius)
        for index, color in enumerate(colors): gradient.add_color_stop_rgb(index, *color)
        c.set_source(gradient); c.fill()
    surface.flush()
    data = np.frombuffer(surface.get_data(), dtype=np.uint8).reshape(512,512,4)
    tiles = np.array([data[y:y+48,x:x+48] for x,y in regions])
    vertices = []
    for layer, (x,y) in enumerate(regions):
        for corner in (0,1,2,2,1,3):
            cx, cy = (-1 if corner & 1 == 0 else 1), (-1 if corner & 2 == 0 else 1)
            px, py = x+(48 if cx>0 else 0), y+(48 if cy>0 else 0)
            vertices.append((-.0125,(.5-py/312.5)*1.219512,px/512*2-1,x,y,layer,cx,cy))
    return tiles, data.copy(), np.array(vertices,dtype=np.float32)


def content(name, balance, precise,compact=False,width=1024):
    height=round(width*625/1024)
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32,width,height); c = cairo.Context(surface)
    c.scale(width/336,height/205)
    def text(x,y,value,size,alpha=1,bold=False):
        c.select_font_face("Sans",cairo.FONT_SLANT_NORMAL,cairo.FONT_WEIGHT_BOLD if bold else cairo.FONT_WEIGHT_NORMAL)
        c.set_font_size(size);c.set_source_rgba(1,1,1,alpha);c.move_to(x,y);c.show_text(value)
    if compact:
        text(24,45,name,14,bold=True);text(24,98,balance+" XMR",26,bold=True);text(24,125,"≈ €248.60",16,.85)
        return surface
    text(24,32,name,12,bold=True)
    text(24 if precise else 53,94,balance+" XMR",16 if precise else 23,bold=True)
    text(24,115,"≈ €248.60",14,.85)
    rounded(c,32,135,272,30,15);c.set_source_rgba(1,1,1,.10);c.fill()
    title="Show rounded amount" if precise else "Show precise amount"
    c.set_font_size(12);width=c.text_extents(title).width;text((336-width)/2,154,title,12)
    text(24,187,"ALEX MORGAN",12,.95)
    return surface


def engraving():
    surface=cairo.ImageSurface(cairo.FORMAT_ARGB32,2048,1024);c=cairo.Context(surface)
    c.scale(2048/336,1024/205);c.select_font_face("Monospace");c.set_font_size(9);c.set_source_rgb(1,1,1)
    for y,text in [(44,"84gJ … wB7"),(57,"PREVIEW ADDRESS")]:c.move_to(24,y);c.show_text(text)
    return surface


def details():
    import gi
    gi.require_version("Rsvg","2.0")
    from gi.repository import Rsvg
    surface=cairo.ImageSurface(cairo.FORMAT_ARGB32,1024,625);c=cairo.Context(surface);c.scale(1024/336,625/205)
    left=336-24-50
    rounded(c,left,73,50,38,9);c.set_source_rgb(1,0,0);c.fill()
    bounds=Rsvg.Rectangle();bounds.x=left+13;bounds.y=80;bounds.width=bounds.height=24
    Rsvg.Handle.new_from_file(str(ASSETS/"qr.svg")).render_document(c,bounds)
    surface.flush();data=np.frombuffer(surface.get_data(),dtype=np.uint8).reshape(625,1024,4).copy()
    yy,xx=np.indices(data.shape[:2]);gx,gy=45.4-5.2,34-3.4
    position=np.clip(((xx/(1024/336)-left-5.2)*gx+(yy/(625/205)-73-3.4)*gy)/(gx*gx+gy*gy),0,1)
    data[:,:,0]=np.where(data[:,:,3]>0,np.rint((.995+(.740-.995)*position)*(1-.5*data[:,:,1]/255)*255),0).astype(np.uint8)
    return data
