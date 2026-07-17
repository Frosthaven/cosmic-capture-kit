# OCR test corpus

Labeled cases for tuning + regression-checking the text scanner (`src/detect/text.rs`).

## Adding a case

Either layout works:

- **Subfolder (recommended):** `tests/ocr/<name>/` containing one image
  (`.png`/`.jpg`/`.jpeg`/`.webp`) and `expected.txt` (the text it should read).
- **Flat:** `tests/ocr/<name>.png` + `tests/ocr/<name>.txt`.

`expected.txt` holds the plain text you expect, written naturally. The bench
normalises whitespace before comparing (so layout/newlines don't matter), but
**case matters** (a wrong case is a real OCR error). Use the captured region you'd
actually scan in the app, not the whole screen.

## Running

```sh
cargo build
./target/debug/cosmic-capture-kit --ocr-bench tests/ocr
```

It prints a per-case Levenshtein similarity (1.0 = exact), the `got` vs `want` when
they differ, and an overall mean + `pass(>=0.90)` count. `CCK_CONF=<n>` and
`CCK_NOCLEAN=1` tune the run the same way they do for `--scan-test`.

## What to include

Variety is what makes tuning safe — fixing one kind of image must not regress
another. Good cases to cover:

- **UI screenshots** (the primary use): menus, settings panes, code editors,
  terminals — often colored text, icons, multiple columns.
- **Stylized / display text:** thumbnails, posters, headings (heavy condensed
  fonts, colored words on dark backgrounds).
- **Small text** and **low-contrast** text.
- **Skewed** captures (slightly rotated).
- A couple of **easy** cases so regressions are obvious.
