+++
title = "Install Stenly on Ubuntu / Debian"
template = "install.html"
+++

# Ubuntu / Debian

Follow the [Linux installation guide](/install/linux/), then install the Ubuntu/Debian dependencies:

```sh
sudo apt update
sudo apt install pipewire pipewire-pulse wireplumber pulseaudio-utils \
  libasound2 libwayland-client0 libxkbcommon0 libxkbcommon-x11-0 libvulkan1 libfontconfig1 libdbus-1-3 \
  wl-clipboard wtype xclip xdotool
```

Confirm your session exposes PipeWire through `pactl info`, then add yourself to `input`, log out and back in, and launch `~/.local/opt/stenly/bin/stenly`.
