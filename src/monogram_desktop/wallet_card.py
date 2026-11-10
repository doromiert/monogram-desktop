"""Source Monogram wallet meshes and GLSL materials hosted in Gtk.GLArea."""
import ctypes
import math
import os
import struct
from pathlib import Path
import numpy as np
from gi.repository import Gtk, Gdk, GLib

os.environ.setdefault("PYOPENGL_PLATFORM", "egl")
from OpenGL import GL
from . import wallet_material as art


def program(vertex, fragment, define=""):
    shaders=[]
    try:
        for kind,name in [(GL.GL_VERTEX_SHADER,vertex),(GL.GL_FRAGMENT_SHADER,fragment)]:
            source=(art.ASSETS/name).read_text().replace("#version 300 es", "#version 330 core\n"+define)
            source=source.replace("precision highp float;","")
            shader=GL.glCreateShader(kind);shaders.append(shader)
            GL.glShaderSource(shader,source);GL.glCompileShader(shader)
            if not GL.glGetShaderiv(shader,GL.GL_COMPILE_STATUS):raise RuntimeError(GL.glGetShaderInfoLog(shader).decode())
        result=GL.glCreateProgram()
        for shader in shaders:GL.glAttachShader(result,shader)
        GL.glLinkProgram(result)
        if not GL.glGetProgramiv(result,GL.GL_LINK_STATUS):
            error=GL.glGetProgramInfoLog(result).decode();GL.glDeleteProgram(result);raise RuntimeError(error)
        return result
    finally:
        for shader in shaders:GL.glDeleteShader(shader)


