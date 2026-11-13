#!/usr/bin/env python3
"""Fictional native-messaging peer using independent libsodium (PyNaCl)."""
import base64
import json
import os
import struct
import sys
import time
from pathlib import Path
from nacl.public import Box, PrivateKey, PublicKey


def encode(value):
    return base64.b64encode(value).decode()


def increment(value):
    return (int.from_bytes(value, 'little') + 1).to_bytes(24, 'little')


key = PrivateKey.generate()
box = None
state = Path(os.environ['MONOGRAM_KEEPASS_FIXTURE_STATE'])
while True:
    header = sys.stdin.buffer.read(4)
    if not header:
        break
    length, = struct.unpack('<I', header)
    assert 0 < length <= 1024 * 1024
    request = json.loads(sys.stdin.buffer.read(length))
    action = request['action']
    if os.environ.get('MONOGRAM_KEEPASS_FIXTURE_SLOW'):
        time.sleep(2)
    nonce = increment(base64.b64decode(request['nonce']))
    if action == 'change-public-keys':
        box = Box(key, PublicKey(base64.b64decode(request['publicKey'])))
        response = dict(action=action, success='true', nonce=encode(nonce), publicKey=encode(key.public_key.encode()))
    else:
        data = json.loads(box.decrypt(base64.b64decode(request['message']), base64.b64decode(request['nonce'])))
        assert data['action'] == action and data['nonce'] == request['nonce']
        stored = json.loads(state.read_text()) if state.exists() else {'password': 'fictional login'}
        response = dict(success='true', nonce=encode(nonce), hash='fictional-database', version='2.8.0')
        if action == 'associate':
            stored['key'] = data['idKey']
            response['id'] = 'Monogram fixture'
        elif action == 'test-associate':
            assert data['key'] == stored['key'] and data['id'] == 'Monogram fixture'
            response['id'] = data['id']
        elif action == 'get-logins':
            assert data['url'] == data['submitUrl'] == 'xmpp://example.invalid'
            assert data['keys'] == [dict(id='Monogram fixture', key=stored['key'])]
            response['entries'] = [dict(login='alice@example.invalid', password=stored['password'], uuid='fictional-entry')]
        elif action == 'set-login':
            assert data['login'] == 'alice@example.invalid' and data['uuid'] == 'fictional-entry'
            stored['password'] = data['password']
        else:
            assert action == 'get-databasehash'
        state.write_text(json.dumps(stored))
        response = dict(action=action, nonce=encode(nonce), message=encode(box.encrypt(json.dumps(response).encode(), nonce).ciphertext))
        if os.environ.get('MONOGRAM_KEEPASS_FIXTURE_TAMPER'):
            response['nonce'] = encode(bytes(24))
    result = json.dumps(response).encode()
    sys.stdout.buffer.write(struct.pack('<I', len(result)) + result)
    sys.stdout.buffer.flush()
