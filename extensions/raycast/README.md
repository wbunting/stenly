# Stenly for Raycast and Vicinae

Use this when a dictation went to the wrong app, or you want to paste it again.

## Commands

| Command | Description |
| --- | --- |
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
