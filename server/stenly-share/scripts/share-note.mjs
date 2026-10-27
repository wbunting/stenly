#!/usr/bin/env node
// Encrypts a note per the Stenly share contract v1 and uploads it.
// Prints the full share link (url#key) and the owner token on stderr.
//
//   node scripts/share-note.mjs [server] [note.json] [--title "Opt-in title"] [--days N]
//   SHARE_UPLOAD_TOKEN=… node scripts/share-note.mjs https://share.example.com
//
// No dependencies: Node's built-in crypto and fetch.

import { createCipheriv, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";

const args = process.argv.slice(2);
const flag = (name) => {
  const i = args.indexOf(name);
  if (i < 0) return undefined;
  const v = args[i + 1];
  args.splice(i, 2);
  return v;
};
const title = flag("--title");
const days = flag("--days");
const server = (args[0] || "http://127.0.0.1:8788").replace(/\/$/, "");

const note = args[1]
  ? JSON.parse(readFileSync(args[1], "utf8"))
  : {
      v: 1,
      title: "Q4 planning sync",
      date: "2026-10-24T09:30:00+09:00",
      duration_secs: 1860,
      attendees: [
        { name: "Ana Ito", email: "ana@example.com" },
        { name: "Ben Ruiz", email: "ben@example.com" },
      ],
      markdown:
        "## Summary\n\nWe agreed to ship encrypted share links before the launch and to keep the hosted server free. Self-hosting stays a single Docker image.\n\n### Decisions\n\n- Ship share links in **v0.2**\n- Keys never leave the URL fragment\n    - The server stores only ciphertext\n\n## Action items\n\n- [ ] **Ana**: Write the self-hosting guide\n- [ ] **Ben**: Wire the desktop share button\n- [x] Pick the envelope format\n",
      transcript: [
        { speaker: "Ana Ito", start: 3.2, text: "Okay, let's start with share links." },
        { speaker: "Ana Ito", start: 7.9, text: "The main question is where the key lives." },
        { speaker: "Ben Ruiz", start: 12.5, text: "In the fragment. The browser never sends it, so the server only sees ciphertext." },
        { speaker: "Ana Ito", start: 21.0, text: "Great, then self-hosting is just the same image." },
      ],
      app: "stenly 0.1.0",
      future_field: "viewers ignore unknown fields",
    };

const key = randomBytes(32);
const nonce = randomBytes(12);
const cipher = createCipheriv("aes-256-gcm", key, nonce);
cipher.setAAD(Buffer.from("stenly-share-v1"));
const ct = Buffer.concat([cipher.update(JSON.stringify(note), "utf8"), cipher.final(), cipher.getAuthTag()]);
const envelope = Buffer.concat([Buffer.from([1]), nonce, ct]).toString("base64url");

const body = { ciphertext: envelope };
if (title) body.title = title;
if (days) body.expires_in_days = Number(days);
const headers = { "Content-Type": "application/json" };
if (process.env.SHARE_UPLOAD_TOKEN) headers.Authorization = `Bearer ${process.env.SHARE_UPLOAD_TOKEN}`;

const res = await fetch(`${server}/api/v1/shares`, { method: "POST", headers, body: JSON.stringify(body) });
const out = await res.json();
if (res.status !== 201) {
  console.error(res.status, out);
  process.exit(1);
}
console.error(`owner_token=${out.owner_token} expires_at=${out.expires_at}`);
console.log(`${out.url}#${key.toString("base64url")}`);
