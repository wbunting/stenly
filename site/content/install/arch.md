+++
title = "Install Stenly on Arch Linux"
template = "install.html"
+++

# Arch Linux

Follow the [Linux installation guide](/install/linux/), then install the Arch dependencies:

```sh
sudo pacman -Syu
sudo pacman -S pipewire pipewire-pulse pipewire-alsa wireplumber \
  alsa-lib wayland libxkbcommon libxkbcommon-x11 vulkan-icd-loader fontconfig dbus \
  wl-clipboard wtype xclip xdotool
```

If your compositor has not started them, enable PipeWire and WirePlumber with:

```sh
systemctl --user --now enable wireplumber pipewire pipewire-pulse
```

Add yourself to `input`, log out and back in, then launch `~/.local/opt/stenly/bin/stenly`.
