//! WHERE A SAVE WRITES (DRAGON-353) — the preview editor's save-target rule, as pure
//! path arithmetic.
//!
//! Two facts drive every decision here:
//!
//! * **A capture's own file is never overwritten by an edit.** A fresh capture is
//!   auto-saved to a path the user did not choose; baking annotations into it in place
//!   would destroy the untouched original with no way back. So the first dirty Save
//!   writes a SEPARATE file, `name-edited.ext`.
//! * **A file the user DID choose is theirs to overwrite.** Once Save As has pointed the
//!   document at an explicit destination — or once a `-edited` file has been created and
//!   become the working document — further saves write straight back to it. Otherwise
//!   every save would spawn `-edited-edited-edited…`, and "save, tweak, save" would leave
//!   a trail of near-identical files.
//!
//! [`save_target`] is the whole rule; `PreviewState::save_in_place` is the "the user chose
//! this path" bit it consults. Everything is a pure function over a path plus an
//! `exists` predicate, so the collision policy is unit-testable with no filesystem.
//!
//! [`png_name`] is the other half (DRAGON-455): a still is WRITTEN as PNG, so its name has
//! to say PNG. It composes over `save_target` at the one call site,
//! `App::preview_save_target`.

use std::path::{Path, PathBuf};

/// The marker appended to a capture's stem when an EDITED copy is saved beside it.
pub(super) const EDITED_SUFFIX: &str = "-edited";

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
///   lost: an edited `clip.JPG` saves as `clip-edited.png`.
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

/// How many `-edited-N` variants to try before giving up and overwriting the plain
/// `-edited` name. Absurdly high on purpose: the loop exists so a pre-existing
/// `shot-edited.png` from an earlier session is never silently clobbered, not to model a
/// realistic count.
const MAX_VARIANTS: u32 = 999;

/// Whether `stem` already carries the edited marker — `foo-edited` or `foo-edited-7`.
///
/// This is what stops the suffix from compounding: once a document IS the `-edited` file,
/// saving it again writes to itself rather than minting `foo-edited-edited`.
fn is_edited_stem(stem: &str) -> bool {
    if stem.ends_with(EDITED_SUFFIX) {
        return true;
    }
    // `…-edited-<digits>`: split the trailing number off and re-check the head.
    match stem.rsplit_once('-') {
        Some((head, tail)) => {
            !tail.is_empty()
                && tail.chars().all(|c| c.is_ascii_digit())
                && head.ends_with(EDITED_SUFFIX)
        }
        None => false,
    }
}

/// Rebuild `src`'s path with a different file STEM, preserving its directory and its
/// extension (including the extension's original case — a `.JPG` capture stays `.JPG`).
fn with_stem(src: &Path, stem: &str) -> PathBuf {
    let name = match src.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}.{ext}"),
        None => stem.to_string(),
    };
    match src.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.join(name),
        _ => PathBuf::from(name),
    }
}

/// The `-edited` sibling of `src`, guaranteed not to collide with a file `exists` reports.
///
/// * `shot.png` → `shot-edited.png`
/// * `shot.png` when `shot-edited.png` is taken → `shot-edited-2.png` (then `-3`, …)
/// * `shot-edited.png` / `shot-edited-2.png` → ITSELF (the document already IS the edited
///   copy; saving it again writes in place rather than compounding the suffix)
/// * `notes` (no extension) → `notes-edited`
///
/// The collision policy is DELIBERATELY non-destructive: a `shot-edited.png` left over from
/// a previous session is somebody's work, and this editor has no way to know it isn't. The
/// only overwrite is of a file this same document already created (the second bullet's
/// exception), which is exactly the "keep saving my edit" case.
pub(super) fn edited_target(src: &Path, exists: &dyn Fn(&Path) -> bool) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if is_edited_stem(&stem) {
        return src.to_path_buf();
    }
    let first = with_stem(src, &format!("{stem}{EDITED_SUFFIX}"));
    if !exists(&first) {
        return first;
    }
    for n in 2..=MAX_VARIANTS {
        let candidate = with_stem(src, &format!("{stem}{EDITED_SUFFIX}-{n}"));
        if !exists(&candidate) {
            return candidate;
        }
    }
    // A thousand taken variants: saving SOMETHING beats refusing to save, so fall back to
    // the plain name (which the caller will overwrite).
    first
}

