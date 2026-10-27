import { test } from "node:test";
import assert from "node:assert/strict";
import { dataDir, escapeMarkdown, output, parseHistory } from "./history.ts";

// Synthetic history in Stenly's on-disk shape (oldest first).
const SAMPLE = [
  '{"at":"2026-01-02T09:00:00+09:00","duration":2.5,"text":"first note"}',
  "not json",
  "",
  '{"at":"2026-01-02T09:05:00+09:00","duration":4.0,"text":"raw second","enhanced":"Second, cleaned.","app":"TextEdit"}',
].join("\n");

test("parses newest first and skips bad lines", () => {
  const h = parseHistory(SAMPLE);
  assert.equal(h.length, 2);
  assert.equal(h[0].app, "TextEdit");
  assert.equal(h[1].text, "first note");
});

test("output prefers the enhanced text", () => {
  const [newest, oldest] = parseHistory(SAMPLE);
  assert.equal(output(newest), "Second, cleaned.");
  assert.equal(output(oldest), "first note");
});

test("normalizes malformed fields instead of crashing", () => {
  const h = parseHistory(
    [
      '{"at":"2026-01-02T10:00:00+09:00","text":"no duration"}',
      '{"at":"2026-01-02T10:01:00+09:00","duration":"3","text":"string duration","enhanced":7,"app":null}',
      '{"at":"not a date","duration":1,"text":"bad date"}',
      '{"at":"2026-01-02T10:02:00+09:00","duration":1}',
      "null",
      "[1,2]",
    ].join("\n"),
  );
  assert.equal(h.length, 2);
  for (const d of h) {
    assert.equal(d.duration, 0);
    assert.doesNotThrow(() => d.duration.toFixed(1));
  }
  assert.equal(h[0].enhanced, undefined);
  assert.equal(h[0].app, undefined);
});

test("escapeMarkdown neutralizes images, links and HTML", () => {
  const s = escapeMarkdown('see ![x](https://example.com/a.png) and <img src="https://example.com/b">');
  assert.ok(!/!\[/.test(s.replace(/\\!\\\[/g, "")));
  assert.ok(s.includes("\\!\\[x\\]\\(https://example\\.com/a\\.png\\)"));
  assert.ok(s.includes("\\<img src=\"https://example\\.com/b\"\\>"));
  assert.equal(escapeMarkdown("a\nb"), "a  \nb");
});

test("entries with the same timestamp get distinct ids", () => {
  const row = '{"at":"2026-01-02T11:00:00+09:00","duration":1,"text":"same time"}';
  const h = parseHistory(`${row}\n${row}`);
  assert.equal(h.length, 2);
  assert.notEqual(h[0].id, h[1].id);
});

test("dataDir ignores a relative XDG_DATA_HOME like the Rust side", () => {
  const saved = { xdg: process.env.XDG_DATA_HOME, data: process.env.STENLY_DATA_DIR };
  delete process.env.STENLY_DATA_DIR;
  try {
    assert.equal(dataDir("/synthetic/dir"), "/synthetic/dir");
    if (process.platform !== "darwin") {
      process.env.XDG_DATA_HOME = "relative/path";
      assert.ok(!dataDir().startsWith("relative"));
      process.env.XDG_DATA_HOME = "/synthetic/xdg";
      assert.equal(dataDir(), "/synthetic/xdg/stenly");
    }
  } finally {
    if (saved.xdg === undefined) delete process.env.XDG_DATA_HOME;
    else process.env.XDG_DATA_HOME = saved.xdg;
    if (saved.data !== undefined) process.env.STENLY_DATA_DIR = saved.data;
  }
});
