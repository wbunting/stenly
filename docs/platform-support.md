# Platform support and speaker identity

This separates implemented code from runtime evidence. A successful build or a parser unit test is not proof that a real meeting app exposes the expected Accessibility tree. Verify the release on the intended OS, app version, browser, layout and audio hardware.

## Desktop platforms

| Platform | Implementation | Evidence / limitations |
| --- | --- | --- |
| macOS | Native desktop, audio capture, dictation, Accessibility meeting drivers | Primary tested platform. Requires appropriate microphone, system-audio and Accessibility permissions. A release still needs signed/notarized installer and clean-install verification. |
| Linux | PipeWire/PulseAudio capture, desktop and dictation backends | Not a claim of all-distro support. Compositor, global-shortcut permissions and audio routing vary. No meeting-app Accessibility speaker-name backend (`meeting_ax: None`). Test actual hardware/session before relying on it. |
| Windows | Fallback code paths | No supported system-audio meeting recording release. Do not advertise Windows support. |

## Speaker identity

The source of record is `crates/stenly-core/src/speakers/drivers/mod.rs`. Its historical verification means an observation from a live call, **not a new test of the current release**.

| App | Roster | Active speaker | Self | Other |
| --- | --- | --- | --- | --- |
| Teams | Historical live-tree observation | Historical live-tree observation | Not verified | Topic from window title |
| Zoom | Historical gallery observation | Label/tile implementation; unverified | Historical live-tree observation | Host tag unverified |
| Google Meet (Chrome / Arc) | **Unverified** | **Unverified** | **Unverified** | Requires consenting live-call tests in both browsers |
| Other apps | No app-specific guarantee | Generic screen-reader labels, when exposed | No guarantee | May be empty |

Remembered voiceprints and invite matching can help name a speaker, but are not a substitute for measuring attribution accuracy. User-renamed names do not prove automatic attribution. Linux has no app-specific name evidence.

## Safe Google Meet verification protocol

1. Arrange a dedicated test call with at least two consenting adults. Explain audio recording, transcription, local AX inspection and whether sanitized evidence will be retained. Use invented display names; no personal meeting titles, email addresses or unrelated tabs. Do not join a real user meeting.
2. The human creates/joins the call in Chrome, then repeats in Arc. The human handles browser login, permissions and meeting/chat clicks; the automation must not type or click into the user's apps. Use headphones; any generated speech goes only to BlackHole, never speakers.
3. Record app, browser and OS versions, date, permission state and layout. With the participants' permission, run `stenly-cli speakers dump <private-path>` and `stenly-cli speakers watch --help` to confirm current capture options before collecting time-series observations. AX dumps can contain unrelated window data: keep them private and sanitize a reviewed minimal fixture before committing it.
4. Check the roster with all participants visible and in the people panel; alternate each remote speaker and the local recorder for several non-overlapping turns. Include silence, mute/unmute, off-screen tiles, layout changes, a participant leaving/rejoining, duplicate display names and overlap. Record expected and observed values at each timestamp.
5. Verify `me` is the local participant and never a remote one. Verify live active-speaker transitions, then check the resulting transcript's attribution against the known turns. Keep the roster, active-speaker, self and final transcript outcomes separate. Repeat in Arc; a Chrome pass is not an Arc pass.
6. Only mark a capability verified with the sanitized evidence, app versions and observed limits. Wrong or missing values mean fail/partial, not pass. Leave the matrix unverified until this happens.

### Minimal extension fallback (design only)

If AX fails reproducibly, a follow-up can use a small opt-in extension restricted to `https://meet.google.com/*`. With an explicit per-call user action, read visible participant names, the self marker and speaking state. Pass only timestamped observations to a localhost/native-messaging bridge after pairing. Do not capture audio, chat contents, authentication tokens or unrelated tabs; do not post notices automatically. Version the observation schema, reject stale observations and retain manual naming when it is uncertain. Test browser permissions, reconnects, duplicate names and meeting navigation. This is a follow-up proposal, **not a shipped extension or verified solution**.
