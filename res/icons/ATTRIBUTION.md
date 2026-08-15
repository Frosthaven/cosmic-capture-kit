# Bundled icon attribution

Every icon the app draws is compiled into the binary (`include_bytes!`), so icon
resolution is platform-independent: nothing depends on a system freedesktop
theme or on the subset libcosmic embeds, and the UI looks the same on Linux,
macOS and Windows. `src/widgets/icons.rs` is the one resolver.

| Files | Upstream | License |
| --- | --- | --- |
| `lucide/*.svg` | [lucide-icons/lucide](https://github.com/lucide-icons/lucide) | ISC, plus MIT on 18 of them ([`lucide/LICENSE`](lucide/LICENSE)) |
| `brands/*.svg` | each provider's own published mark (source URL in every file) | see below |
| `dev.thedragon.CosmicCaptureKit*.svg`, `cosmic-capture-kit*.windows.ico` | original project artwork by [Ashley Ball](https://ashleythedesigner.com/) | project license |

## Lucide (the UI set, DRAGON-324)

106 glyphs, chosen by what each control DOES rather than by an old freedesktop
name. (The count is the number of `.svg` files in `lucide/`; it read 90 against
93 files until DRAGON-614, 96 against 99 until DRAGON-659, 100 against 103
until DRAGON-680, and 103 against 106 until DRAGON-682, so re-count rather than
trusting the line.)

**Three of the 106 are DERIVED rather than copied** (DRAGON-682), and the
distinction is worth keeping honest. `panel-right-open.svg` and
`panel-right-close.svg` are the exact mirror of the vendored `panel-left-*`
pair, which is a deterministic reflection of upstream geometry.
`circle-dot-dashed.svg` is GENERATED: eight equal arcs of a radius-10 circle
around a centre dot, in lucide's own 24x24 viewBox and stroke conventions, drawn
to the design the owner linked rather than copied byte for byte from upstream.
All three carry the same license as the set they sit in and are stylistically
identical to it; none is passed off as an upstream file. The SVGs
stroke with `currentColor`, so they are marked symbolic and tinted with the
active foreground or accent color like a native symbolic icon.
`timer.svg` (DRAGON-574, the tray menu's Countdown Timer entry), `trash.svg`
(the tray menu's Cancel & Delete Recording control), `pipette.svg`
(DRAGON-582, the colour picker tool, the glyph the owner named), `globe.svg`
(DRAGON-588, the Keyboard Shortcuts page's Global tab), `binary.svg` +
`file-archive.svg` (DRAGON-591, the About page's release-kind line, one glyph
per package kind), `apple.svg` + `grid-2x2.svg` (DRAGON-614, the same line's
macOS and Windows kinds, the glyphs the owner named), `key.svg` (DRAGON-412,
the macOS tray menu's Manage Permissions entry, the glyph the owner named),
`loader-circle.svg` (DRAGON-659, the record chip's warming spinner, the glyph
the owner named) and `palette.svg` + `swatch-book.svg` (DRAGON-680, the tray
menu's Colors submenu and its Palette Viewer entry, the glyphs the owner named)
are
verbatim copies of the official Lucide `timer`, `trash`, `pipette`, `globe`,
`binary`, `file-archive`, `apple`, `grid-2x2`, `key`, `loader-circle`,
`palette` and `swatch-book` icons,
fetched from
upstream and already in the set's house format (24-unit viewBox, `currentColor`
strokes, stroke-width 2).

`apple.svg` is Lucide's own generic apple-fruit drawing, not Apple Inc's logo
and not a brand mark. That is why it sits here with the tinted symbolic set
rather than in `brands/`: nothing about it is a trademark, and it is used to
name the macOS build the way the `package` box names the Flatpak one.

**This attribution is not optional, and it cannot be dropped while the SVGs are
here.** ISC grants use "provided that the above copyright notice and this
permission notice appear in all copies", and we ship 103 copies: in the repo, and
compiled into every binary. Eighteen of them (`check`, `chevron-down`, `circle`,
`search`, `x`, `zoom-in` and twelve more) are derived from Feather and carry
Cole Bemis's MIT notice on top, which asks the same thing.

So the notice travels with the files rather than with a link: upstream's own
license text is vendored verbatim at [`lucide/LICENSE`](lucide/LICENSE), covering
both. Re-copy it from upstream if the icon set is ever refreshed, and delete it
only in the same commit that deletes the SVGs.

## The provider marks (the cloud accounts, DRAGON-482)

Lucide has no cloud-provider marks, so the cloud-accounts feature uses each
provider's OWN official mark: a sanitized copy of the vector that brand
publishes, with the source URL recorded in a comment at the top of every file.
Nothing is restyled. The single change made to any of them is centring the mark
in a square 24x24 viewBox; the geometry and the colors are the brand's.

They are the one exception to the tint rule above, and that is the point: a
brand mark is recognised BY its colors. Using an unmodified mark to say which
service an account connects to is the case every one of these brands' guidelines
sanctions. Keep it that way: do not recolor one, do not restyle one, and do not
put one on anything but the provider it belongs to. Each remains the trademark
of its owner, and none of them implies endorsement.

One mark carries an extra record: `proton-drive.svg` is Proton's official mark,
restored at owner direction. Proton does not sanction this integration (no
third-party API; it runs through Proton's MIT-licensed CLI), so the sanction
sentence above does not cover it. DRAGON-566 shipped a neutral locked-cloud
glyph for that reason; the owner then reviewed Proton's third-party branding
terms (quoted in DRAGON-566) and accepted the risk on 2026-08-07, so the
neutral glyph and the app's disclosure line were removed at owner direction,
not by oversight. The file's own comment carries the same record.

## Not in this directory

The macOS menu-bar tray uses native SF Symbols rather than a bundled file (see
`src/platform/mac/tray.rs`).
