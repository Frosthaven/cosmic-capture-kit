//! WHERE A SAVE WRITES — the preview editor's save-target rule, as pure path arithmetic.
//!
//! # The DRAGON-467 model, and why the `-edited` suffix is gone
//!
//! Until DRAGON-467 a dirty Save wrote a SEPARATE `name-edited.ext` beside the capture, with
//! a `-edited-2`, `-edited-3` collision walk behind it, so a fresh capture's auto-saved
//! original could never be overwritten by an edit. That mechanism is REMOVED. The reason it
//! could not stay is worth recording, because "protect the original" is a real instinct and
//! it will be suggested again:
//!
//! * **No mainstream capture tool names a derived copy.** Surveyed for this ticket: CleanShot
//!   X, ShareX, the Windows 11 Snipping Tool, macOS Screenshot/Markup and Preview, Flameshot,
//!   Greenshot, Snagit. Not one writes `-edited`, `_annotated` or any other semantic suffix.
//!   Where a tool DOES keep the untouched version it distinguishes it by FORMAT, not by name
//!   (`.cleanshot`, `.greenshot`, `.snagx` project files), and the flat image is the derived
//!   artifact. Where a collision suffix exists at all it is the plain OS counter, ShareX's
//!   `" (2)"`, never a word.
//! * **A known path means overwrite.** ShareX's editor writes straight over the file it knows
//!   about (its `SaveImageRequested` handler), macOS Preview overwrites on Cmd+S, and the
//!   Snipping Tool overwrites a file it opened. Our `-edited` rule was the odd one out.
//! * **The protection moved somewhere better.** Save is now a Save AS: the picker opens
//!   pre-filled with the path a plain save would have used, and the NATIVE dialog asks before
//!   replacing anything. That is the Snipping Tool's flow exactly ("save an additional copy of
//!   the captured snip by selecting the Save as button"), and it beats a suffix because the
//!   user sees the name, the folder and the replace prompt, instead of finding out afterwards
//!   that they have two files.
//!
//! So there is no derivation left to do. [`save_prefill`] answers one question — what path
//! does the picker open on — and the answer is always a path the user then confirms.
//!
//! [`png_name`] is the other half (DRAGON-455): a still is WRITTEN as PNG, so its name has to
//! say PNG. It composes over [`save_prefill`] at the one call site,
//! `App::preview_save_target`.

use std::path::{Path, PathBuf};

/// The one extension a still is ever saved under. See [`png_name`].
pub(super) const PNG_EXT: &str = "png";

/// THE STILL FORMAT RULE, as path arithmetic (DRAGON-455).
///
/// A still is written as PNG — the destination extension does not select a format (see
/// [`super::edit::bake_image`]). So the name a still is saved under has to say PNG too, or
/// the file lies about itself, which is the whole bug.
///
/// Three arms, and the split is about how much is really KNOWN about the current tail:
///
/// * already `.png` in any case — a `.PNG` capture IS a PNG — is returned untouched. We do
///   not re-case a name the user chose.
/// * another still-image extension we can open (`.jpg`, `.webp`, … — [`super::is_image_path`])
///   is REPLACED. That tail is certainly a format name, so nothing of the user's own name is
///   lost: an edited `clip.JPG` saves as `clip.png`.
/// * anything else — no extension at all, a bare trailing dot, or a dotted NAME like
///   `report.v2` — gets `.png` APPENDED, never substituted. `Path::extension` reads `v2`
///   there as an extension, and replacing it would silently eat part of a name the user
///   typed (the same trap `platform::windows::file_panel`'s allow-list guards). `report.v2.png`
///   is ugly and honest; `report.png` is tidy and wrong.
///
/// A name with no extension at all lands in the second arm too, through `set_extension`,
/// which fills an empty slot rather than appending past a trailing dot — so `shot` and
/// `shot.` both become `shot.png`, never `shot..png`.
///
/// RECORDINGS never come here: [`super::edit::bake_video`] really does honour the
/// destination container, so a recording keeps its own extension.
pub(super) fn png_name(path: &Path) -> PathBuf {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
    if ext.eq_ignore_ascii_case(PNG_EXT) {
        return path.to_path_buf();
    }
    if ext.is_empty() || super::is_image_path(path) {
        let mut out = path.to_path_buf();
        out.set_extension(PNG_EXT);
        return out;
    }
    // A non-empty extension implies a file name, so the fallback below is unreachable
    // defence rather than a real arm.
    match path.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push(".");
            name.push(PNG_EXT);
            path.with_file_name(name)
        }
        None => path.with_extension(PNG_EXT),
    }
}

