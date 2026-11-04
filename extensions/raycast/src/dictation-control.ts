import { execFile } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

/** Invoke Stenly's own menu, without synthetic keys or stealing the paste target. */
export function toggleScript(bundleId = "ai.stenly.app"): string {
  if (!/^[a-zA-Z0-9.-]+$/.test(bundleId)) throw new Error("Invalid application identifier");
  return `tell application "System Events"
  set targets to application processes whose bundle identifier is "${bundleId}"
  if (count of targets) is not 1 then error "Open Stenly first, then run Toggle Dictation again."
  set stenlyProcess to item 1 of targets
  tell stenlyProcess
    repeat with statusBar in menu bars
      repeat with statusItem in menu bar items of statusBar
        if description of statusItem is "status menu" then
          try
            perform action "AXPress" of statusItem
            repeat 20 times
              if exists menu 1 of statusItem then exit repeat
              delay 0.05
            end repeat
            if exists menu item "Start dictation" of menu 1 of statusItem then
              click menu item "Start dictation" of menu 1 of statusItem
              return
            end if
            perform action "AXCancel" of statusItem
          on error messageText number errorNumber
            try
              perform action "AXCancel" of statusItem
            end try
            error messageText number errorNumber
          end try
        end if
      end repeat
    end repeat
  end tell
end tell
error "Stenly's dictation menu is unavailable. Open the app and try again."`;
}

export async function toggleDictation(): Promise<void> {
  if (process.platform !== "darwin") {
    throw new Error("Manual dictation control currently requires the macOS Stenly app.");
  }
  try {
    await execFileAsync("/usr/bin/osascript", ["-e", toggleScript()], { timeout: 8000 });
  } catch {
    // stderr can include private, dynamically generated menu titles.
    throw new Error("Open Stenly and allow your launcher in System Settings → Privacy & Security → Accessibility (and Automation → System Events), then try again.");
  }
}
