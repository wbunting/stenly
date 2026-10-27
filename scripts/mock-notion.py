#!/usr/bin/env python3
"""A tiny stand-in for the Notion REST API, for trying the Notion destination
without a real workspace: `python3 scripts/mock-notion.py 8787`, then set the
destination's `api_base = "http://127.0.0.1:8787/v1"` and any token.

Serves a "Meetings" database (Name/Date/Attendees/Tags/Meeting link/Company)
and a "Companies" database (Acme Corp → acme.com, Beta → beta.io), and keeps
created pages in memory. GET /_pages dumps what was created/updated."""
import json, sys, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MEETINGS = "11111111-2222-3333-4444-555555555555"
COMPANIES = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
DBS = {
    MEETINGS: {"object": "database", "id": MEETINGS, "title": [{"plain_text": "Meetings"}], "properties": {
        "Name": {"type": "title"}, "Date": {"type": "date"}, "Attendees": {"type": "multi_select"},
        "Tags": {"type": "multi_select"}, "Meeting link": {"type": "url"}, "Notes": {"type": "rich_text"},
        "Company": {"type": "relation", "relation": {"database_id": COMPANIES}}}},
    COMPANIES: {"object": "database", "id": COMPANIES, "title": [{"plain_text": "Companies"}], "properties": {
        "Name": {"type": "title"}, "Website": {"type": "url"}, "Industry": {"type": "select"}}},
}
COMPANY_ROWS = [("acme-corp", "Acme Corp", "https://www.acme.com"), ("beta", "Beta", "beta.io"), ("example-labs", "Example Labs", "https://www.guest.example.org")]
PAGES = {}
BLOCKS = {}

class H(BaseHTTPRequestHandler):
    def reply(self, code, obj):
        b = json.dumps(obj).encode()
        self.send_response(code); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(n) or b"{}")
    def auth(self):
        if not (self.headers.get("Authorization") or "").startswith("Bearer "):
            self.reply(401, {"object": "error", "status": 401, "code": "unauthorized", "message": "API token is invalid."}); return False
        return True
    def do_GET(self):
        p = self.path.split("?")[0]
        if p == "/_pages": return self.reply(200, {"pages": PAGES, "blocks": BLOCKS})
        if not self.auth(): return
        if p == "/v1/users/me": return self.reply(200, {"object": "user", "type": "bot", "name": "Stenly (mock)"})
        if p == "/v1/users": return self.reply(200, {"results": [], "has_more": False})
        if p.startswith("/v1/databases/"):
            d = DBS.get(p.split("/")[3]);
            return self.reply(200, d) if d else self.reply(404, {"object": "error", "status": 404, "code": "object_not_found", "message": "Could not find database"})
        if p.startswith("/v1/pages/"):
            pid = p.split("/")[3]
            return self.reply(200, PAGES[pid]) if pid in PAGES else self.reply(404, {"object": "error", "status": 404, "code": "object_not_found", "message": "Could not find page"})
        if p.startswith("/v1/blocks/"):
            bid = p.split("/")[3]
            return self.reply(200, {"results": [{"id": i} for i in BLOCKS.get(bid, [])], "has_more": False})
        self.reply(400, {"object": "error", "status": 400, "code": "invalid_request_url", "message": "no route " + p})
    def do_POST(self):
        if not self.auth(): return
        p = self.path.split("?")[0]; b = self.body()
        if p == "/v1/search": return self.reply(200, {"results": list(DBS.values()), "has_more": False})
        if p == f"/v1/databases/{COMPANIES}/query":
            f = b.get("filter", {}); needle = json.dumps(f).lower()
            res = [{"id": i, "properties": {"Name": {"type": "title", "title": [{"plain_text": n}]}, "Website": {"type": "url", "url": w}}}
                   for (i, n, w) in COMPANY_ROWS if any(x in needle for x in [w.split("//")[-1].replace("www.", ""), n.lower()])]
            return self.reply(200, {"results": res, "has_more": False})
        if p == "/v1/pages":
            pid = str(uuid.uuid4()); PAGES[pid] = {"id": pid, "url": f"https://www.notion.so/mock-{pid.replace('-', '')}", "archived": False, "properties": b.get("properties")}
            BLOCKS[pid] = [str(uuid.uuid4()) for _ in b.get("children", [])]
            return self.reply(200, PAGES[pid])
        self.reply(400, {"object": "error", "status": 400, "code": "invalid_request_url", "message": "no route " + p})
    def do_PATCH(self):
        if not self.auth(): return
        p = self.path.split("?")[0]; b = self.body()
        if p.startswith("/v1/pages/"):
            pid = p.split("/")[3]; PAGES[pid]["properties"] = b.get("properties"); return self.reply(200, PAGES[pid])
        if p.startswith("/v1/blocks/"):
            bid = p.split("/")[3]; ids = [str(uuid.uuid4()) for _ in b.get("children", [])]
            BLOCKS.setdefault(bid, []).extend(ids); return self.reply(200, {"results": [{"id": i} for i in ids]})
        self.reply(400, {"object": "error", "status": 400, "message": "no route"})
    def do_DELETE(self):
        if not self.auth(): return
        bid = self.path.split("/")[3]
        for k in BLOCKS: BLOCKS[k] = [x for x in BLOCKS[k] if x != bid]
        self.reply(200, {"id": bid, "archived": True})
    def log_message(self, fmt, *a): sys.stderr.write("mock-notion: " + (fmt % a) + "\n")

if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1]) if len(sys.argv) > 1 else 8787), H).serve_forever()
