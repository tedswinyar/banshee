"""dmgbuild settings for the styled Banshee DMG. Paths come from the environment
so scripts/build-dmg.sh remains the single source of truth. dmgbuild writes the
.DS_Store directly — no Finder, reproducible, CI-safe."""
import os

_app = os.environ["DMG_APP"]

files = [_app]
symlinks = {"Applications": "/Applications"}
icon_locations = {
    os.path.basename(_app): (148, 170),
    "Applications": (512, 170),
}
# License notices ride along (banshee-4z1), positioned below the visible window
# so they are present/accessible without cluttering the drag-to-install view.
for _var, _pos in (
    ("DMG_NOTICES", (160, 560)),
    ("DMG_LICENSE", (330, 560)),
    ("DMG_SPARKLE_LICENSE", (500, 560)),
):
    _p = os.environ.get(_var, "")
    if _p and os.path.exists(_p):
        files.append(_p)
        icon_locations[os.path.basename(_p)] = _pos

background = os.environ["DMG_BG"]
window_rect = ((200, 120), (660, 400))
icon_size = 128
text_size = 13
