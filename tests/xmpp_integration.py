"""Disposable loopback-only Prosody fixture. No real accounts, settings or credentials."""
import asyncio
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import time
from uuid import uuid4

from monogram_desktop.xmpp import XmppClient, XmppSession


async def wait_for(events, kind, predicate=lambda data: True, timeout=25, after=0):
    async def poll():
        while True:
            match = next((data for event, data in events[after:] if event == kind and predicate(data)), None)
            if match is not None:
                return match
            await asyncio.sleep(.03)
    return await asyncio.wait_for(poll(), timeout)


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


async def exercise(port, certificate):
    context = ssl.create_default_context(cafile=str(certificate))
    clients = []
    def make(user, password="fixture-password", trusted=True):
        config = {"jid": user + "@localhost", "host": "127.0.0.1", "port": port, "direct_tls": False}
        events = []
        client = XmppClient(config, password, lambda kind, data: events.append((kind, data)), ssl_context=context if trusted else None)
        clients.append(client)
        client.open_connection()
        return client, events
    try:
        alice, a = make("alice")
        bob, b = make("bob")
        other, c = make("alice")
        for events in (a, b, c):
            await wait_for(events, "status", lambda d: d["state"] == "online")
            await wait_for(events, "roster")
        await alice.update_roster("bob@localhost", name="Bobby")
        await wait_for(a, "roster", lambda d: any(contact["jid"] == "bob@localhost" and contact["name"] == "Bobby" for contact in d["contacts"]))
        await asyncio.sleep(.25)
        bob.plugin["xep_0184"].auto_ack = True
        outgoing = uuid4().hex
        alice.send_text("bob@localhost", "Hello 🌿 from desktop", outgoing)
        incoming = await wait_for(b, "message", lambda d: d["wire_id"] == outgoing)
        assert not incoming["own"] and incoming["text"] == "Hello 🌿 from desktop"
        assert "origin:" + outgoing in incoming["aliases"]
        await wait_for(a, "receipt", lambda d: d["id"] == outgoing and d["peer"] == "bob@localhost")
        carbon = await wait_for(c, "message", lambda d: d["wire_id"] == outgoing)
        assert carbon["own"] and carbon["peer"] == "bob@localhost"
        reply = uuid4().hex
        bob.send_text("alice@localhost", "Reply from peer", reply, outgoing, "alice@localhost",incoming["text"])
        response = await wait_for(a, "message", lambda d: d["wire_id"] == reply)
        assert response["reply_id"] == outgoing and not response["own"]
        bob.send_typing("alice@localhost", True)
        await wait_for(a, "typing", lambda d: d["composing"])
        await alice.history("bob@localhost")
        await wait_for(a, "history", lambda d: "error" not in d)
        archived = [d for k, d in a if k == "message" and d["historical"]]
        assert {outgoing, reply} <= {d["wire_id"] for d in archived}, archived
        assert all(any(alias.startswith("archive:") for alias in d["aliases"]) for d in archived)
        # A real socket loss must reconnect with the same account, without replaying sends.
        mark=len(a)
        alice.transport.abort()
        await wait_for(a,"status",lambda d:d["state"]=="online",after=mark)
        # A real thread session exercises cancellation/teardown, not only the client object.
        threaded_events = []
        threaded_config = {"jid": "alice@localhost", "host": "127.0.0.1", "port": port, "direct_tls": False}
        session = XmppSession(threaded_config, "fixture-password", lambda k, d: threaded_events.append((k, d)), ssl_context=context)
        await wait_for(threaded_events, "status", lambda d: d["state"] == "online")
        session.close()
        await asyncio.to_thread(session.thread.join, 3)
        assert not session.thread.is_alive(), "Cancelled session left its transport thread running"
        wrong, wrong_events = make("alice", "incorrect-password")
        await wait_for(wrong_events, "status", lambda d: d["state"] == "error")
        assert not wrong.online
        untrusted, untrusted_events = make("alice", trusted=False)
        await wait_for(untrusted_events, "status", lambda d: d["state"] == "error")
        assert not untrusted.online
        assert not any(k == "status" and d["state"] == "online" for k, d in untrusted_events)
        mismatch_events=[]
        mismatch=XmppClient({**threaded_config,"jid":"alice@wrong.localhost"},"fixture-password",lambda k,d:mismatch_events.append((k,d)),ssl_context=context)
        clients.append(mismatch);mismatch.open_connection()
        await wait_for(mismatch_events,"status",lambda d:d["state"]=="error")
        assert not mismatch.online
        # A server that omits STARTTLS may not receive any SASL authentication data.
        plaintext=[]
        async def no_tls(reader,writer):
            plaintext.append(await reader.read(4096))
            writer.write(b"<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='localhost' id='plain' version='1.0'><stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism><mechanism>SCRAM-SHA-256</mechanism></mechanisms></stream:features>")
            await writer.drain()
            plaintext.append(await reader.read(4096));writer.close();await writer.wait_closed()
        server=await asyncio.start_server(no_tls,"127.0.0.1",0)
        try:
            clear_events=[]
            clear=XmppClient({**threaded_config,"port":server.sockets[0].getsockname()[1]},"fixture-password",lambda k,d:clear_events.append((k,d)),ssl_context=context)
            clients.append(clear);clear.open_connection()
            await wait_for(clear_events,"status",lambda d:d["state"]=="error")
            assert not clear.online and b"<auth" not in b"".join(plaintext)
        finally:
            server.close();await server.wait_closed()
        print("Live XMPP: verified TLS login, roster, Unicode text, reply IDs, receipts, carbons, MAM, typing, socket reconnect, password/certificate/hostname rejection, no plaintext SASL and thread cancellation passed", flush=True)
    finally:
        for client in clients:
            client.stopping = True
            client.cancel_connection_attempt()
            if client.ready_timeout:
                client.ready_timeout.cancel()
            if client.retry_task:
                client.retry_task.cancel()
            await client.disconnect(wait=0, ignore_send_queue=True)
        await asyncio.sleep(.1)


