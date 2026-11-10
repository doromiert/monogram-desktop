"""Isolated, fictional Prosody and TCP proxy fixture for the Rust transport."""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import threading
import select
import argparse


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def main():
    with tempfile.TemporaryDirectory(prefix="monogram-xmpp-") as temporary:
        root = Path(temporary)
        root.chmod(0o700)
        (root / "data").mkdir()
        cert, key = root / "localhost.crt", root / "localhost.key"
        ca, ca_key = root / "ca.crt", root / "ca.key"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-subj", "/CN=Monogram Fixture CA", "-addext", "basicConstraints=critical,CA:TRUE", "-keyout", str(ca_key), "-out", str(ca)], check=True, capture_output=True)
        csr = root / "localhost.csr"
        subprocess.run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(csr)], check=True, capture_output=True)
        extensions = root / "leaf.ext"
        extensions.write_text("basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost\n")
        subprocess.run(["openssl", "x509", "-req", "-in", str(csr), "-CA", str(ca), "-CAkey", str(ca_key), "-CAcreateserial", "-days", "2", "-extfile", str(extensions), "-out", str(cert)], check=True, capture_output=True)
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
                run_checks(root, port, ca)
            finally:
                server.terminate()
                try:
                    server.wait(5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


def run_checks(root, server_port, cert):
    parser = argparse.ArgumentParser()
    parser.add_argument("--test-binary", required=True)
    parser.add_argument("--app-binary")
    args = parser.parse_args()
    marker = root / "drop-proxy"
    stop = threading.Event()
    listener = socket.socket()
    listener.bind(("127.0.0.1", 0))
    listener.listen()
    listener.settimeout(.1)
    clients = []
    def proxy():
        pairs = {}
        try:
            while not stop.is_set():
                if marker.exists():
                    marker.unlink()
                    for sock in list(pairs):
                        sock.close()
                    pairs.clear()
                try:
                    client, _ = listener.accept()
                    upstream = socket.create_connection(("127.0.0.1", server_port))
                    pairs[client] = upstream
                    pairs[upstream] = client
                except socket.timeout:
                    pass
                if not pairs:
                    continue
                readable, _, _ = select.select(list(pairs), [], [], .01)
                for sock in readable:
                    if sock not in pairs:
                        continue
                    try:
                        data = sock.recv(65536)
                        if data:
                            pairs[sock].sendall(data)
                            continue
                    except OSError:
                        pass
                    other = pairs.pop(sock)
                    pairs.pop(other, None)
                    sock.close()
                    other.close()
        finally:
            for sock in list(pairs):
                sock.close()
    thread = threading.Thread(target=proxy)
    thread.start()
    env = dict(os.environ, MONOGRAM_IT_PORT=str(listener.getsockname()[1]), MONOGRAM_IT_RECONNECT=str(marker), SSL_CERT_FILE=str(cert))
    def run(name, environment):
        subprocess.run([args.test_binary, "--ignored", "--exact", name, "--nocapture"], env=environment, check=True, timeout=110)
    try:
        run("live_roundtrip", env)
        if args.app_binary:
            subprocess.run([args.app_binary, "--live-smoke-test"], env=env, check=True, timeout=50)
        empty = root / "empty-ca.pem"
        empty.write_text("")
        run("tls_rejection", dict(env, SSL_CERT_FILE=str(empty), SSL_CERT_DIR=str(root / "empty-ca")))
        # Endpoint override must never override the service's TLS identity.
        run("tls_rejection", dict(env, MONOGRAM_IT_DOMAIN="wrong.localhost"))
        plain = socket.socket()
        plain.bind(("127.0.0.1", 0))
        plain.listen()
        transcript = []
        def plaintext_server():
            client, _ = plain.accept()
            with client:
                client.settimeout(5)
                transcript.append(client.recv(4096))
                client.sendall(b"<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='localhost' id='plain' version='1.0'><stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism></mechanisms></stream:features>")
                try:
                    transcript.append(client.recv(4096))
                except (TimeoutError, ConnectionResetError):
                    pass
        server_thread = threading.Thread(target=plaintext_server)
        server_thread.start()
        try:
            run("tls_rejection", dict(env, MONOGRAM_IT_PORT=str(plain.getsockname()[1])))
            server_thread.join(6)
            assert b"<auth" not in b"".join(transcript), "SASL leaked without TLS"
        finally:
            plain.close()
        print("Untrusted certificates and missing STARTTLS rejected without plaintext SASL", flush=True)
    finally:
        stop.set()
        thread.join(2)
        listener.close()


if __name__ == "__main__":
    main()
