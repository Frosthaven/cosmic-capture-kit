# Bundled icon attribution

Cosmic Capture Kit compiles its entire UI icon set into the binary
(`include_bytes!`) from [Lucide](https://lucide.dev) (DRAGON-324). Bundling every
glyph makes icon resolution platform-independent: the app no longer depends on a
system freedesktop icon theme or on the subset libcosmic embeds, so icons render
identically on Linux, macOS, and Windows. The resolver
(`src/widgets/icons.rs`) maps each app icon name to the Lucide glyph that best fits
what the control does.

| Files | Upstream | License |
| --- | --- | --- |
| `lucide/*.svg` | [lucide-icons/lucide](https://github.com/lucide-icons/lucide) | ISC / MIT |

Lucide is distributed under the ISC License (a permissive MIT-equivalent). The SVGs
stroke with `currentColor`, so they are marked symbolic and tinted with the active
foreground/accent color like a native symbolic icon.

Not icons: the app/window brand art (`dev.frosthaven.CosmicCaptureKit*.svg`,
`cosmic-capture-kit*.windows.ico`) is original project artwork, and the macOS
menu-bar tray uses native SF Symbols (see `src/platform/mac/tray.rs`).