def main():
    with tempfile.TemporaryDirectory(prefix="monogram-xmpp-") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        (root / "data").mkdir()
        cert, key = root / "localhost.crt", root / "localhost.key"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost", "-keyout", str(key), "-out", str(cert)], check=True, capture_output=True)
        key.chmod(0o600)
        port = free_port()
        config = root / "prosody.cfg.lua"
        config.write_text(f'''
daemonize = false
run_as_root = false
pidfile = "{root}/prosody.pid"
data_path = "{root}/data"
interfaces = {{ "127.0.0.1" }}
unbound = {{ resolvconf = false; hoststxt = false }}
c2s_ports = {{ {port} }}
s2s_ports = {{}}
http_ports = {{}}
https_ports = {{}}
allow_registration = false
c2s_require_encryption = true
ssl = {{ key = "{key}"; certificate = "{cert}" }}
modules_enabled = {{ "roster"; "saslauth"; "tls"; "disco"; "carbons"; "mam"; "ping" }}
log = {{ {{ levels = {{ min = "warn" }}; to = "console" }} }}
VirtualHost "localhost"
authentication = "internal_hashed"
default_archive_policy = true
archive_expires_after = "1d"
VirtualHost "wrong.localhost"
authentication = "internal_hashed"
''')
        for user in ("alice", "bob"):
            subprocess.run(["prosodyctl", "--config", str(config), "register", user, "localhost", "fixture-password"], check=True, capture_output=True)
        with (root / "server.log").open("w+") as log:
            server = subprocess.Popen(["prosody", "--config", str(config), "-F"], stdout=log, stderr=subprocess.STDOUT)
            try:
                for _ in range(100):
                    if server.poll() is not None:
                        log.seek(0)
                        raise RuntimeError("Prosody failed: " + log.read())
                    try:
                        with socket.create_connection(("127.0.0.1", port), timeout=.1):
                            break
                    except OSError:
                        time.sleep(.05)
                else:
                    raise RuntimeError("Prosody did not start")
                asyncio.run(exercise(port, cert))
                if os.environ.get("MONOGRAM_IT_GUI") == "1":
                    environment = os.environ.copy()
                    environment.update(MONOGRAM_IT_CERT=str(cert), MONOGRAM_IT_PORT=str(port), XDG_STATE_HOME=str(root / "ui-state"))
                    subprocess.run([os.sys.executable, str(Path(__file__).with_name("live_window.py"))], check=True, env=environment, timeout=60)
            finally:
                server.terminate()
                try:
                    server.wait(5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
