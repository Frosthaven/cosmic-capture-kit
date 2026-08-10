#!/usr/bin/env bash
# linear-ticket-chart.sh: render the Cosmic Capture Kit AI-disclosure chart from Linear.
#
# Produces one SVG with two panels:
#   left:  share of all COMPLETED project tickets that are Bug / Feature / Task
#   right: the same split for tickets completed in the last 30 days
# Unlabeled tickets are still tallied as "unknown" in the snapshot JSON, but
# the chart does not draw them (the project labels everything).
# Dark background, neon bars, colors taken from the Linear labels themselves.
# Header carries the app logo and an AI-collaboration disclosure line; the
# footer carries the QA note and the Linear brand mark next to the source
# credit.
#
# Intended for the public-mirror deployment jobs: run it before the docs/repo
# mirror step so the exported tree carries a fresh chart. It needs the
# LINEAR_API_KEY secret in the job environment. Nothing else is written.
#
# Usage:
#   LINEAR_API_KEY=lin_api_... scripts/linear-ticket-chart.sh [--out PATH]
#   scripts/linear-ticket-chart.sh --from-json snapshot.json --out PATH
#
# Options:
#   --out PATH        where the SVG lands (default: docs/ticket-mix.svg)
#   --project NAME    Linear project name (default: Cosmic Capture Kit)
#   --logo PATH       app logo SVG to inline (default: res/icons/dev.frosthaven.CosmicCaptureKit.svg)
#   --from-json PATH  skip the API and render from a saved snapshot
#   --dump-json PATH  also save the fetched snapshot (for --from-json later)
#
# Requirements: bash, curl, jq, base64, GNU date, and rsvg-convert (librsvg)
# for the PNG the README embeds. No Rust, no node, no python.
# The snapshot JSON shape (also what --dump-json writes):
#   { "project": "...", "colors": {"bug": "#..", "feature": "#..", "task": "#.."},
#     "lifetime": {"bug": N, "feature": N, "task": N, "unknown": N, "total": N},
#     "last30":   {"bug": N, "feature": N, "task": N, "unknown": N, "total": N} }

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
API_URL="https://api.linear.app/graphql"
PROJECT_NAME="Cosmic Capture Kit"
OUT_PATH="$REPO_ROOT/docs/ticket-mix.svg"
LOGO_PATH="$REPO_ROOT/res/icons/dev.thedragon.CosmicCaptureKit.svg"
FROM_JSON=""
DUMP_JSON=""
# Linear's gray for "no label"; the three real colors come from the labels.
UNKNOWN_COLOR="#8A8F98"

# Footer brand mark: Linear's 24x24 single-path glyph from Simple Icons (CC0),
# embedded so the render needs no network beyond the Linear API itself.
LINEAR_BADGE_COLOR="#5E6AD2"
LINEAR_BADGE_D='M2.886 4.18A11.982 11.982 0 0 1 11.99 0C18.624 0 24 5.376 24 12.009c0 3.64-1.62 6.903-4.18 9.105L2.887 4.18ZM1.817 5.626l16.556 16.556c-.524.33-1.075.62-1.65.866L.951 7.277c.247-.575.537-1.126.866-1.65ZM.322 9.163l14.515 14.515c-.71.172-1.443.282-2.195.322L0 11.358a12 12 0 0 1 .322-2.195Zm-.17 4.862 9.823 9.824a12.02 12.02 0 0 1-9.824-9.824Z'

while [ $# -gt 0 ]; do
    case "$1" in
        --out)       OUT_PATH="$2"; shift 2 ;;
        --project)   PROJECT_NAME="$2"; shift 2 ;;
        --logo)      LOGO_PATH="$2"; shift 2 ;;
        --from-json) FROM_JSON="$2"; shift 2 ;;
        --dump-json) DUMP_JSON="$2"; shift 2 ;;
        -h|--help)   sed -n '2,30p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

api() {
    # $1 = request body. Fails loudly on transport or GraphQL errors.
    local resp
    resp="$(curl -sS --fail-with-body -X POST "$API_URL" \
        -H "Content-Type: application/json" \
        -H "Authorization: ${LINEAR_API_KEY}" \
        --data "$1")" || { echo "Linear API request failed: $resp" >&2; return 1; }
    if echo "$resp" | jq -e '.errors' >/dev/null 2>&1; then
        echo "Linear API returned errors: $(echo "$resp" | jq -c '.errors')" >&2
        return 1
    fi
    echo "$resp"
}