class CardArea(Gtk.GLArea):
    def __init__(self,card):
        super().__init__(hexpand=True)
        self.content_width=500;self.content_height=305
        self.card=card;self.rx=0.;self.ry=0.;self.ready=False;self.frames=0
        self.programs={};self.vaos=[];self.buffers=[];self.textures=[];self.framebuffers=[];self.renderbuffers=[]
        self.multisample=None;self.multisample_size=None
        self.set_allowed_apis(Gdk.GLAPI.GL);self.set_required_version(3,3)
        self.set_has_depth_buffer(True);self.set_auto_render(False)
        self.connect("realize",self.initialize);self.connect("unrealize",self.cleanup);self.connect("render",self.render)
        self.connect("resize",lambda *_:card.queue_controls())

    def do_get_request_mode(self):return Gtk.SizeRequestMode.HEIGHT_FOR_WIDTH

    def do_measure(self,orientation,for_size):
        natural=self.content_width if orientation==Gtk.Orientation.HORIZONTAL else round(max(0,for_size)*self.content_height/self.content_width) if for_size>=0 else self.content_height
        return 0,natural,-1,-1

    def motion(self,_,x,y):
        self.ry=(x/max(1,self.get_width())-.5)*math.radians(30)
        self.rx=(y/max(1,self.get_height())-.5)*math.radians(30);self.card.queue_controls();self.queue_render()

    def reset(self,*_):self.rx=self.ry=0.;self.card.queue_controls();self.queue_render()

    def texture(self,data,unit=0,mipmap=True):
        GL.glActiveTexture(GL.GL_TEXTURE0+unit)
        texture=GL.glGenTextures(1);self.textures.append(texture);GL.glBindTexture(GL.GL_TEXTURE_2D,texture)
        self.upload(data,mipmap)
        return texture

    def upload(self,data,mipmap=True):
        if hasattr(data,"get_data"):
            data.flush();width,height=data.get_width(),data.get_height();pixels=bytes(data.get_data())
        else:
            height,width=data.shape[:2];pixels=data.tobytes()
        GL.glTexImage2D(GL.GL_TEXTURE_2D,0,GL.GL_RGBA8,width,height,0,GL.GL_BGRA,GL.GL_UNSIGNED_BYTE,pixels)
        for option,value in [(GL.GL_TEXTURE_MIN_FILTER,GL.GL_LINEAR_MIPMAP_LINEAR if mipmap else GL.GL_LINEAR),(GL.GL_TEXTURE_MAG_FILTER,GL.GL_LINEAR),(GL.GL_TEXTURE_WRAP_S,GL.GL_CLAMP_TO_EDGE),(GL.GL_TEXTURE_WRAP_T,GL.GL_CLAMP_TO_EDGE)]:GL.glTexParameteri(GL.GL_TEXTURE_2D,option,value)
        if mipmap:GL.glGenerateMipmap(GL.GL_TEXTURE_2D)

    def geometry(self,data,attributes):
        vao=GL.glGenVertexArrays(1);vbo=GL.glGenBuffers(1);self.vaos.append(vao);self.buffers.append(vbo)
        GL.glBindVertexArray(vao);GL.glBindBuffer(GL.GL_ARRAY_BUFFER,vbo);GL.glBufferData(GL.GL_ARRAY_BUFFER,data.nbytes,data,GL.GL_STATIC_DRAW)
        for index,size,offset in attributes:
            if index>=0:
                GL.glEnableVertexAttribArray(index);GL.glVertexAttribPointer(index,size,GL.GL_FLOAT,False,data.shape[1]*4,ctypes.c_void_p(offset*4))
        return vao,len(data)

    def initialize(self,*_):
        self.make_current()
        if self.get_error():return
        try:
            for name,define in [("body","WALLET_BODY_PASS"),("engraving","WALLET_ENGRAVING_PASS"),("qr","WALLET_QR_PASS"),("edge","WALLET_EDGE_PASS"),("flecks","WALLET_FLECKS_PASS")]:
                self.programs[name]=program("wallet_card_vertex.glsl","wallet_card_fragment.glsl","#define "+define)
            for name,fragment in [("content","wallet_card_content_fragment.glsl"),("logo","wallet_card_diamond_fragment.glsl")]:self.programs[name]=program("wallet_card_vertex.glsl",fragment)
            self.programs["monero"]=program("modelVertex.glsl","modelFragment.glsl")
            self.meshes={}
            for name,file in [("face","card_back.binobj"),("edge","card_edge.binobj")]:
                data=art.mesh(file)
                for material in (["edge"] if name=="edge" else ["body","engraving","qr","content","logo"]):
                    p=self.programs[material]
                    self.meshes[material]=self.geometry(data,[(GL.glGetAttribLocation(p,"aPosition"),3,0),(GL.glGetAttribLocation(p,"aNormal"),3,3)])
            self.detail=self.texture(np.zeros((625,1024,4),dtype=np.uint8) if self.card.compact else art.details(),0,False)
            self.engraving=self.texture(art.engraving(),1)
            self.front=self.texture(art.content(self.card.name,self.card.balance,self.card.get_active(),self.card.compact),0,False)
            self.uploaded=None
            tiles,tail,vertices=art.flecks_artwork()
            GL.glActiveTexture(GL.GL_TEXTURE2);self.flecks=GL.glGenTextures(1);self.textures.append(self.flecks)
            GL.glBindTexture(GL.GL_TEXTURE_2D_ARRAY,self.flecks)
            GL.glTexImage3D(GL.GL_TEXTURE_2D_ARRAY,0,GL.GL_RGBA8,48,48,len(tiles),0,GL.GL_BGRA,GL.GL_UNSIGNED_BYTE,tiles.tobytes())
            for option,value in [(GL.GL_TEXTURE_MIN_FILTER,GL.GL_LINEAR_MIPMAP_LINEAR),(GL.GL_TEXTURE_MAG_FILTER,GL.GL_LINEAR),(GL.GL_TEXTURE_WRAP_S,GL.GL_CLAMP_TO_EDGE),(GL.GL_TEXTURE_WRAP_T,GL.GL_CLAMP_TO_EDGE),(GL.GL_TEXTURE_MAX_LEVEL,3)]:GL.glTexParameteri(GL.GL_TEXTURE_2D_ARRAY,option,value)
            GL.glGenerateMipmap(GL.GL_TEXTURE_2D_ARRAY)
            self.tail=self.texture(tail,3)
            p=self.programs["flecks"]
            self.meshes["flecks"]=self.geometry(vertices,[(GL.glGetAttribLocation(p,"aPosition"),3,0),(GL.glGetAttribLocation(p,"aFlecksRegion"),3,3),(GL.glGetAttribLocation(p,"aFlecksCorner"),2,6)])
            normal=GL.glGetAttribLocation(p,"aNormal")
            if normal>=0:GL.glDisableVertexAttribArray(normal);GL.glVertexAttrib3f(normal,-1,0,0)
            if not self.card.compact:self.bake_logo()
            GL.glBindVertexArray(0);GL.glBindBuffer(GL.GL_ARRAY_BUFFER,0)
            self.ready=True
        except Exception as error:
            self.cleanup();self.set_error(GLib.Error(str(error)))
            raise

    def uniform(self,p,name,value):
        location=GL.glGetUniformLocation(p,name)
        if location<0:return
        if isinstance(value,np.ndarray):GL.glUniformMatrix4fv(location,1,True,value.astype(np.float32))
        elif isinstance(value,int):GL.glUniform1i(location,value)
        elif isinstance(value,tuple):getattr(GL,"glUniform"+str(len(value))+"f")(location,*value)
        else:GL.glUniform1f(location,value)

    def bake_logo(self):
        previous=int(GL.glGetIntegerv(GL.GL_FRAMEBUFFER_BINDING));viewport=GL.glGetIntegerv(GL.GL_VIEWPORT)
        frame=GL.glGenFramebuffers(1);self.framebuffers.append(frame);GL.glBindFramebuffer(GL.GL_FRAMEBUFFER,frame)
        self.logo=self.texture(np.zeros((512,512,4),dtype=np.uint8),0,False)
        GL.glFramebufferTexture2D(GL.GL_FRAMEBUFFER,GL.GL_COLOR_ATTACHMENT0,GL.GL_TEXTURE_2D,self.logo,0)
        depth=GL.glGenRenderbuffers(1);self.renderbuffers.append(depth);GL.glBindRenderbuffer(GL.GL_RENDERBUFFER,depth);GL.glRenderbufferStorage(GL.GL_RENDERBUFFER,GL.GL_DEPTH_COMPONENT24,512,512);GL.glFramebufferRenderbuffer(GL.GL_FRAMEBUFFER,GL.GL_DEPTH_ATTACHMENT,GL.GL_RENDERBUFFER,depth)
        assert GL.glCheckFramebufferStatus(GL.GL_FRAMEBUFFER)==GL.GL_FRAMEBUFFER_COMPLETE
        raw=Path(__file__).with_name("assets").joinpath("monero.bin").read_bytes()
        magic,count,stride=struct.unpack("<4sII",raw[:12]);assert magic==b"XMR2" and stride==11 and len(raw)==12+count*44
        data=np.frombuffer(raw[12:],dtype="<f4").reshape(count,stride)
        vao,count=self.geometry(data,[(0,3,0),(1,3,3),(2,3,6),(3,2,9)])
        model=art.rotation(-.10,.20)@np.diag([.8,.8,.8,1])
        view=np.eye(4,dtype=np.float32);view[2,3]=-3.8
        p=self.programs["monero"];GL.glUseProgram(p)
        self.uniform(p,"model",model);self.uniform(p,"mvp",art.perspective(35,1,.1,10)@view@model);self.uniform(p,"time",0.)
        GL.glViewport(0,0,512,512);GL.glClearColor(0,0,0,0);GL.glDepthMask(True);GL.glClear(GL.GL_COLOR_BUFFER_BIT|GL.GL_DEPTH_BUFFER_BIT)
        GL.glEnable(GL.GL_DEPTH_TEST);GL.glDepthFunc(GL.GL_LEQUAL);GL.glEnable(GL.GL_CULL_FACE);GL.glDisable(GL.GL_BLEND)
        GL.glBindVertexArray(vao);GL.glDrawArrays(GL.GL_TRIANGLES,0,count)
        GL.glBindTexture(GL.GL_TEXTURE_2D,self.logo);GL.glGenerateMipmap(GL.GL_TEXTURE_2D);GL.glTexParameteri(GL.GL_TEXTURE_2D,GL.GL_TEXTURE_MIN_FILTER,GL.GL_LINEAR_MIPMAP_LINEAR)
        GL.glDisable(GL.GL_CULL_FACE);GL.glBindFramebuffer(GL.GL_FRAMEBUFFER,previous);GL.glViewport(*viewport)

    def render_target(self,w,h):
        target=int(GL.glGetIntegerv(GL.GL_FRAMEBUFFER_BINDING))
        if self.multisample is None:
            self.multisample=GL.glGenFramebuffers(1);self.framebuffers.append(self.multisample)
            self.color_buffer=GL.glGenRenderbuffers(1);self.depth_buffer=GL.glGenRenderbuffers(1)
            self.renderbuffers.extend([self.color_buffer,self.depth_buffer])
        GL.glBindFramebuffer(GL.GL_FRAMEBUFFER,self.multisample)
        if self.multisample_size!=(w,h):
            samples=min(4,int(GL.glGetIntegerv(GL.GL_MAX_SAMPLES)))
            for buffer,format,attachment in [(self.color_buffer,GL.GL_RGBA8,GL.GL_COLOR_ATTACHMENT0),(self.depth_buffer,GL.GL_DEPTH_COMPONENT24,GL.GL_DEPTH_ATTACHMENT)]:
                GL.glBindRenderbuffer(GL.GL_RENDERBUFFER,buffer);GL.glRenderbufferStorageMultisample(GL.GL_RENDERBUFFER,samples,format,w,h)
                GL.glFramebufferRenderbuffer(GL.GL_FRAMEBUFFER,attachment,GL.GL_RENDERBUFFER,buffer)
            assert GL.glCheckFramebufferStatus(GL.GL_FRAMEBUFFER)==GL.GL_FRAMEBUFFER_COMPLETE
            self.multisample_size=(w,h)
        return target

    def render(self,area,context):
        if not self.ready:return False
        viewport=GL.glGetIntegerv(GL.GL_VIEWPORT);w,h=int(viewport[2]),int(viewport[3])
        scale=w/max(1,self.get_width())
        mv,mvp=art.matrices(w,h,self.rx,self.ry,24*scale)
        target=self.render_target(w,h);GL.glEnable(GL.GL_MULTISAMPLE)
        GL.glViewport(0,0,w,h);GL.glClearColor(0,0,0,0);GL.glDepthMask(True);GL.glClear(GL.GL_COLOR_BUFFER_BIT|GL.GL_DEPTH_BUFFER_BIT)
        GL.glEnable(GL.GL_DEPTH_TEST);GL.glDisable(GL.GL_CULL_FACE);GL.glDisable(GL.GL_BLEND);GL.glDepthFunc(GL.GL_LEQUAL)
        # Android captures the front at the face's pixel size, not a fixed atlas size.
        _,rest=art.matrices(w,h,0,0,24*scale)
        left=rest@np.array([-.0125,0,-1,1.]);right=rest@np.array([-.0125,0,1,1.])
        text_width=max(1,round(abs(right[0]/right[3]-left[0]/left[3])*w/2))
        current=(self.card.name,self.card.balance,self.card.get_active(),text_width)
        if current!=self.uploaded:
            GL.glActiveTexture(GL.GL_TEXTURE0);GL.glBindTexture(GL.GL_TEXTURE_2D,self.front);self.upload(art.content(*current[:3],compact=self.card.compact,width=text_width),False);self.uploaded=current
        for unit,texture in [(0,self.detail),(1,self.engraving),(3,self.tail)]:
            GL.glActiveTexture(GL.GL_TEXTURE0+unit);GL.glBindTexture(GL.GL_TEXTURE_2D,texture)
        GL.glActiveTexture(GL.GL_TEXTURE2);GL.glBindTexture(GL.GL_TEXTURE_2D_ARRAY,self.flecks)
        for name in ("body","engraving","qr","flecks","logo","content","edge"):
            if self.card.compact and name in ("engraving","qr","logo"):continue
            if name=="logo" and self.card.get_active():continue
            p=self.programs[name];GL.glUseProgram(p)
            for uniform,value in [("uModelViewMatrix",mv),("uMVPMatrix",mvp),("uMainLightDirection",(.175,0.,.83)),("uCardGradientRotation",math.degrees(self.ry)*2-math.degrees(self.rx)*1.25),("uDetailsOffset",0.),("uCardDetailTexture",0),("uEngravingTexture",1),("uEngravingTexel",(1/2048,1/1024)),("uFlecksTiles",2),("uFlecksTail",3),("uQrCenter",((336-24-25)/336,92/205)),("uFlecksPadding",(0.,0.)),("uDiamondBounds",(24/336,75/205,24/336,24/205)),("uDiamondAlpha",1.)]:self.uniform(p,uniform,value)
            if name in ("logo","content"):
                GL.glEnable(GL.GL_BLEND);GL.glBlendFunc(GL.GL_ONE,GL.GL_ONE_MINUS_SRC_ALPHA)
            else:GL.glDisable(GL.GL_BLEND)
            GL.glDepthMask(name in ("body","edge"));GL.glDepthFunc(GL.GL_LEQUAL if name in ("body","edge") else GL.GL_EQUAL)
            if name=="flecks":
                GL.glEnable(GL.GL_POLYGON_OFFSET_FILL);GL.glPolygonOffset(1.,2.);GL.glDepthFunc(GL.GL_GREATER)
            GL.glActiveTexture(GL.GL_TEXTURE0);GL.glBindTexture(GL.GL_TEXTURE_2D,self.logo if name=="logo" else self.front if name=="content" else self.detail)
            vao,count=self.meshes[name];GL.glBindVertexArray(vao);GL.glDrawArrays(GL.GL_TRIANGLES,0,count)
            if name=="flecks":GL.glDisable(GL.GL_POLYGON_OFFSET_FILL)
        GL.glDepthMask(True);GL.glDisable(GL.GL_BLEND);GL.glBindVertexArray(0);GL.glBindBuffer(GL.GL_ARRAY_BUFFER,0);GL.glActiveTexture(GL.GL_TEXTURE0)
        GL.glBindFramebuffer(GL.GL_READ_FRAMEBUFFER,self.multisample);GL.glBindFramebuffer(GL.GL_DRAW_FRAMEBUFFER,target)
        GL.glBlitFramebuffer(0,0,w,h,0,0,w,h,GL.GL_COLOR_BUFFER_BIT,GL.GL_NEAREST);GL.glBindFramebuffer(GL.GL_FRAMEBUFFER,target)
        self.frames+=1
        return True

    def cleanup(self,*_):
        self.make_current()
        if not self.get_error():
            for p in self.programs.values():GL.glDeleteProgram(p)
            for items,delete in [(self.vaos,GL.glDeleteVertexArrays),(self.buffers,GL.glDeleteBuffers),(self.textures,GL.glDeleteTextures),(self.framebuffers,GL.glDeleteFramebuffers),(self.renderbuffers,GL.glDeleteRenderbuffers)]:
                if items:delete(len(items),items)
        self.programs.clear();self.vaos.clear();self.buffers.clear();self.textures.clear();self.framebuffers.clear();self.renderbuffers.clear();self.ready=False
        self.multisample=None;self.multisample_size=None


