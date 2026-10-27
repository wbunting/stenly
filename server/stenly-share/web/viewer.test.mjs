// Browser-side tests for web/viewer.js, run with Node's built-in test runner
// against the same WebCrypto API browsers use:  node --test web/
import assert from "node:assert/strict";
import { test } from "node:test";

import { ShareError, b64urlDecode, decryptEnvelope, fmtClock, inline, keyFromHash, markdown, parseNote, restoreKey, safeHref, stashKey, textOf, turns } from "./viewer.js";

const KEY = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"; // gitleaks:allow -- public ascending bytes 0..31 WebCrypto test vector, never a live key
const ENVELOPE =
  "AaChoqOkpaanqKmqq506Cg9_-i6dFgzzv2JY-vwkySpkssQ2DfJqU_Zdh1dsswQsm8BVPR9lvifoXR_wjWdoMikMtG8OHTBXMIlQ1tjdzKUGRIeZOWidVgcd55Jbwe3HNthABw";
const PLAINTEXT = '{"v":1,"title":"Test standup","markdown":"# Test standup\\n\\n- Ship it"}';

test("decrypts the contract test vector with WebCrypto", async () => {
  const key = keyFromHash("#" + KEY);
  assert.deepEqual([...key], Array.from({ length: 32 }, (_, i) => i));
  const text = await decryptEnvelope(key, b64urlDecode(ENVELOPE));
  assert.equal(text, PLAINTEXT);
  const note = parseNote(text);
  assert.equal(note.title, "Test standup");
  assert.equal(note.markdown, "# Test standup\n\n- Ship it");
  assert.equal(textOf(markdown(note.markdown)), "Test standupShip it");
});

test("a wrong key, a tampered envelope and bad versions are rejected", async () => {
  const wrong = new Uint8Array(32).fill(9);
  await assert.rejects(decryptEnvelope(wrong, b64urlDecode(ENVELOPE)), (e) => e instanceof ShareError && e.code === "wrong_key");
  const tampered = b64urlDecode(ENVELOPE);
  tampered[40] ^= 1;
  await assert.rejects(decryptEnvelope(keyFromHash(KEY), tampered), (e) => e.code === "wrong_key");
  const v2 = b64urlDecode(ENVELOPE);
  v2[0] = 2;
  await assert.rejects(decryptEnvelope(keyFromHash(KEY), v2), (e) => e.code === "version");
  await assert.rejects(decryptEnvelope(keyFromHash(KEY), new Uint8Array(10).fill(1)), (e) => e.code === "corrupt");
});

test("missing and malformed keys", () => {
  assert.throws(() => keyFromHash(""), (e) => e.code === "no_key");
  assert.throws(() => keyFromHash("#"), (e) => e.code === "no_key");
  assert.throws(() => keyFromHash("#" + KEY.slice(0, 20)), (e) => e.code === "bad_key");
  assert.throws(() => keyFromHash("#not*base64"), (e) => e.code === "bad_key");
  assert.throws(() => keyFromHash("#abc%"), (e) => e.code === "bad_key");
  assert.throws(() => keyFromHash("#" + KEY.slice(0, 40) + "%E0"), (e) => e.code === "bad_key");
});

test("plaintext: optional fields and unknown fields", () => {
  const n = parseNote(JSON.stringify({ v: 1, title: "", markdown: "x", future: { a: 1 }, attendees: [{ name: "A" }, null, {}], transcript: [{ speaker: "S", start: 1.5, text: "hi" }, { text: "" }] }));
  assert.equal(n.title, "Meeting notes");
  assert.deepEqual(n.attendees, [{ name: "A", email: "" }]);
  assert.equal(n.transcript.length, 1);
  assert.equal(n.date, null);
  assert.equal(n.durationSecs, null);
  assert.throws(() => parseNote("{"), (e) => e.code === "corrupt");
  assert.throws(() => parseNote('{"v":2,"title":"x","markdown":""}'), (e) => e.code === "version");
});

test("markdown: what Stenly notes use", () => {
  const md = "## Summary\n\nWe met.\nIt went well.\n\n### Pilot\n\n- Start in Nov\n    - Two teams\n- Budget\n\n## Action items\n\n- [ ] **Me**: Send it\n- [x] Done thing\n\n1. one\n2. two\n\n> [!warning] Summary failed: x\n";
  const tree = markdown(md);
  assert.deepEqual(
    tree.map((n) => n.t),
    ["h2", "p", "h3", "ul", "h2", "ul", "ol", "aside"],
  );
  assert.deepEqual(tree[1].c, ["We met.", { t: "br", c: [] }, "It went well."]);
  const pilot = tree[3];
  assert.equal(pilot.c.length, 2);
  assert.equal(pilot.c[0].c[1].t, "ul"); // nested list
  const tasks = tree[5];
  assert.equal(tasks.a.class, "tasks");
  assert.equal(tasks.c[0].a.class, "task");
  assert.equal(tasks.c[1].a.class, "task done");
  assert.equal(tree[7].a.class, "callout callout-warning");
});

test("markdown is rendered as data, never HTML", () => {
  const tree = markdown('<img src=x onerror=alert(1)>\n\n[click](javascript:alert(1)) and [ok](https://ex.com)');
  assert.equal(tree[0].t, "p");
  assert.equal(tree[0].c[0], "<img src=x onerror=alert(1)>");
  const links = tree[1].c.filter((n) => typeof n === "object" && n.t === "a");
  assert.equal(links.length, 1);
  assert.equal(links[0].a.href, "https://ex.com");
  assert.equal(safeHref("javascript:alert(1)"), null);
  assert.equal(safeHref("mailto:a@b.c"), "mailto:a@b.c");
});

test("inline: code, emphasis, autolinks, snake_case", () => {
  assert.deepEqual(inline("use `a*b*c` now"), ["use ", { t: "code", c: ["a*b*c"] }, " now"]);
  assert.deepEqual(inline("*hi*"), [{ t: "em", c: ["hi"] }]);
  assert.deepEqual(inline("snake_case_name"), ["snake_case_name"]);
  const auto = inline("see https://stenly.ai/x.");
  assert.equal(auto[1].a.href, "https://stenly.ai/x");
  assert.equal(auto[2], ".");
});

test("transcript helpers", () => {
  assert.equal(fmtClock(12.5), "0:12");
  assert.equal(fmtClock(3725), "1:02:05");
  const t = turns([
    { speaker: "A", start: 1, text: "x" },
    { speaker: "A", start: 2, text: "y" },
    { speaker: "B", start: 3, text: "z" },
  ]);
  assert.deepEqual(t, [
    { speaker: "A", start: 1, texts: ["x", "y"] },
    { speaker: "B", start: 3, texts: ["z"] },
  ]);
});

test("company mode: the key survives the sign-in round trip (sessionStorage)", () => {
  const mem = new Map();
  const storage = { setItem: (k, v) => mem.set(k, v), getItem: (k) => mem.get(k) ?? null, removeItem: (k) => mem.delete(k) };
  stashKey(storage, "/s/abc", "#" + KEY);
  // Back from the IdP without a fragment: restored once, then forgotten.
  assert.equal(restoreKey(storage, "/s/abc", ""), "#" + KEY);
  assert.equal(restoreKey(storage, "/s/abc", ""), "");
  // A fragment in the URL wins; other shares don't see this key.
  stashKey(storage, "/s/abc", "#" + KEY);
  assert.equal(restoreKey(storage, "/s/other", ""), "");
  assert.equal(restoreKey(storage, "/s/abc", "#explicit"), "#explicit");
});
