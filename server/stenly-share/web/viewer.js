// Stenly share viewer: decrypts an end-to-end encrypted note in the browser.
//
// The key lives in the URL fragment (`/s/<id>#<key>`), which browsers never
// send to the server. This file is served as a same-origin module with a
// strict CSP (no inline scripts, no eval, no third-party code), and renders
// note Markdown into DOM nodes with textContent only: no innerHTML.
//
// It is also an ES module for tests: `node --test web/` imports the pure
// helpers below (no DOM needed).

export const AAD = "stenly-share-v1";
export const VERSION = 1;

// ------------------------------------------------------------------ crypto

/** base64url (no padding) → Uint8Array. Throws on invalid input. */
export function b64urlDecode(s) {
  if (!/^[A-Za-z0-9_-]*$/.test(s)) throw new Error("not base64url");
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export class ShareError extends Error {
  constructor(code, message) {
    super(message || code);
    this.code = code;
  }
}

/** Parses the key from a location hash (`#<43 chars>`). */
export function keyFromHash(hash) {
  const raw = (hash || "").replace(/^#/, "").trim();
  if (!raw) throw new ShareError("no_key");
  let k;
  try {
    k = decodeURIComponent(raw).trim();
  } catch {
    // A truncated or mangled link (e.g. `#abc%`).
    throw new ShareError("bad_key");
  }
  let bytes;
  try {
    bytes = b64urlDecode(k);
  } catch {
    throw new ShareError("bad_key");
  }
  if (bytes.length !== 32) throw new ShareError("bad_key");
  return bytes;
}

/**
 * Decrypts a v1 envelope `[0x01][12-byte nonce][ciphertext || tag]` with
 * AES-256-GCM (AAD `stenly-share-v1`) and returns the plaintext string.
 */
export async function decryptEnvelope(keyBytes, envelope, subtle = globalThis.crypto && globalThis.crypto.subtle) {
  if (!subtle) throw new ShareError("insecure");
  if (envelope.length < 29) throw new ShareError("corrupt");
  if (envelope[0] !== VERSION) throw new ShareError("version");
  const key = await subtle.importKey("raw", keyBytes, { name: "AES-GCM" }, false, ["decrypt"]);
  let plain;
  try {
    plain = await subtle.decrypt(
      { name: "AES-GCM", iv: envelope.subarray(1, 13), additionalData: new TextEncoder().encode(AAD), tagLength: 128 },
      key,
      envelope.subarray(13),
    );
  } catch {
    throw new ShareError("wrong_key");
  }
  return new TextDecoder().decode(plain);
}

/** Parses and normalizes the plaintext note (unknown fields are ignored). */
export function parseNote(text) {
  let v;
  try {
    v = JSON.parse(text);
  } catch {
    throw new ShareError("corrupt");
  }
  if (!v || typeof v !== "object") throw new ShareError("corrupt");
  if (typeof v.v === "number" && v.v > 1) throw new ShareError("version");
  const str = (x) => (typeof x === "string" ? x : "");
  return {
    title: str(v.title).trim() || "Meeting notes",
    date: str(v.date) || null,
    durationSecs: Number.isFinite(v.duration_secs) && v.duration_secs > 0 ? v.duration_secs : null,
    attendees: Array.isArray(v.attendees)
      ? v.attendees.filter((a) => a && typeof a === "object").map((a) => ({ name: str(a.name).trim(), email: str(a.email).trim() })).filter((a) => a.name || a.email)
      : [],
    markdown: str(v.markdown),
    transcript: Array.isArray(v.transcript)
      ? v.transcript.filter((l) => l && typeof l === "object" && str(l.text)).map((l) => ({ speaker: str(l.speaker).trim(), start: Number.isFinite(l.start) ? l.start : null, text: str(l.text) }))
      : [],
    app: str(v.app),
  };
}

// ------------------------------------------------------------------ markdown
//
// A small, safe Markdown subset covering what Stenly writes: headings,
// paragraphs, nested bullet/numbered lists, task items, block quotes and
// Obsidian callouts, fenced code, rules, **bold**, *italic*, `code`, and
// http(s)/mailto links. Output is a plain tree `{t, a?, c?}` / string, which
// `toDom` turns into nodes. Raw HTML in the Markdown is shown as text.

const h = (t, c = [], a) => (a ? { t, a, c } : { t, c });

export function safeHref(url) {
  const u = url.trim();
  return /^(https?:|mailto:)/i.test(u) ? u : null;
}

/** Inline Markdown → array of nodes. */
export function inline(text) {
  const out = [];
  let buf = "";
  const flush = () => {
    if (buf) out.push(buf);
    buf = "";
  };
  let i = 0;
  while (i < text.length) {
    const rest = text.slice(i);
    let m;
    if (rest[0] === "\\" && /^\\[\\`*_[\]()#!>-]/.test(rest)) {
      buf += rest[1];
      i += 2;
    } else if ((m = /^`([^`]+)`/.exec(rest))) {
      flush();
      out.push(h("code", [m[1]]));
      i += m[0].length;
    } else if ((m = /^(\*\*|__)(?=\S)([\s\S]*?\S)\1/.exec(rest))) {
      flush();
      out.push(h("strong", inline(m[2])));
      i += m[0].length;
    } else if ((m = /^(\*|_)(?=\S)([\s\S]*?\S)\1(?![*_])/.exec(rest)) && !(m[1] === "_" && /\w/.test(text[i - 1] || ""))) {
      flush();
      out.push(h("em", inline(m[2])));
      i += m[0].length;
    } else if ((m = /^~~(?=\S)([\s\S]*?\S)~~/.exec(rest))) {
      flush();
      out.push(h("del", inline(m[1])));
      i += m[0].length;
    } else if ((m = /^\[([^\]]+)\]\(([^)\s]+)\)/.exec(rest))) {
      flush();
      const href = safeHref(m[2]);
      out.push(href ? h("a", inline(m[1]), { href, rel: "noopener noreferrer nofollow", target: "_blank" }) : m[1]);
      i += m[0].length;
    } else if ((m = /^\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/.exec(rest))) {
      // Obsidian wiki link: show its label as text.
      buf += m[2] || m[1];
      i += m[0].length;
    } else if ((m = /^https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"]/.exec(rest)) && !/\w/.test(text[i - 1] || "")) {
      flush();
      out.push(h("a", [m[0]], { href: m[0], rel: "noopener noreferrer nofollow", target: "_blank" }));
      i += m[0].length;
    } else {
      buf += rest[0];
      i += 1;
    }
  }
  flush();
  return out;
}

const indentOf = (line) => line.replace(/\t/g, "    ").match(/^ */)[0].length;
const LIST = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;

/** Block Markdown → array of nodes. */
export function markdown(src) {
  const lines = src.replace(/\r\n?/g, "\n").split("\n");
  // Drop YAML frontmatter if a note carries it.
  if (lines[0] === "---") {
    const end = lines.indexOf("---", 1);
    if (end > 0) lines.splice(0, end + 1);
  }
  return blocks(lines);
}

function blocks(lines) {
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    let m;
    if (!line.trim()) {
      i++;
    } else if ((m = /^\s*(```|~~~)(.*)$/.exec(line))) {
      const fence = m[1];
      const body = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence)) body.push(lines[i++]);
      i++;
      out.push(h("pre", [h("code", [body.join("\n")])]));
    } else if ((m = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line))) {
      out.push(h("h" + m[1].length, inline(m[2])));
      i++;
    } else if (/^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(line)) {
      out.push(h("hr"));
      i++;
    } else if (/^\s{0,3}>/.test(line)) {
      const body = [];
      while (i < lines.length && /^\s{0,3}>/.test(lines[i])) body.push(lines[i++].replace(/^\s{0,3}> ?/, ""));
      out.push(quote(body));
    } else if (LIST.test(line)) {
      const start = i;
      const base = indentOf(line);
      const ordered = /^\s*\d/.test(line);
      // A top-level item of the other kind (bullet vs number) starts a new list.
      const sameKind = (l) => indentOf(l) > base || (LIST.test(l) && /^\s*\d/.test(l) === ordered);
      i++;
      while (i < lines.length) {
        const l = lines[i];
        if (!l.trim()) {
          // A blank line continues the list only if more list content follows.
          const next = lines.slice(i + 1).find((x) => x.trim());
          if (next !== undefined && (indentOf(next) > base || (LIST.test(next) && indentOf(next) >= base && sameKind(next)))) {
            i++;
            continue;
          }
          break;
        }
        if (indentOf(l) < base) break;
        if (indentOf(l) === base && !(LIST.test(l) && sameKind(l))) break;
        i++;
      }
      out.push(list(lines.slice(start, i)));
    } else {
      const para = [];
      while (i < lines.length && lines[i].trim() && !/^\s{0,3}(#{1,6}\s|>|```|~~~)/.test(lines[i]) && !LIST.test(lines[i])) para.push(lines[i++].trim());
      const c = [];
      para.forEach((p, k) => {
        if (k) c.push(h("br"));
        c.push(...inline(p));
      });
      out.push(h("p", c));
    }
  }
  return out;
}

function quote(body) {
  const m = /^\[!(\w+)\]([+-]?)\s*(.*)$/.exec(body[0] || "");
  if (!m) return h("blockquote", blocks(body));
  const kind = m[1].toLowerCase();
  const titleText = m[3] || kind[0].toUpperCase() + kind.slice(1);
  const title = h("p", inline(titleText), { class: "callout-title" });
  return h("aside", [title, ...blocks(body.slice(1))], { class: "callout callout-" + kind.replace(/[^a-z]/g, "") });
}

function list(lines) {
  const base = indentOf(lines[0]);
  const ordered = /^\s*\d/.test(lines[0]);
  const items = [];
  let cur = null;
  for (const l of lines) {
    const m = LIST.exec(l);
    if (m && indentOf(l) === base) {
      cur = { first: m[3], rest: [] };
      items.push(cur);
    } else if (cur) {
      cur.rest.push(l.slice(Math.min(base + 2, indentOf(l))));
    }
  }
  return h(
    ordered ? "ol" : "ul",
    items.map(({ first, rest }) => {
      const task = /^\[([ xX])\]\s+(.*)$/.exec(first);
      const head = inline(task ? task[2] : first);
      const nested = rest.some((r) => r.trim()) ? blocks(rest) : [];
      if (!task) return h("li", [...head, ...nested]);
      const done = task[1] !== " ";
      return h("li", [h("span", [], { class: "task-box" + (done ? " done" : ""), "aria-hidden": "true" }), h("span", head, { class: "task-text" }), ...nested], {
        class: "task" + (done ? " done" : ""),
      });
    }),
    ordered ? undefined : rest_class(items),
  );
}

function rest_class(items) {
  return items.some(({ first }) => /^\[[ xX]\]\s/.test(first)) ? { class: "tasks" } : undefined;
}

/** Plain text of a tree (for tests and the copy button). */
export function textOf(nodes) {
  return nodes.map((n) => (typeof n === "string" ? n : n.t === "br" ? "\n" : textOf(n.c || []))).join("");
}

// ------------------------------------------------------------------ formatting

export function fmtClock(secs) {
  if (!Number.isFinite(secs) || secs < 0) return "";
  const s = Math.floor(secs);
  const hh = Math.floor(s / 3600);
  const mm = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return hh ? `${hh}:${String(mm).padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
}

export function fmtDuration(secs) {
  const m = Math.round(secs / 60);
  if (m < 1) return "under a minute";
  if (m < 60) return `${m} min`;
  const hh = Math.floor(m / 60);
  return m % 60 ? `${hh} h ${m % 60} min` : `${hh} h`;
}

/** Groups consecutive transcript lines by speaker. */
export function turns(lines) {
  const out = [];
  for (const l of lines) {
    const last = out[out.length - 1];
    if (last && last.speaker === l.speaker) last.texts.push(l.text);
    else out.push({ speaker: l.speaker, start: l.start, texts: [l.text] });
  }
  return out;
}

// ------------------------------------------------------------------ DOM

function el(tag, attrs, children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
  for (const c of children || []) e.append(typeof c === "string" ? document.createTextNode(c) : c);
  return e;
}

export function toDom(nodes) {
  return nodes.map((n) => (typeof n === "string" ? document.createTextNode(n) : el(n.t, n.a, toDom(n.c || []))));
}

const STATES = {
  no_key: ["This link is missing its key", "Share links end with #… — the key that unlocks the note. It never reaches our server. Ask the sender for the full link."],
  bad_key: ["This link’s key is incomplete", "The part after # looks cut off. Copy the whole link again, or ask the sender to resend it."],
  wrong_key: ["This key doesn’t unlock the note", "The link’s key doesn’t match this note — it may be from an older share, or was edited. Ask the sender for the current link."],
  revoked: ["This note was unshared", "Its owner revoked the link, and the encrypted copy has been deleted from this server."],
  expired: ["This link has expired", "Shared notes are deleted after their expiry date. Ask the sender to share it again."],
  not_found: ["No note at this address", "This link doesn’t exist on this server. Check that you copied it correctly."],
  version: ["This note needs a newer viewer", "It was shared by a newer version of Stenly. Try again after this server is updated."],
  corrupt: ["This note couldn’t be read", "The encrypted data is damaged. Ask the sender to share it again."],
  insecure: ["This page needs HTTPS", "Your browser only decrypts notes on secure (https://) pages. Ask the server’s admin to enable TLS."],
  rate_limited: ["Too many requests", "Wait a minute and reload the page."],
  sign_in: ["Sign in to view this note", "This server shares notes only with signed-in people from your company."],
  network: ["Couldn’t reach the server", "Check your connection and reload the page."],
};

function showState(root, code) {
  const [title, body] = STATES[code] || STATES.corrupt;
  root.dataset.state = code;
  root.replaceChildren(el("section", { class: "state", role: "status" }, [el("h1", { class: "state-title" }, [title]), el("p", { class: "state-body" }, [body])]));
  document.title = `${title} · Stenly`;
}

function render(root, note, link) {
  const meta = [];
  if (note.date) {
    const d = new Date(note.date);
    if (!Number.isNaN(d.getTime())) meta.push(el("time", { datetime: note.date }, [d.toLocaleString(undefined, { dateStyle: "full", timeStyle: "short" })]));
  }
  if (note.durationSecs) meta.push(el("span", {}, [fmtDuration(note.durationSecs)]));

  const header = el("header", { class: "note-head" }, [el("h1", { class: "note-title" }, [note.title]), meta.length ? el("p", { class: "note-meta" }, meta) : ""]);
  if (note.attendees.length) {
    header.append(
      el(
        "ul",
        { class: "attendees", "aria-label": "Attendees" },
        note.attendees.map((a) => el("li", { title: a.email || "" }, [a.name || a.email])),
      ),
    );
  }

  const tree = markdown(note.markdown);
  const body = el("div", { class: "prose" }, toDom(tree));
  const parts = [header, body];

  if (note.transcript.length) {
    const t = turns(note.transcript);
    const words = note.transcript.reduce((n, l) => n + l.text.split(/\s+/).filter(Boolean).length, 0);
    parts.push(
      el("details", { class: "transcript" }, [
        el("summary", {}, [el("span", {}, ["Transcript"]), el("span", { class: "transcript-count" }, [`${words.toLocaleString()} words`])]),
        el(
          "ol",
          { class: "turns" },
          t.map((turn) =>
            el("li", { class: "turn" }, [
              el("p", { class: "turn-head" }, [el("span", { class: "speaker" }, [turn.speaker || "Speaker"]), turn.start != null ? el("time", {}, [fmtClock(turn.start)]) : ""]),
              ...turn.texts.map((x) => el("p", { class: "turn-text" }, [x])),
            ]),
          ),
        ),
      ]),
    );
  }

  const copyNotes = el("button", { type: "button", class: "btn" }, ["Copy notes"]);
  const copyLink = el("button", { type: "button", class: "btn btn-quiet" }, ["Copy link"]);
  const printBtn = el("button", { type: "button", class: "btn btn-quiet" }, ["Print"]);
  const flash = (b, label) => {
    const old = b.textContent;
    b.textContent = label;
    setTimeout(() => (b.textContent = old), 1600);
  };
  const copy = async (b, text) => {
    try {
      await navigator.clipboard.writeText(text);
      flash(b, "Copied");
    } catch {
      flash(b, "Couldn't copy");
    }
  };
  copyNotes.addEventListener("click", () => copy(copyNotes, `# ${note.title}\n\n${note.markdown.trim()}\n`));
  copyLink.addEventListener("click", () => copy(copyLink, link));
  printBtn.addEventListener("click", () => window.print());

  root.dataset.state = "ready";
  root.replaceChildren(el("div", { class: "actions" }, [copyNotes, copyLink, printBtn]), el("article", { class: "note" }, parts));
  document.title = `${note.title} · Stenly`;
}

// Company mode: identity-provider redirects drop the URL fragment, so the
// key is parked in sessionStorage (this tab only, never sent anywhere) while
// the reader signs in, and put back into the address bar on return.
const STASH = "stenly-share-key:";

export function stashKey(storage, path, hash) {
  if (hash && hash.length > 1) storage.setItem(STASH + path, hash);
}

export function restoreKey(storage, path, hash) {
  if (hash && hash.length > 1) return hash;
  const saved = storage.getItem(STASH + path);
  if (saved) storage.removeItem(STASH + path);
  return saved || "";
}

function signInGate(root) {
  const link = document.getElementById("sign-in-link");
  if (!link) return;
  link.addEventListener("click", () => stashKey(sessionStorage, location.pathname, location.hash));
}

async function boot() {
  const root = document.getElementById("note");
  if (!root) return;
  if (root.dataset.state === "sign_in") return signInGate(root);
  if (!root.dataset.id) return;
  const restored = restoreKey(sessionStorage, location.pathname, location.hash);
  if (restored !== location.hash) history.replaceState(null, "", location.pathname + location.search + restored);
  // Print the transcript too.
  addEventListener("beforeprint", () => document.querySelectorAll("details.transcript").forEach((d) => (d.open = true)));
  let key;
  try {
    key = keyFromHash(location.hash);
    if (!(globalThis.crypto && crypto.subtle)) throw new ShareError("insecure");
    const res = await fetch(`/api/v1/shares/${encodeURIComponent(root.dataset.id)}`, { headers: { Accept: "application/json" }, cache: "no-store", credentials: "same-origin" }).catch(() => {
      throw new ShareError("network");
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) {
      const code =
        res.status === 410 ? (body.error === "revoked" ? "revoked" : "expired") : res.status === 404 ? "not_found" : res.status === 429 ? "rate_limited" : res.status === 401 ? "sign_in" : "network";
      throw new ShareError(code);
    }
    let envelope;
    try {
      envelope = b64urlDecode(body.ciphertext || "");
    } catch {
      throw new ShareError("corrupt");
    }
    const note = parseNote(await decryptEnvelope(key, envelope));
    render(root, note, location.href);
  } catch (e) {
    showState(root, e instanceof ShareError ? e.code : "corrupt");
    if (!(e instanceof ShareError)) console.error(e);
  }
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", boot);
  else boot();
  addEventListener("hashchange", () => location.reload());
}
