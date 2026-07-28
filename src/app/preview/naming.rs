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

use std::path::{Path, PathBuf};

/// The marker appended to a capture's stem when an EDITED copy is saved beside it.
pub(super) const EDITED_SUFFIX: &str = "-edited";

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
        // A capitalised / multi-part extension keeps its exact spelling.
        assert_eq!(
            edited_target(Path::new("/shots/clip.JPG"), &none),
            PathBuf::from("/shots/clip-edited.JPG")
        );
        assert_eq!(
            edited_target(Path::new("/rec/take.mp4"), &none),
            PathBuf::from("/rec/take-edited.mp4")
        );
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
    #[test]
    fn repeated_save_cycles_settle_on_one_file() {
        let none = taken(&[]);
        // 1. Fresh capture, first dirty save.
        let capture = Path::new("/shots/2026.png");
        let first = save_target(capture, false, &none);
        assert_eq!(first, PathBuf::from("/shots/2026-edited.png"));
        // 2. The document adopts it (save_in_place = true) — the next save is in place...
        assert_eq!(save_target(&first, true, &none), first);
        // ...and would be even if the flag were somehow lost, because the stem carries the
        // marker. Belt and braces: the suffix can never compound.
        assert_eq!(save_target(&first, false, &none), first);
        // 3. Save As elsewhere; that destination is explicit, so it stays exact.
        let elsewhere = Path::new("/home/me/final.png");
        assert_eq!(save_target(elsewhere, true, &none), PathBuf::from("/home/me/final.png"));
        // The ORIGINAL capture is never a target after step 1.
        assert_ne!(first, capture);
    }
}