/// THE save rule (DRAGON-467): the path the Save picker opens PRE-FILLED with, i.e. exactly
/// what a plain overwrite-save would have written.
///
/// Three inputs, three arms, in priority order:
///
/// 1. **`saved`** — `PreviewState::saved_path`, set once this document has actually written a
///    file (a Save destination the user picked). That path IS the document now, so a further
///    save goes straight back to it. This is the "a known path means overwrite" behaviour
///    every surveyed tool has.
/// 2. **`default_dir`** — the user's configured save folder (`screenshot_dir` for a still,
///    `record_dir` for a recording), used with the capture's own FILE NAME. This is what makes
///    the picker land in the right place for a capture that has not been saved yet, INCLUDING
///    one that only exists in the session runtime directory because "Automatically save
///    originals" is off. The setting is the basis for the Save action, so the folder it names
///    is where the picker opens, never the transient location the bytes happen to be in.
/// 3. **neither** — an external `--preview` document, which has no configured folder of its
///    own: it stays exactly where it is, so saving a file the user opened offers to write it
///    back over itself (again, what Preview and the Snipping Tool do), with the native
///    dialog's replace prompt as the guard.
///
/// The result is never uniquified and never suffixed. Collisions are the file chooser's job:
/// it names the existing file and asks. Pure; unit-tested below.
pub(super) fn save_prefill(
    saved: Option<&Path>,
    current: &Path,
    default_dir: Option<&Path>,
) -> PathBuf {
    if let Some(saved) = saved {
        return saved.to_path_buf();
    }
    match (default_dir, current.file_name()) {
        (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => current.to_path_buf(),
    }
}

#[cfg(test)]
mod save_prefill_tests {
    use super::*;

    /// A document that has SAVED once re-opens on its own file, whatever else is configured —
    /// the overwrite-in-place behaviour, and the reason repeated saves settle on ONE file
    /// instead of accumulating variants.
    #[test]
    fn a_saved_document_prefills_its_own_path() {
        let saved = Path::new("/home/me/report.png");
        let capture = Path::new("/run/user/1000/Screenshot 2026.png");
        let dir = Path::new("/home/me/Capture");
        assert_eq!(save_prefill(Some(saved), capture, Some(dir)), PathBuf::from("/home/me/report.png"));
        // Even with no configured folder at all.
        assert_eq!(save_prefill(Some(saved), capture, None), PathBuf::from("/home/me/report.png"));
    }

    /// An UNSAVED capture prefills the configured folder plus its own name. This is the case
    /// the "Automatically save originals" toggle creates: the bytes live in the runtime
    /// directory, but the picker must still open in the folder the user configured.
    #[test]
    fn an_unsaved_capture_prefills_the_configured_folder() {
        let dir = Path::new("/home/me/Capture");
        for capture in [
            // Already in the save folder (originals ON).
            "/home/me/Capture/Screenshot 2026.png",
            // Only in the runtime dir (originals OFF) — same answer.
            "/run/user/1000/Screenshot 2026.png",
        ] {
            assert_eq!(
                save_prefill(None, Path::new(capture), Some(dir)),
                PathBuf::from("/home/me/Capture/Screenshot 2026.png"),
                "{capture}"
            );
        }
        // Recordings behave identically against their own folder — there is no second
        // implementation for video, and the container rides along untouched.
        assert_eq!(
            save_prefill(
                None,
                Path::new("/run/user/1000/Recording 2026-07-29 at 10.30.mkv"),
                Some(Path::new("/home/me/Videos")),
            ),
            PathBuf::from("/home/me/Videos/Recording 2026-07-29 at 10.30.mkv")
        );
    }

    /// An EXTERNAL document (`--preview some/file.png`) has no configured folder, so it stays
    /// where it is: the save offers to write back over the file the user opened.
    #[test]
    fn an_external_document_stays_where_it_is() {
        let file = Path::new("/home/me/Downloads/holiday.png");
        assert_eq!(save_prefill(None, file, None), PathBuf::from("/home/me/Downloads/holiday.png"));
        // An empty configured folder is treated as no folder rather than as the root, so a
        // blank setting can never relocate a save to `/`.
        assert_eq!(save_prefill(None, file, Some(Path::new(""))), file.to_path_buf());
    }

    /// Degenerate paths: a path with no file name cannot be relocated, so it is returned
    /// as-is rather than producing the bare directory as a save target.
    #[test]
    fn a_path_with_no_file_name_is_left_alone() {
        assert_eq!(
            save_prefill(None, Path::new("/"), Some(Path::new("/home/me/Capture"))),
            PathBuf::from("/")
        );
    }

    /// The full life cycle: capture -> edit -> Save (picker, user confirms) -> edit -> Save.
    /// After the first save the document adopts the chosen path, so every later save prefills
    /// THAT, and exactly one edited file exists no matter how many times the user saves.
    #[test]
    fn repeated_saves_settle_on_the_chosen_file() {
        let dir = Path::new("/home/me/Capture");
        for (capture, chosen) in [
            ("/run/user/1000/2026.png", "/home/me/Capture/2026.png"),
            ("/run/user/1000/2026.mp4", "/home/me/Elsewhere/take-final.mp4"),
        ] {
            // 1. First save: the picker opens on the configured folder + the capture's name.
            let first = save_prefill(None, Path::new(capture), Some(dir));
            assert_eq!(first, dir.join(Path::new(capture).file_name().unwrap()));
            // 2. The user may accept that or pick elsewhere; either way it becomes `saved`...
            let saved = PathBuf::from(chosen);
            // ...and every later save prefills exactly it, never a derived variant.
            assert_eq!(save_prefill(Some(&saved), Path::new(capture), Some(dir)), saved);
            assert_eq!(save_prefill(Some(&saved), &saved, Some(dir)), saved);
        }
    }
}

#[cfg(test)]
mod png_name_tests {
    use super::*;

    /// The already-PNG arm: nothing moves, and a `.PNG` capture is not re-cased. The name
    /// the user chose is theirs.
    #[test]
    fn png_name_leaves_a_png_alone_whatever_its_case() {
        for name in ["/shots/a.png", "/shots/a.PNG", "/shots/a.Png", "a.png", "/shots/a.b.png"] {
            assert_eq!(png_name(Path::new(name)), PathBuf::from(name), "{name}");
        }
    }

    /// The REPLACE arm: a tail that certainly names a still format is swapped for `png`,
    /// so an edited external JPEG saves as a `.png` rather than PNG bytes wearing `.JPG`.
    #[test]
    fn png_name_replaces_an_image_extension() {
        for (given, want) in [
            ("/shots/clip.JPG", "/shots/clip.png"),
            ("/shots/clip.jpeg", "/shots/clip.png"),
            ("/shots/clip.webp", "/shots/clip.png"),
            ("/shots/clip.tiff", "/shots/clip.png"),
            ("/shots/clip.avif", "/shots/clip.png"),
            // The directory and a dotted STEM both survive — only the format tail moves.
            ("/shots/report.v2.gif", "/shots/report.v2.png"),
        ] {
            assert_eq!(png_name(Path::new(given)), PathBuf::from(want), "{given}");
        }
    }

    /// A missing extension (DRAGON-429's user-facing case, now handled portably rather than
    /// only in the Windows panel) and a bare trailing dot both simply GAIN `.png`.
    #[test]
    fn png_name_fills_in_a_missing_extension() {
        for (given, want) in [
            ("/shots/my shot", "/shots/my shot.png"),
            ("my shot", "my shot.png"),
            ("/shots/shot.", "/shots/shot.png"),
        ] {
            assert_eq!(png_name(Path::new(given)), PathBuf::from(want), "{given}");
        }
    }

    /// The APPEND arm, and the reason it exists: `Path::extension` reads the tail of a
    /// dotted NAME as an extension, so replacing it would eat part of what the user typed.
    /// `.png` goes on the end instead.
    #[test]
    fn png_name_appends_to_a_tail_that_is_not_a_format() {
        for (given, want) in [
            // A dotted name, not a type — the same shape the Windows dialog's allow-list
            // exists for.
            ("/shots/Recording 2026-07-29 at 10.30", "/shots/Recording 2026-07-29 at 10.30.png"),
            ("/shots/report.v2", "/shots/report.v2.png"),
            ("/shots/shot.xyz", "/shots/shot.xyz.png"),
            // A container the user typed on a STILL save: honest, not obeyed.
            ("/shots/shot.mp4", "/shots/shot.mp4.png"),
        ] {
            assert_eq!(png_name(Path::new(given)), PathBuf::from(want), "{given}");
        }
    }

    /// Idempotent, which is what lets it run on both the SUGGESTED name and the picked
    /// path: the native panels already force `.png` in their own ways, and a second pass
    /// must not stack a second extension.
    #[test]
    fn png_name_is_idempotent() {
        for name in [
            "/shots/a.png",
            "/shots/clip.JPG",
            "/shots/shot.xyz",
            "/shots/my shot",
            "/shots/Recording at 10.30",
        ] {
            let once = png_name(Path::new(name));
            assert_eq!(png_name(&once), once, "{name}");
        }
    }

    /// THE COMPOSITION `App::preview_save_target` performs: force PNG FIRST, then place the
    /// name in the configured folder. Order matters — relocating first would carry a `.JPG`
    /// tail into the save folder and offer the user a name nothing will ever be written to.
    #[test]
    fn a_still_prefill_is_always_png_in_the_configured_folder() {
        let external = png_name(Path::new("/shots/clip.JPG"));
        assert_eq!(
            save_prefill(None, &external, Some(Path::new("/home/me/Capture"))),
            PathBuf::from("/home/me/Capture/clip.png")
        );
    }
}
