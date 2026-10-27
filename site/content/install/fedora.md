+++
title = "Install Stenly on Fedora"
template = "install.html"
+++

# Fedora

Follow the [Linux installation guide](/install/linux/), then install the Fedora dependencies:

```sh
sudo dnf install pipewire pipewire-pulseaudio pipewire-alsa wireplumber \
  alsa-lib libwayland-client libxkbcommon libxkbcommon-x11 vulkan-loader fontconfig dbus \
  wl-clipboard wtype xclip xdotool
```

Fedora Workstation normally already runs PipeWire. Add yourself to `input`, log out and back in, then launch `~/.local/opt/stenly/bin/stenly`.
