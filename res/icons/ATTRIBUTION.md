# Bundled icon attribution

Cosmic Capture Kit ships a small set of SVG icons compiled into the binary
(`include_bytes!`) so they render on platforms where the system icon theme does
not provide them — notably macOS, where `cosmic::widget::icon::from_name` resolves
only against libcosmic's embedded `cosmic-icons` subset (no freedesktop theme dirs
exist). Every other icon the app uses is a name that libcosmic already embeds.

| File | Icon name | Upstream | License |
| --- | --- | --- | --- |
| `cosmic/screenshot-selection-symbolic.svg` | `screenshot-selection-symbolic` | [pop-os/xdg-desktop-portal-cosmic](https://github.com/pop-os/xdg-desktop-portal-cosmic) (`data/icons/scalable/actions/`) | CC-BY-SA-4.0 |
| `cosmic/screenshot-window-symbolic.svg` | `screenshot-window-symbolic` | [pop-os/xdg-desktop-portal-cosmic](https://github.com/pop-os/xdg-desktop-portal-cosmic) (`data/icons/scalable/actions/`) | CC-BY-SA-4.0 |
| `cosmic/screenshot-screen-symbolic.svg` | `screenshot-screen-symbolic` | [pop-os/xdg-desktop-portal-cosmic](https://github.com/pop-os/xdg-desktop-portal-cosmic) (`data/icons/scalable/actions/`) | CC-BY-SA-4.0 |
| `local/object-move-symbolic.svg` | `object-move-symbolic` | Original work for this project (the preview pan/grab tool). Named for the freedesktop `object-move-symbolic` slot the Linux system theme fills; that name is NOT in libcosmic's embedded bundle, and — as of this writing — no longer ships in the current GNOME `adwaita-icon-theme`, `cosmic-icons`, or Yaru, so a project-owned symbolic glyph (a 4-way move arrow, `currentColor`) is bundled instead. | Same as this project (GPL-3.0) |

The `cosmic-icons` upstream (which libcosmic embeds) is CC-BY-SA-4.0; the screenshot
trio above are vendored from the COSMIC portal package that ships them on Linux, so
the app looks identical there and on macOS.
