# dmgbuild settings for the Stenly installer disk image (MIT-licensed dmgbuild;
# see scripts/requirements-dmg.txt). Evaluated by dmgbuild with these -D defines:
#   app=<path to the signed .app>   background=<path to background PNG>
#   icon=<path to volume .icns>
# dmgbuild writes the Finder window layout (.DS_Store) itself, so no GUI session
# or AppleScript is needed.
import os

app = defines["app"]  # noqa: F821 - provided by dmgbuild
app_name = os.path.basename(app)

format = "UDZO"
filesystem = "HFS+"

files = [(app, app_name)]
symlinks = {"Applications": "/Applications"}
icon = defines.get("icon")  # noqa: F821
background = defines["background"]  # noqa: F821

# The background is 660x400 (the @2x file next to it is picked up automatically
# for Retina displays). The window rect includes the 32pt title bar, so add it to
# show the whole background.
window_rect = ((200, 120), (660, 432))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False
arrange_by = None
icon_size = 128
text_size = 13
label_pos = "bottom"

# App on the left, Applications on the right, centred on the arrow.
icon_locations = {app_name: (170, 190), "Applications": (490, 190)}
