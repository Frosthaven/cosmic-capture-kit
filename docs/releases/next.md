<!-- Draft for the next release. Format copied from v0.29.0's cck-notes span:
     platform-grouped, bold lead phrase, plain prose, wrapped at ~80.
     Paste the section below into the cck-notes markers, never the whole body. -->

**All Platforms:**

- **New color picker tool**: Select colors from your screen pixels, with
  history, zoomable magnification, and arrow key movements for pixel-perfect
  grabs!
- **Enhanced system tray menu**, putting the most common capture options at your
  fingertips.
- **The dimmed overlay fades in** where available for an *aesthetic* experience.
- **Settings window improvements**: many adjustments to the settings window,
  including showing paths and versions where possible (health screen, etc) and
  build type in the about section.

**macOS:**

- **Improved audio recording** for better microphone processing.

**Linux:**

- **The tray menu stays out of your screenshots**: taking a capture from the
  tray no longer catches the dropdown that launched it.
- **Flatpak builds**: We can now generate a flatpak build for future release.

<!-- ─────────────────────────────────────────────────────────────────────────
NOTES FOR YOU, not part of the release body.

Assumes in-flight work lands. Cut the line if it does not:
  - "remembers it" (zoom persistence)
  - the OCR shortcut change, if you want it mentioned at all

Deliberately left out:
  - macOS and Windows package badges. Nothing a user acts on.
  - The self-capture feature, added and removed within the same batch.
  - Internals: the iced fork fix, config path work, doc and test corrections.
  - Flatpak autostart. Still cannot work on COSMIC; no Background portal exists.

No macOS or Windows section this time. Nothing in this batch is visible there
beyond the shared items, and v0.29.0 only carried those sections because it had
real platform-specific changes.
────────────────────────────────────────────────────────────────────────── -->