class WalletCard(Gtk.Overlay):
    def __init__(self,name,balance,on_qr=None,compact=False):
        super().__init__(hexpand=True)
        self.precise=False;self.compact=compact;self.controls_pending=0
        self.name=name;self.rounded_balance=balance.removesuffix(" XMR");self.precise_balance=self.rounded_balance+"12345678"
        self.balance=self.rounded_balance
        self.area=CardArea(self);self.set_child(self.area);self.add_css_class("wallet-model")
        self.area.update_property([Gtk.AccessibleProperty.LABEL],[name+" · "+balance+" · approximately 248.60 euros"])
        self.controls=Gtk.Fixed();self.add_overlay(self.controls)
        self.precision_button=Gtk.Button(tooltip_text="Show precise amount")
        self.precision_button.connect("clicked",lambda _:self.set_active(not self.precise))
        self.qr_button=Gtk.Button(tooltip_text="Show receiving QR code")
        self.qr_button.connect("clicked",lambda _:on_qr(self.name) if on_qr else None)
        for button,title in [(self.precision_button,"Show precise amount"),(self.qr_button,"Show receiving QR code")]:
            button.add_css_class("wallet-hit-target");button.update_property([Gtk.AccessibleProperty.LABEL],[title]);self.controls.put(button,0,0)
            button.set_visible(not compact)
        motion=Gtk.EventControllerMotion();motion.connect("motion",self.area.motion);motion.connect("leave",self.area.reset);self.add_controller(motion)

    def get_active(self):return self.precise

    def set_active(self,value):
        self.precise=value;self.precision()

    def precision(self,*_):
        self.balance=self.precise_balance if self.get_active() else self.rounded_balance
        title="Show rounded amount" if self.get_active() else "Show precise amount"
        self.precision_button.set_tooltip_text(title);self.precision_button.update_property([Gtk.AccessibleProperty.LABEL],[title]);self.area.queue_render()

    def position_controls(self):
        self.controls_pending=0
        if self.compact:return
        width,height=self.area.get_width(),self.area.get_height()
        if not width or not height:return
        _,mvp=art.matrices(width,height,self.area.rx,self.area.ry)
        for button,(x,y,w,h),material_uv in [(self.precision_button,(32,135,272,30),False),(self.qr_button,(262,73,50,38),True)]:
            points=[]
            for u,v in [(x,y),(x+w,y),(x+w,y+h),(x,y+h)]:
                model_y=(.5-v/205)*1.219512 if material_uv else .618561-v/205*1.212122
                point=mvp@np.array([-.0125,model_y,u/336*2-1,1.])
                points.append(((point[0]/point[3]*.5+.5)*width,(.5-point[1]/point[3]*.5)*height))
            xs,ys=zip(*points);self.controls.move(button,min(xs),min(ys));button.set_size_request(max(1,round(max(xs)-min(xs))),max(1,round(max(ys)-min(ys))))
        return False

    def queue_controls(self):
        if not self.compact and not self.controls_pending:self.controls_pending=GLib.idle_add(self.position_controls)

    def set_content_width(self,width):self.area.content_width=width;self.area.queue_resize()
    def set_content_height(self,height):self.area.content_height=height;self.area.queue_resize()
