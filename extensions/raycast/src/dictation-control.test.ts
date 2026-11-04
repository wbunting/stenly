import { test } from "node:test";
import assert from "node:assert/strict";
import { toggleScript } from "./dictation-control.ts";

test("uses Stenly's status action without stealing the paste target", () => {
  const script = toggleScript();
  assert.match(script, /bundle identifier is "ai.stenly.app"/);
  assert.match(script, /menu item "Start dictation"/);
  assert.match(script, /AXPress/);
  assert.doesNotMatch(script, /activate|keystroke|key code|Start recording|Quit Stenly/);
});

test("fixture target cannot inject AppleScript", () => {
  assert.match(toggleScript("ai.stenly.dictation-fixture"), /ai.stenly.dictation-fixture/);
  assert.throws(() => toggleScript('bad" & do shell script "oops'), /Invalid/);
});
