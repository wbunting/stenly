# When another app blocks the dictation shortcut

On macOS, Secure Keyboard Entry can prevent global keyboard listeners from
receiving your dictation shortcut even when Stenly has Accessibility access.
Password fields and some terminal modes enable this protection. Restarting
Stenly or changing the shortcut does not release another app's protection.

Stenly checks independently of its keyboard listener, about once per second:

- The menu bar shows **Dictation shortcut blocked** while protection is on.
- Stenly's menu and **Settings → Dictation** identify the app when macOS exposes
  an owner. Otherwise, Stenly reports Secure Keyboard Entry without guessing.
- After five seconds, one non-activating notice explains the block. Brief
  password-field visits stay quiet; repeated samples do not repeat the notice.
  Meeting prompts take priority over this notice.
- **Show blocking app** brings that app forward only when you choose it. It
  rechecks the owner first and never closes the app or dismisses its prompts.
- Once protection ends, the warning clears automatically and Stenly forgets
  any held shortcut whose release might have been hidden by the lock.

Leave the password field or dismiss the password prompt in the named app. If
protection remains stuck, save your work and restart that app yourself. Do not
turn off password protections or reset privacy permissions to work around it.
Stenly never disables Secure Keyboard Entry, reads password-field contents, or
records keyboard input for these diagnostics. Never dictate secrets.

## Implementation and verification

The public Carbon `IsSecureEventInputEnabled` query is authoritative for the
lock. Owner attribution uses the current on-console session's
`kCGSSessionSecureInputPID` in the I/O Registry, plus `NSRunningApplication`.
That registry property is undocumented: missing, malformed, ambiguous, or
unresolvable attribution falls back to an unknown app rather than failing
keyboard registration. Linux and other platforms return no Secure Input block.

Portable regression tests cover short visits, sustained blocks, single notices,
owner changes, recovery, and unknown owners:

```sh
cargo test -p stenly-core keyboard_guard
cargo test -p stenly-platform
```

For a macOS smoke test, run a separately built Stenly with synthetic config and
isolated `STENLY_CONFIG` / `STENLY_DATA_DIR`; disable sounds, AI cleanup and model
preload, and set `STENLY_PASTE_ONLY_INTO` to a disposable test app. Use a separate
fixture app that activates itself, calls `EnableSecureEventInput`, and balances
it with `DisableSecureEventInput` on release/exit (also add a safety timeout).
Verify the named app, menu status, delayed notice and automatic clearing with
the same Stenly process. Do not type into real password fields or change the
installed daily-use app to perform this test.
