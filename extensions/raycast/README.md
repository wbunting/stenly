# Stenly for Raycast and Vicinae

Use this when a dictation went to the wrong app, or you want to paste it again.

## Commands

| Command | Description |
| --- | --- |
| **Toggle Dictation** | Starts or stops dictation in the running macOS Stenly app, without using its global hotkey. Run again to finish. |
| **Recent Dictations** | Lists Stenly's dictation history, newest first. You can search the text and app name. `↵` pastes into the frontmost app and `⌘↵` copies (you can swap these in preferences), `⌘⇧C` copies the raw transcript before AI clean-up, `⌘D` shows or hides the detail pane, and `⌘R` reloads. |

The extension reads `<data dir>/dictation/history.jsonl` directly, so Stenly doesn't need to be running. The data dir is `~/Library/Application Support/stenly` on macOS and `$XDG_DATA_HOME/stenly` on Linux. You can override it with `STENLY_DATA_DIR` or the **Stenly Data Directory** preference. History length follows Stenly's `dictation.history_limit`.

## Build

```sh
npm install
npm run build           # Raycast (ray build); `npm run dev` for hot reload
npm run vicinae:build   # Vicinae: installs into ~/.local/share/vicinae/extensions/stenly
npm test                # history parser tests (synthetic data)
```

Vicinae runs the extension through its Raycast compatibility layer, so the same source works in both. Preferences are stored separately in each launcher.

## Manual dictation and hotkey troubleshooting

Toggle Dictation closes the launcher and invokes Stenly's status-menu action; it
never synthesizes a keystroke or brings Stenly's main window to the front. It
uses the same recording/paste pipeline as the hotkey. Stenly must already be
running. During transcription, wait for Stenly to finish before starting again.
The manual command currently supports macOS; Recent Dictations also supports Linux.

Allow Raycast or Vicinae under **System Settings → Privacy & Security →
Accessibility** and, if requested, **Automation → System Events**. The command
reports a failure if the app or menu cannot be reached; it does not launch a
second Stenly process or change your shortcut.

If a hotkey stops working despite Stenly having Accessibility access, check for
**Secure Keyboard Entry** in another app. Browsers enable it for password fields
and some terminals expose it as a menu option. Leave the password field/prompt
(or turn off a terminal's explicit Secure Keyboard Entry mode when appropriate).
Do not disable password protections or reset macOS privacy permissions just to
make the hotkey work. Manual menu control does not depend on keyboard event taps;
pasting may still be prevented by a secure password field. Never dictate secrets.
