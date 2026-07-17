#!/usr/bin/env bash
# Mirror a published release from the PRIVATE working repo to the PUBLIC repo,
# and stamp the update channel (DRAGON-226). Releases are user-facing: the
# public repo is where the Linux "Get Update" button and the About links land
# (RELEASES_URL), and its latest-release `update.json` asset IS the in-app
# update channel (`update::DEFAULT_MANIFEST_URL` polls
# releases/latest/download/update.json). The whole build/publish pipeline stays
# on the private repo with its secrets. Run after clicking Publish on the
# private repo's draft:
#
#   scripts/mirror-release.sh v0.12.0
#
# Copies the tag's assets and body verbatim — release bodies are human-edited
# at publish time (the DRAGON-176 flow) and must never reference private-repo
# commits/PRs or external GitHub repos — then generates `update.json` (the same
# shape publish-update.yml ships to the LEGACY Pages channel) pointing at the
# public dmg asset, and attaches it. The legacy Pages push stays alive only
# until the installed base is past 0.12.0 (see RELEASING.md's phase-out note).
set -euo pipefail

TAG="${1:?usage: mirror-release.sh <tag>}"
PRIVATE="Frosthaven/cosmic-capture-kit-private"
PUBLIC="Frosthaven/cosmic-capture-kit"
VERSION="${TAG#v}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"

gh release download "$TAG" -R "$PRIVATE" --clobber
gh release view "$TAG" -R "$PRIVATE" --json body --jq .body > notes.md

if grep -qE "cosmic-capture-kit-private|Co-Authored-By" notes.md; then
    echo "ERROR: release notes reference the private repo (or carry attribution) — fix the body first" >&2
    exit 1
fi

# The update manifest (same fields the legacy Pages channel serves), pointing
# at the PUBLIC release's own dmg asset. Skipped with a warning if this release
# carries no dmg (a Linux-only release still updates the channel manifest-wise
# only when a dmg exists — the manifest is the MAC one-click channel).
DMG="$(ls CosmicCaptureKit-*.dmg 2>/dev/null | head -1 || true)"
if [[ -n "$DMG" ]]; then
    NOTES="$(cat notes.md)" VERSION="$VERSION" \
    PUBLISHED="$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    URL="https://github.com/$PUBLIC/releases/download/$TAG/$DMG" \
    SHA="$(sha256sum "$DMG" | awk '{print $1}')" \
    SIZE="$(stat -c%s "$DMG")" \
    python3 - <<'PY'
import json, os
m = {
    "version": os.environ["VERSION"],
    "notes": os.environ.get("NOTES", ""),
    "published": os.environ["PUBLISHED"],
    "platforms": {
        "macos": {
            "url": os.environ["URL"],
            "sha256": os.environ["SHA"],
            "size": int(os.environ["SIZE"]),
        }
    },
}
with open("update.json", "w") as f:
    json.dump(m, f, indent=2)
    f.write("\n")
PY
else
    echo "WARNING: no dmg asset on $TAG — mirroring without update.json" >&2
fi

assets=$(ls | grep -v "^notes.md$")
# shellcheck disable=SC2086  # asset names are our own version-stamped files
gh release create "$TAG" -R "$PUBLIC" --title "$TAG" --notes-file notes.md $assets
echo "mirrored $TAG -> https://github.com/$PUBLIC/releases/tag/$TAG"
