"""Folders stay on their horizontal track while being reordered."""
from gi.repository import Gtk,Graphene

class FolderBar(Gtk.Box):
    def __init__(self,window):
        super().__init__(spacing=4)
        self.window=window;self.dragged=None;self.offset=0.;self.dragging=False

    def enable_drag(self,button):
        gesture=Gtk.GestureDrag(button=1,propagation_phase=Gtk.PropagationPhase.CAPTURE)
        def begin(g,x,y):
            self.dragged=button;self.grab_x=x;self.dragging=False;self.offset=0.;g.set_state(Gtk.EventSequenceState.CLAIMED)
        def update(g,dx,dy):
            if not self.dragging and abs(dx)<8:return
            self.dragging=True
            ok,rect=button.compute_bounds(self)
            if not ok:return
            pointer=rect.get_x()+self.grab_x+dx
            others=[];child=self.get_first_child()
            while child:
                if child is not button:
                    ok,bounds=child.compute_bounds(self)
                    if ok:others.append((child.get_label(),bounds.get_x()+bounds.get_width()/2))
                child=child.get_next_sibling()
            order=[name for name,mid in others]
            index=next((i for i,(_,mid) in enumerate(others) if pointer<mid),len(order))
            order.insert(index,button.get_label())
            self.window.state.folder_order=order;self.window.order_folder_buttons()
            self.offset=max(0,min(self.get_width()-rect.get_width(),pointer-self.grab_x));self.queue_draw()
        def end(g,dx,dy):
            if not self.dragging:button.set_active(True)
            self.dragged=None;self.offset=0.;self.queue_draw()
        def cancel(*_):self.dragged=None;self.offset=0.;self.queue_draw()
        gesture.connect("drag-begin",begin);gesture.connect("drag-update",update);gesture.connect("drag-end",end)
        gesture.connect("cancel",cancel);button.add_controller(gesture)

    def do_snapshot(self,snapshot):
        child=self.get_first_child()
        while child:
            if child is not self.dragged or not self.dragging:self.snapshot_child(child,snapshot)
            child=child.get_next_sibling()
        if self.dragged and self.dragging:
            ok,rect=self.dragged.compute_bounds(self)
            if ok:
                snapshot.save();snapshot.translate(Graphene.Point().init(self.offset-rect.get_x(),0));self.snapshot_child(self.dragged,snapshot);snapshot.restore()