/// THE save-target rule. `current` is the document's working path; `explicit` is
/// `PreviewState::save_in_place` — "the user chose this path" (a Save As destination, or
/// an `-edited` file this document already minted and adopted).
///
/// Explicit ⇒ write straight back. Otherwise ⇒ the collision-safe `-edited` sibling, so a
/// fresh capture's auto-saved original survives its first edit.
pub(super) fn save_target(
    current: &Path,
    explicit: bool,
    exists: &dyn Fn(&Path) -> bool,
) -> PathBuf {
    if explicit {
        current.to_path_buf()
    } else {
        edited_target(current, exists)
    }
}

/// The real filesystem predicate for [`save_target`] / [`edited_target`]. Split out only
/// so the tests can substitute a fake set of taken names.
pub(super) fn on_disk(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// An `exists` predicate over a fixed set of taken paths.
    fn taken(names: &[&str]) -> impl Fn(&Path) -> bool + use<> {
        let set: HashSet<PathBuf> = names.iter().map(PathBuf::from).collect();
        move |p: &Path| set.contains(p)
    }

    /// The base derivation: `-edited` goes on the STEM, the extension rides along
    /// unchanged (case included), and the directory is preserved.
    #[test]
    fn edited_target_appends_to_the_stem_and_keeps_the_extension() {
        let none = taken(&[]);
        assert_eq!(
            edited_target(Path::new("/shots/Screenshot 2026.png"), &none),
            PathBuf::from("/shots/Screenshot 2026-edited.png")
        );
        // A capitalised extension keeps its exact spelling. Shown on a RECORDING since
        // DRAGON-455: this derivation is format-agnostic (it is the video half's rule too),
        // and a still never reaches it with a foreign extension any more — `png_name` runs
        // first, so `clip.JPG` derives from `clip.png`. See
        // `a_still_target_is_always_png_before_the_edited_derivation`.
        assert_eq!(
            edited_target(Path::new("/shots/clip.MOV"), &none),
            PathBuf::from("/shots/clip-edited.MOV")
        );
        // RECORDINGS derive identically (DRAGON-398: the video editor's Save writes the same
        // `-edited` sibling as an image's, from this same rule — there is no second naming
        // implementation for video). Every container the recorder can produce:
        for (src, want) in [
            ("/rec/take.mp4", "/rec/take-edited.mp4"),
            ("/rec/take.mkv", "/rec/take-edited.mkv"),
            ("/rec/take.webm", "/rec/take-edited.webm"),
            ("/rec/take.mov", "/rec/take-edited.mov"),
            // A recorder's default name is spaced and dotted; both survive.
            ("/rec/Recording 2026-07-29 at 10.30.mp4", "/rec/Recording 2026-07-29 at 10.30-edited.mp4"),
        ] {
            assert_eq!(edited_target(Path::new(src), &none), PathBuf::from(want), "{src}");
        }
        // Only the LAST extension is an extension, so a dotted stem keeps its dots.
        assert_eq!(
            edited_target(Path::new("/shots/a.b.png"), &none),
            PathBuf::from("/shots/a.b-edited.png")
        );
    }

    /// A file with NO extension still gets the marker (and gains no stray dot).
    #[test]
    fn edited_target_handles_a_missing_extension() {
        let none = taken(&[]);
        assert_eq!(
            edited_target(Path::new("/shots/notes"), &none),
            PathBuf::from("/shots/notes-edited")
        );
        // A bare relative name keeps its bare form (no directory invented).
        assert_eq!(edited_target(Path::new("notes"), &none), PathBuf::from("notes-edited"));
    }

    /// THE ANTI-COMPOUNDING RULE: a document that already IS the edited copy saves back to
    /// ITSELF, so save → edit → save cycles stay on one file instead of growing
    /// `-edited-edited-edited`. True even when the name is taken (it is taken BY US).
    #[test]
    fn an_already_edited_name_saves_in_place() {
        let all = |_: &Path| true;
        for name in ["/shots/a-edited.png", "/shots/a-edited-2.png", "/shots/a-edited-17.mp4"] {
            assert_eq!(edited_target(Path::new(name), &all), PathBuf::from(name), "{name}");
        }
        // A stem that merely CONTAINS the word is not the marker.
        assert_eq!(
            edited_target(Path::new("/shots/edited-shot.png"), &taken(&[])),
            PathBuf::from("/shots/edited-shot-edited.png")
        );
        // `-edited-` followed by something that isn't a number is not a variant either.
        assert_eq!(
            edited_target(Path::new("/shots/a-edited-final.png"), &taken(&[])),
            PathBuf::from("/shots/a-edited-final-edited.png")
        );
    }

    /// COLLISION POLICY: a pre-existing `-edited` file (somebody else's work, from an
    /// earlier session) is never silently overwritten — the save steps to `-edited-2`,
    /// `-edited-3`, … until it finds a free name.
    #[test]
    fn a_pre_existing_edited_file_is_never_clobbered() {
        let one = taken(&["/shots/a-edited.png"]);
        assert_eq!(
            edited_target(Path::new("/shots/a.png"), &one),
            PathBuf::from("/shots/a-edited-2.png")
        );
        let several =
            taken(&["/shots/a-edited.png", "/shots/a-edited-2.png", "/shots/a-edited-3.png"]);
        assert_eq!(
            edited_target(Path::new("/shots/a.png"), &several),
            PathBuf::from("/shots/a-edited-4.png")
        );
    }

    /// The whole rule: an implicit target derives `-edited`; an EXPLICIT one (a Save As
    /// destination, or an adopted `-edited` file) writes straight back — never gaining a
    /// second marker, and never uniquified away from the path the user picked.
    #[test]
    fn save_target_respects_an_explicitly_chosen_path() {
        // Everything is "taken", so a uniquifying bug would be loud.
        let all = |_: &Path| true;
        let chosen = Path::new("/home/me/report.png");
        assert_eq!(save_target(chosen, true, &all), PathBuf::from("/home/me/report.png"));
        // The same path WITHOUT the explicit flag is a fresh capture: protect the original.
        assert_eq!(
            save_target(chosen, false, &taken(&[])),
            PathBuf::from("/home/me/report-edited.png")
        );
    }

    /// The FULL life cycle the ambiguity is really about: capture → edit → Save →
    /// edit → Save → Save As → edit → Save. After the first save the document adopts the
    /// `-edited` file and every later save writes to whatever path is current, so exactly
    /// TWO files exist at the end (the untouched original and the edit), plus whatever
    /// Save As put where the user asked.
    ///
    /// Run for BOTH media kinds (DRAGON-398): a cut recording follows exactly the same
    /// life cycle as an annotated screenshot, because the video editor's Save routes through
    /// this same rule rather than a parallel one.
    #[test]
    fn repeated_save_cycles_settle_on_one_file() {
        let none = taken(&[]);
        for (capture, first_edit, elsewhere) in [
            ("/shots/2026.png", "/shots/2026-edited.png", "/home/me/final.png"),
            ("/rec/2026.mp4", "/rec/2026-edited.mp4", "/home/me/final.mp4"),
        ] {
            // 1. Fresh capture, first dirty save.
            let capture = Path::new(capture);
            let first = save_target(capture, false, &none);
            assert_eq!(first, PathBuf::from(first_edit));
            // 2. The document adopts it (save_in_place = true) — the next save is in place...
            assert_eq!(save_target(&first, true, &none), first);
            // ...and would be even if the flag were somehow lost, because the stem carries the
            // marker. Belt and braces: the suffix can never compound.
            assert_eq!(save_target(&first, false, &none), first);
            // 3. Save As elsewhere; that destination is explicit, so it stays exact.
            assert_eq!(save_target(Path::new(elsewhere), true, &none), PathBuf::from(elsewhere));
            // The ORIGINAL capture is never a target after step 1.
            assert_ne!(first, capture.to_path_buf());
        }
    }

    // ── DRAGON-455: a still's name says PNG, because a still IS a PNG ────────────────

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

    /// THE COMPOSITION `App::preview_save_target` performs: force PNG FIRST, then derive
    /// `-edited`. Order matters — deriving first would produce `clip-edited.JPG` and run the
    /// collision walk against a name nothing will ever be written to.
    #[test]
    fn a_still_target_is_always_png_before_the_edited_derivation() {
        let none = taken(&[]);
        let external = png_name(Path::new("/shots/clip.JPG"));
        assert_eq!(
            save_target(&external, false, &none),
            PathBuf::from("/shots/clip-edited.png")
        );
        // And the collision walk now checks the name that WILL be written.
        let clash = taken(&["/shots/clip-edited.png"]);
        assert_eq!(
            save_target(&external, false, &clash),
            PathBuf::from("/shots/clip-edited-2.png")
        );
    }
}
