"""Exercise the actual native window against the disposable fixture server."""
import os
from pathlib import Path
import ssl
import sys
from uuid import uuid4

from gi.repository import Gio, GLib
from monogram_desktop.app import MonogramApplication
from monogram_desktop.live_state import LiveState
from monogram_desktop.xmpp import XmppSession


def main():
    context = ssl.create_default_context(cafile=os.environ["MONOGRAM_IT_CERT"])
    alice = {"jid": "alice@localhost", "host": "127.0.0.1", "port": int(os.environ["MONOGRAM_IT_PORT"]), "direct_tls": False}
    bob = {**alice, "jid": "bob@localhost"}
    app = MonogramApplication()
    app.set_flags(Gio.ApplicationFlags.NON_UNIQUE)
    errors = []
    completed = []
    peer_events = []
    peer = None
    phase = 0
    text = "native composer " + uuid4().hex
    reply = "native peer " + uuid4().hex

    def exception(kind, value, traceback):
        import traceback as tb
        tb.print_exception(kind, value, traceback)
        errors.append(value)
        if peer:
            peer.close()
        app.quit()
    sys.excepthook = exception

    def activate(_):
        nonlocal peer
        def receive(kind, data):
            peer_events.append((kind, data))
            if kind == "message" and data["text"] == text:
                peer.call("send_text", "alice@localhost", reply, uuid4().hex, data["wire_id"], "alice@localhost")
        peer = XmppSession(bob, "fixture-password", receive, ssl_context=context)
        app.window.accounts.connect(alice, "fixture-password", ssl_context=context)
        GLib.timeout_add(50, poll)

    def poll():
        nonlocal phase
        w = app.window
        a = w.accounts
        if phase == 0 and a.status == "online" and any(k == "status" and d["state"] == "online" for k, d in peer_events):
            w.select_chat(w.state.chat_for("bob@localhost"))
            w.chat.entry.get_buffer().set_text(text)
            assert w.chat.send_button.get_sensitive()
            w.chat.send_button.emit("clicked")
            assert not w.chat.text()
            phase = 1
        elif phase == 1:
            chat = w.state.chat_for("bob@localhost")
            received = next((m for m in chat.messages if m.text == reply), None)
            if received is None:
                return True
            assert not received.encrypted and received.reply_id
            outgoing = next(m for m in chat.messages if m.text == text)
            assert outgoing.own and outgoing.delivery == "sent"
            w.chat.entry.get_buffer().set_text("restore this draft 🌿")
            w.chat.reply(received)
            w.save_draft()
            a.activate(a.preview)
            assert not w.state.account
            a.activate(LiveState(alice["jid"], a.store))
            w.select_chat(w.state.chat_for("bob@localhost"))
            assert w.chat.text() == "restore this draft 🌿"
            assert w.chat.replying.text == reply
            a.connect(alice, "fixture-password", ssl_context=context)
            phase = 2
        elif phase == 2 and a.status == "online" and "bob@localhost" in a.history_pages:
            chat = w.state.chat_for("bob@localhost")
            assert sum(m.text == text for m in chat.messages) == 1
            assert sum(m.text == reply for m in chat.messages) == 1
            assert w.chat.text() == "restore this draft 🌿"
            generation = a.generation
            a.store.save_account(bob)
            a.activate(LiveState(bob["jid"], a.store))
            assert all(not c.messages for c in w.state.chats)
            a.event(generation, alice["jid"], "message", {"text": "must be rejected before parsing"})
            assert all(not c.messages for c in w.state.chats)
            a.activate(LiveState(alice["jid"], a.store))
            w.select_chat(w.state.chat_for("bob@localhost"))
            assert w.chat.text() == "restore this draft 🌿"
            peer.close()
            w.close()
            completed.append(True)
            app.quit()
            print("Native live GTK: composer exchange, reply rendering, reconnect/MAM dedup, draft+reply restoration, account isolation and stale-callback rejection passed", flush=True)
            return False
        return True

    def timeout():
        errors.append(RuntimeError(f"Native live fixture timed out in phase {phase}"))
        if peer:
            peer.close()
        app.window.accounts.disconnect()
        app.quit()
        return False

    app.connect_after("activate", activate)
    GLib.timeout_add_seconds(45, timeout)
    app.run([sys.argv[0]])
    if peer:
        peer.close()
        peer.thread.join(3)
    if errors:
        raise errors[0]
    if not completed:
        raise RuntimeError("Native live fixture did not finish")


if __name__ == "__main__":
    main()