fetch_snapshot() {
    [ -n "${LINEAR_API_KEY:-}" ] || {
        echo "LINEAR_API_KEY is not set and no --from-json was given" >&2
        exit 1
    }

    # Resolve the project id from its name.
    local body project_id
    body="$(jq -n --arg name "$PROJECT_NAME" '{
        query: "query($name: String!) { projects(filter: { name: { eqIgnoreCase: $name } }) { nodes { id name } } }",
        variables: { name: $name }
    }')"
    project_id="$(api "$body" | jq -r '.data.projects.nodes[0].id // empty')"
    [ -n "$project_id" ] || { echo "Linear project not found: $PROJECT_NAME" >&2; exit 1; }

    # Page through every issue in the project, archived ones included.
    # Lifetime stats need the archived tail: Linear auto-archives old done issues.
    local cursor="" page after_arg
    : > "$TMP_DIR/issues.jsonl"
    while :; do
        if [ -n "$cursor" ]; then after_arg="$cursor"; else after_arg=""; fi
        body="$(jq -n --arg pid "$project_id" --arg after "$after_arg" '{
            query: "query($pid: ID!, $after: String) { issues(filter: { project: { id: { eq: $pid } } }, first: 250, after: $after, includeArchived: true) { pageInfo { hasNextPage endCursor } nodes { createdAt completedAt labels { nodes { name color } } } } }",
            variables: { pid: $pid, after: (if $after == "" then null else $after end) }
        }')"
        page="$(api "$body")"
        echo "$page" | jq -c '.data.issues.nodes[]' >> "$TMP_DIR/issues.jsonl"
        if [ "$(echo "$page" | jq -r '.data.issues.pageInfo.hasNextPage')" != "true" ]; then
            break
        fi
        cursor="$(echo "$page" | jq -r '.data.issues.pageInfo.endCursor')"
    done

    local cutoff
    cutoff="$(date -u -d "30 days ago" +%Y-%m-%dT%H:%M:%SZ)"

    jq -s --arg project "$PROJECT_NAME" --arg cutoff "$cutoff" '
        def category:
            [(.labels.nodes // [])[].name | ascii_downcase] as $names
            | if   ($names | index("bug"))     then "bug"
              elif ($names | index("feature")) then "feature"
              elif ($names | index("task"))    then "task"
              else "unknown" end;
        def tally:
            map(category) | {
                bug:     (map(select(. == "bug"))     | length),
                feature: (map(select(. == "feature")) | length),
                task:    (map(select(. == "task"))    | length),
                unknown: (map(select(. == "unknown")) | length),
                total:   length
            };
        def label_color($want):
            [.[] | (.labels.nodes // [])[]
             | select((.name | ascii_downcase) == $want) | .color]
            | first // empty;
        {
            project: $project,
            colors: {
                bug:     ((label_color("bug"))     // "#EB5757"),
                feature: ((label_color("feature")) // "#BB87FC"),
                task:    ((label_color("task"))    // "#5E6AD2")
            },
            lifetime: (map(select(.completedAt != null)) | tally),
            last30: (map(select((.completedAt != null) and (.completedAt >= $cutoff))) | tally)
        }
    ' "$TMP_DIR/issues.jsonl"
}

if [ -n "$FROM_JSON" ]; then
    SNAPSHOT="$(cat "$FROM_JSON")"
else
    SNAPSHOT="$(fetch_snapshot)"
fi
if [ -n "$DUMP_JSON" ]; then
    echo "$SNAPSHOT" | jq . > "$DUMP_JSON"
fi

# The logo is inlined as a data URI so the SVG stays self-contained. If the
# app id ever renames again, fall back to any dev.*.CosmicCaptureKit.svg so
# the chart does not silently lose its logo.
if [ ! -f "$LOGO_PATH" ]; then
    for cand in "$REPO_ROOT"/res/icons/dev.*.CosmicCaptureKit.svg; do
        case "$cand" in *.macos.svg) continue ;; esac
        if [ -f "$cand" ]; then LOGO_PATH="$cand"; break; fi
    done
fi
LOGO_B64=""
if [ -f "$LOGO_PATH" ]; then
    LOGO_B64="$(base64 -w0 "$LOGO_PATH")"
else
    echo "logo not found, rendering without it: $LOGO_PATH" >&2
fi

# ---------------------------------------------------------------------------
# Render: snapshot JSON in, SVG text out. Pure jq, so the geometry is one
# deterministic function of the counts and stays diffable in git.
# ---------------------------------------------------------------------------
RENDER='
def r2: (. * 100 | round) / 100;
def ceilv: if . == (. | floor) then . else (. | floor) + 1 end;
def fmtpct: (. * 10 | round) / 10 | if . == (. | floor) then (. | floor) else . end;
def pct($n; $t): if $t == 0 then 0 else ($n / $t * 100) end;

# Rounded top, square baseline, per the bar mark spec.
def barpath($x; $y; $w; $h; $r):
    "M\($x | r2),\(($y + $h) | r2) L\($x | r2),\(($y + $r) | r2) " +
    "Q\($x | r2),\($y | r2) \(($x + $r) | r2),\($y | r2) " +
    "L\(($x + $w - $r) | r2),\($y | r2) " +
    "Q\(($x + $w) | r2),\($y | r2) \(($x + $w) | r2),\(($y + $r) | r2) " +
    "L\(($x + $w) | r2),\(($y + $h) | r2) Z";

. as $snap
| ["bug", "feature", "task"] as $cats
| {bug: "Bugs Caught", feature: "Feature", task: "Task"} as $names
| ($snap.colors + {unknown: $unknown_color}) as $colors
| [ {key: "lifetime", title: "Project lifetime",      tally: $snap.lifetime},
    {key: "last30",   title: "\($range30) (30 days)", tally: $snap.last30} ] as $panels
| ([ $panels[] as $p | $cats[] | pct($p.tally[.]; $p.tally.total) ] | max) as $rawmax
| ([5, 10, 20, 25] | first(.[] | select($rawmax <= . * 4)) // 25) as $step
| ((($rawmax / $step) | ceilv) * $step | if . < 10 then 10 else . end) as $ymax
| 48   as $m
| 347  as $pw
| 192  as $top
| 332  as $base
| ($base - $top) as $span
| 24   as $bw
| ($pw / ($cats | length)) as $slot
| (if $logo_b64 != "" then $m + 58 else $m end) as $hx

| "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 838 456\" width=\"838\" height=\"456\" role=\"img\" aria-label=\"AI disclosure for \($snap.project): share of tickets by label, project lifetime and last 30 days\" font-family=\"Inter, Segoe UI, system-ui, sans-serif\">",
  "<title>\($snap.project) AI Disclosure</title>",
  "<defs>",
  ( $cats[] | "<filter id=\"glow-\(.)\" x=\"-60%\" y=\"-60%\" width=\"220%\" height=\"220%\"><feDropShadow dx=\"0\" dy=\"0\" stdDeviation=\"6\" flood-color=\"\($colors[.])\" flood-opacity=\"0.55\"/></filter>" ),
  "</defs>",
  "<rect x=\"0\" y=\"0\" width=\"838\" height=\"456\" rx=\"14\" fill=\"#0E0F12\"/>",
  (if $logo_b64 != "" then
     "<image x=\"\($m)\" y=\"32\" width=\"44\" height=\"44\" href=\"data:image/svg+xml;base64,\($logo_b64)\" xlink:href=\"data:image/svg+xml;base64,\($logo_b64)\"/>"
   else empty end),
  "<text x=\"\($hx)\" y=\"52\" font-size=\"21\" font-weight=\"650\" fill=\"#F7F8F8\">AI Disclosure</text>",
  "<text x=\"\($hx)\" y=\"74\" font-size=\"14\" fill=\"#B8BEC8\">This project is managed by a seasoned developer in collaboration with LLMs.</text>",
  "<text x=\"\($m)\" y=\"124\" font-size=\"16\" font-weight=\"650\" fill=\"#E8EAED\">Share of completed tickets by label</text>",

  ( $panels | to_entries[]
    | .value as $p
    | (if .key == 0 then $m else $m + $pw + $m end) as $px
    | "<text x=\"\($px)\" y=\"164\" font-size=\"15\" font-weight=\"600\" fill=\"#D0D6E0\">\($p.title) <tspan fill=\"#697077\" font-weight=\"400\">&#183; \($p.tally.total) tickets</tspan></text>",
      # Gridlines and tick labels, hairline and recessive.
      ( [range(0; ($ymax / $step) + 1)][]
        | (. * $step) as $tv
        | ($base - ($tv / $ymax * $span)) as $ty
        | "<line x1=\"\($px)\" y1=\"\($ty | r2)\" x2=\"\($px + $pw)\" y2=\"\($ty | r2)\" stroke=\"\(if $tv == 0 then "#2A2E36" else "#1B1E23" end)\" stroke-width=\"1\"/>",
          "<text x=\"\($px - 10)\" y=\"\(($ty + 4) | r2)\" font-size=\"11\" fill=\"#6B7280\" text-anchor=\"end\">\($tv)%</text>" ),
      # Bars with cap labels, category names, and counts.
      ( $cats | to_entries[]
        | .key as $ci | .value as $cat
        | ($p.tally[$cat]) as $count
        | pct($count; $p.tally.total) as $pv
        | ($pv / $ymax * $span) as $h
        | ($base - $h) as $by
        | ($px + $ci * $slot + ($slot - $bw) / 2) as $bx
        | ($bx + $bw / 2) as $cx
        | "<g><title>\($names[$cat]): \($count) of \($p.tally.total) tickets (\($pv | fmtpct)%)</title>",
          (if $count > 0 then
             "<path d=\"\(barpath($bx; $by; $bw; $h; (if $h < 8 then $h / 2 else 4 end)))\" fill=\"\($colors[$cat])\" filter=\"url(#glow-\($cat))\"/>"
           else empty end),
          "<text x=\"\($cx | r2)\" y=\"\((if $count > 0 then $by - 10 else $base - 8 end) | r2)\" font-size=\"13\" font-weight=\"600\" fill=\"#F7F8F8\" text-anchor=\"middle\">\($pv | fmtpct)%</text>",
          "<text x=\"\($cx | r2)\" y=\"360\" font-size=\"12.5\" fill=\"#C9CED6\" text-anchor=\"middle\">\($names[$cat])</text>",
          "<text x=\"\($cx | r2)\" y=\"379\" font-size=\"11\" fill=\"#697077\" text-anchor=\"middle\">\($count)</text>",
          "</g>" ) ),

  "<text x=\"\($m)\" y=\"414\" font-size=\"12\" fill=\"\($colors.bug)\" xml:space=\"preserve\">LLMs write bugs. A thorough human QA process keeps <tspan font-style=\"italic\">most</tspan> of these bugs from hitting production.</text>",
  "<g transform=\"translate(686, 402) scale(0.6667)\" opacity=\"0.9\"><title>Linear</title><path d=\"\($linear_d)\" fill=\"\($linear_color)\"/></g>",
  "<text x=\"790\" y=\"414\" font-size=\"11\" fill=\"#697077\" text-anchor=\"end\">Source: Linear</text>",
  "<text x=\"790\" y=\"432\" font-size=\"11\" fill=\"#4C525B\" text-anchor=\"end\">github.com/Frosthaven/cosmic-capture-kit &#183; generated \($gen)</text>",
  "</svg>"
'

mkdir -p "$(dirname "$OUT_PATH")"
# The 30-day range shows its year: once when both ends share it, twice across
# a year boundary. "Now" is captured once so a render straddling midnight
# cannot mix two different days in one label.
NOW_EPOCH="$(date -u +%s)"
START_EPOCH=$((NOW_EPOCH - 30 * 86400))
if [ "$(date -u -d "@$START_EPOCH" +%Y)" = "$(date -u -d "@$NOW_EPOCH" +%Y)" ]; then
    RANGE30="$(date -u -d "@$START_EPOCH" '+%b %-d') - $(date -u -d "@$NOW_EPOCH" '+%b %-d, %Y')"
else
    RANGE30="$(date -u -d "@$START_EPOCH" '+%b %-d, %Y') - $(date -u -d "@$NOW_EPOCH" '+%b %-d, %Y')"
fi

echo "$SNAPSHOT" | jq -r \
    --arg gen "$(date -u -d "@$NOW_EPOCH" +%Y-%m-%d)" \
    --arg range30 "$RANGE30" \
    --arg unknown_color "$UNKNOWN_COLOR" \
    --arg logo_b64 "$LOGO_B64" \
    --arg linear_d "$LINEAR_BADGE_D" \
    --arg linear_color "$LINEAR_BADGE_COLOR" \
    "$RENDER" > "$OUT_PATH"
echo "wrote $OUT_PATH" >&2

# The README embeds the PNG; the SVG stays alongside as the source of truth.
# 2x the 838px layout width keeps hidpi displays crisp.
PNG_PATH="${OUT_PATH%.svg}.png"
if command -v rsvg-convert >/dev/null 2>&1; then
    rsvg-convert -w 1676 -o "$PNG_PATH" "$OUT_PATH"
    echo "wrote $PNG_PATH" >&2
else
    echo "rsvg-convert not found: $PNG_PATH NOT refreshed (the README embeds it; install librsvg)" >&2
fi
