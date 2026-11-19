#!/usr/bin/env python3
"""Trusted synchronous JS; use DOM clicks and poll subsequent snapshots for async work."""
import argparse
import json
import socket
import uuid
from pathlib import Path

parser = argparse.ArgumentParser(__doc__)
parser.add_argument('--home', required=True)
parser.add_argument('--script', type=Path)
parser.add_argument("--shutdown", action="store_true")
args = parser.parse_args()
script = args.script.read_text() if args.script else 'return {title:document.title,text:document.body.innerText};'
request = b'{"shutdown":true}' if args.shutdown else json.dumps({'id': str(uuid.uuid4()), 'script': script}).encode()
if len(request) > 65536:
    parser.error('request exceeds 64 KiB')
with socket.socket(socket.AF_UNIX) as client:
    client.settimeout(10)
    client.connect(str(Path(args.home) / '.qa-driver/control.sock'))
    client.sendall(request)
    client.shutdown(socket.SHUT_WR)
    response = bytearray()
    while chunk := client.recv(4096):
        response.extend(chunk)
        if len(response) > 262144:
            raise SystemExit('response exceeds limit')
reply = json.loads(response)
if reply.get("error"):
    raise SystemExit(reply["error"])
value = reply.get("result")
while isinstance(value, str):
    value = json.loads(value)
print(json.dumps(value if value is not None else reply))
if isinstance(value, dict) and value.get("ok") is False:
    raise SystemExit(1)
