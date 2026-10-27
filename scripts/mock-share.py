#!/usr/bin/env python3
"""In-memory Stenly encrypted-share server for desktop/CLI integration tests.

    STENLY_MOCK_SHARE_TOKEN=test python3 scripts/mock-share.py 8788

It deliberately stores and returns ciphertext only. `GET /_shares` is a test
escape hatch so an independent decryptor can validate the app's upload.
"""
import base64, hashlib, json, os, secrets, sys
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = os.environ.get("STENLY_MOCK_SHARE_TOKEN", "")
SHARES = {}

def b64(n): return base64.urlsafe_b64encode(n).rstrip(b"=").decode()
def now(): return datetime.now(timezone.utc)
def stamp(d): return d.isoformat().replace("+00:00", "Z")
def error(h, status, code, message): h.reply(status, {"error": code, "message": message})

class H(BaseHTTPRequestHandler):
    def reply(self, status, body=None):
        raw = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        if raw: self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw))); self.end_headers()
        if raw: self.wfile.write(raw)
    def body(self):
        try:
            return json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        except json.JSONDecodeError:
            error(self, 400, "bad_json", "body must be JSON"); return None
    def upload_ok(self):
        if TOKEN and self.headers.get("Authorization") != "Bearer " + TOKEN:
            error(self, 401, "unauthorized", "upload token required"); return False
        return True
    def owner(self, item):
        token = self.headers.get("X-Stenly-Owner-Token", "")
        if not secrets.compare_digest(hashlib.sha256(token.encode()).hexdigest(), item["owner_hash"]):
            error(self, 403, "forbidden", "wrong owner token"); return False
        return True
    def payload(self):
        body = self.body()
        if body is None: return None
        cipher = body.get("ciphertext")
        if not isinstance(cipher, str) or not cipher:
            error(self, 400, "invalid_ciphertext", "ciphertext is required"); return None
        if len(cipher) > 2_097_152:
            error(self, 413, "too_large", "ciphertext exceeds max_bytes"); return None
        return body
    def do_GET(self):
        p = self.path.split("?", 1)[0]
        if p == "/healthz": return self.reply(200, {"ok": True})
        if p == "/api/v1/info": return self.reply(200, {"name": "stenly-share", "version": "mock", "max_bytes": 2097152, "default_ttl_days": 90, "upload_token_required": bool(TOKEN), "auth": "none"})
        if p == "/_shares": return self.reply(200, {k: {x: v[x] for x in ("ciphertext", "title", "expires_at", "revoked")} for k,v in SHARES.items()})
        if p.startswith("/s/"):
            # The production server serves the decrypting viewer here. Keeping a
            # tiny HTML placeholder makes the mock contract-compatible without
            # duplicating browser crypto in a test fixture.
            raw = b"<!doctype html><title>Stenly share mock</title>"
            self.send_response(200); self.send_header("Content-Type", "text/html; charset=utf-8"); self.send_header("Content-Length", str(len(raw))); self.end_headers(); self.wfile.write(raw); return
        if p.startswith("/api/v1/shares/"): 
            item = SHARES.get(p.rsplit("/", 1)[-1])
            if not item: return error(self, 404, "not_found", "unknown share")
            if item["revoked"] or now() >= item["expires"]: return error(self, 410, "gone", "share was revoked or expired")
            return self.reply(200, {k:item[k] for k in ("ciphertext", "title", "created_at", "expires_at") if item.get(k) is not None})
        error(self, 404, "not_found", "no route")
    def do_POST(self):
        if self.path.split("?",1)[0] != "/api/v1/shares": return error(self, 404, "not_found", "no route")
        if not self.upload_ok(): return
        body = self.payload()
        if body is None: return
        ident, owner = b64(secrets.token_bytes(16)), b64(secrets.token_bytes(32))
        expiry = now() + timedelta(days=int(body.get("expires_in_days", 90)))
        item = {"ciphertext":body["ciphertext"], "title":body.get("title"), "created_at":stamp(now()), "expires_at":stamp(expiry), "expires":expiry, "owner_hash":hashlib.sha256(owner.encode()).hexdigest(), "revoked":False}
        SHARES[ident] = item
        self.reply(201, {"id":ident, "url":f"http://{self.headers.get('Host')}/s/{ident}", "owner_token":owner, "expires_at":item["expires_at"]})
    def do_PUT(self):
        if not self.upload_ok(): return
        ident = self.path.split("?",1)[0].rsplit("/",1)[-1]; item = SHARES.get(ident)
        if not item: return error(self, 404, "not_found", "unknown share")
        if not self.owner(item): return
        body = self.payload()
        if body is None: return
        item["ciphertext"] = body["ciphertext"]; item["title"] = body.get("title")
        if "expires_in_days" in body:
            item["expires"] = now() + timedelta(days=int(body["expires_in_days"])); item["expires_at"] = stamp(item["expires"])
        # Echoing the supplied owner token satisfies the response shape without
        # retaining its raw value in server state.
        self.reply(200, {"id":ident, "url":f"http://{self.headers.get('Host')}/s/{ident}", "owner_token":self.headers.get("X-Stenly-Owner-Token"), "expires_at":item["expires_at"]})
    def do_DELETE(self):
        ident = self.path.split("?",1)[0].rsplit("/",1)[-1]; item = SHARES.get(ident)
        if not item: return error(self, 404, "not_found", "unknown share")
        if not self.owner(item): return
        item["revoked"] = True; self.reply(204)
    def log_message(self, fmt, *args): sys.stderr.write("mock-share: " + fmt % args + "\n")

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 8788), H).serve_forever()
