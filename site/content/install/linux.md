+++
title = "Install Stenly on Linux"
template = "install.html"
+++

# Install Stenly on Linux

Stenly is designed to run locally: it records your microphone and the audio playing on **your** computer, then writes files to your machine. Public Linux installers are not available yet; release packaging and publication are still pending. Once an installer is published, this guide will include the verified download and setup commands.

## One-time desktop setup

- **PipeWire:** Stenly records system audio through PipeWire/PulseAudio compatibility. Use a normal PipeWire desktop session (WirePlumber is the usual session manager) and confirm `pactl info` works before recording.
- **Dictation shortcut:** global shortcuts read `/dev/input`. Add your user to the `input` group, then **log out and back in** before using dictation:

  ```sh
  sudo usermod -aG input "$USER"
  ```

  Membership of `input` grants access to raw keyboard events. Use it only on a personal desktop you trust.
- **Pasting dictated text:** on Wayland install `wl-clipboard` and `wtype`; on X11 install `xclip` (or `xsel`) and `xdotool`. Stenly uses the one appropriate to your session.
- **Tray:** KDE, Xfce, Cinnamon and most wlroots compositors show Stenly's StatusNotifier tray icon directly. GNOME does not ship tray support: install and enable an AppIndicator/KStatusNotifierItem extension, or use the app window.

For package-manager commands specific to your distribution, see [Ubuntu / Debian](/install/ubuntu/), [Fedora](/install/fedora/), or [Arch Linux](/install/arch/).

## Launching and troubleshooting

Launch with `stenly`; Stenly creates its config at `~/.config/stenly/config.toml`. Start `stenly-cli config` to see the active locations. On Wayland, launch from the graphical session so the app inherits `WAYLAND_DISPLAY` and the user D-Bus session; do not run the app itself with `sudo`.

If system audio is missing, first check `pactl info`, then inspect the chosen sink in your desktop's sound settings. If dictation does not paste, make sure the Wayland/X11 helper for your session is installed and that you logged out and in after joining `input`.
