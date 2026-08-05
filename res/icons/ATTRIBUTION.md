# Bundled icon attribution

Every icon the app draws is compiled into the binary (`include_bytes!`), so icon
resolution is platform-independent: nothing depends on a system freedesktop
theme or on the subset libcosmic embeds, and the UI looks the same on Linux,
macOS and Windows. `src/widgets/icons.rs` is the one resolver.

| Files | Upstream | License |
| --- | --- | --- |
| `lucide/*.svg` | [lucide-icons/lucide](https://github.com/lucide-icons/lucide) | ISC, plus MIT on 18 of them ([`lucide/LICENSE`](lucide/LICENSE)) |
| `brands/*.svg` | each provider's own published mark (source URL in every file) | see below |
| `dev.frosthaven.CosmicCaptureKit*.svg`, `cosmic-capture-kit*.windows.ico` | original project artwork by [Ashley Ball](https://ashleythedesigner.com/) | project license |

## Lucide (the UI set, DRAGON-324)

85 glyphs, chosen by what each control DOES rather than by an old freedesktop
name. The SVGs stroke with `currentColor`, so they are marked symbolic and
tinted with the active foreground or accent color like a native symbolic icon.

**This attribution is not optional, and it cannot be dropped while the SVGs are
here.** ISC grants use "provided that the above copyright notice and this
permission notice appear in all copies", and we ship 85 copies: in the repo, and
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

## Not in this directory

The macOS menu-bar tray uses native SF Symbols rather than a bundled file (see
`src/platform/mac/tray.rs`).
