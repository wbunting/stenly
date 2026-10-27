import { test } from "node:test";
import assert from "node:assert/strict";
import { output, parseHistory } from "./history.ts";

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
