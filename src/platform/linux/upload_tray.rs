//! The Linux body of the cloud upload counter (DRAGON-482).
//!
//! One `ksni` StatusNotifierItem owned by THIS process, in the shape the recording tray's
//! `OwnBacking` already uses (`platform/linux/tray.rs`): the blocking `ksni` API owns its
//! own thread and D-Bus connection, a `Handle` updates the item in place, and dropping the
//! handle takes the item off the panel. Failure-safe: no SNI host means no item, and the
//! upload runs exactly the same.
//!
//! Everything the item SHOWS is decided in the shared tree (`cloud::upload::tray`): the
//! number, the glyph and the tooltip wording. This file only rasterizes and registers, and
//! it renders through the recording tray's own rasterizer, so the upload counter and the
//! recording icon are pixel-siblings (same 64px source, same ARGB32 packing, same accent
//! tint) rather than two different-looking icons from one app.
//!
//! # Why the counter is INK-fitted and the recording icon is not (DRAGON-500)
//!
//! It goes through `tray::render_icon_fitted`, not `render_icon`. The recording icon is one
//! glyph that always covers the same part of its box, so mapping the 24-unit viewBox 1:1 into
//! the pixmap draws it at a consistent weight. The counter is FOUR faces that do not: the
//! digits fill the box, the tick and the cross use two thirds of it, and the indeterminate
//! cloud mark barely two fifths. Rendered 1:1 the panel therefore got an icon whose size
//! changed as the upload progressed, and whose most common face (a small file never leaves
//! `Face::Indeterminate`) was a tiny cloud in a mostly empty cell: the owner's "the upload
//! icon is really small". Fitting each face's own ink to the cell is a RENDER-side fix, so the
//! shared artwork stays exactly as `cloud::tray` draws it for every platform.
//!
//! # Cancel (DRAGON-490)
//!
//! One `ksni` menu entry, [`crate::cloud::upload::tray::CANCEL_LABEL`], the same shape the
//! recording tray's own `menu()` already uses (`StandardItem` + an `activate` closure). This
//! REVERSES what this file's doc used to say outright — that a "cancel" leaving a
//! half-written remote file is worse than waiting, so there would never be a menu at all. The
//! owner asked for it anyway: the tradeoff is now accepted (see `cloud::tray`'s module doc),
//! not avoided. Choosing it sets the SAME `Arc<AtomicBool>` the child's per-chunk loop
//! already checks; nothing here talks to the transfer directly.

#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cloud::upload::tray::{CANCEL_LABEL, Face, face_svg, tooltip};

/// The `ksni` item: the account label and the number it is drawing, plus the accent it is
/// tinted with, a one-entry icon cache, and the cancel flag its one menu entry sets.
struct CounterTray {
    /// The account label, for the tooltip.
    label: String,
    /// What the item is currently drawing: a percentage, or the transfer's end state.
    face: Face,
    /// The app's resolved accent, exactly as the recording tray tints its icon.
    accent: [u8; 3],
    /// The last-rendered icon, keyed by the FACE, so a host that re-queries `icon_pixmap()`
    /// on every panel redraw (several do) does not re-run the usvg parse and the rasterize
    /// when nothing has changed. `ksni::Tray` methods are `&self`, hence the interior
    /// mutability; `icon_pixmap` is the only reader and writer.
    icon_cache: RefCell<Option<(Face, Vec<ksni::Icon>)>>,
    /// DRAGON-490: the SAME flag `child::run_cloud_upload` checks between chunks. The one
    /// menu entry below sets it; nothing else here reads it.
    canceled: Arc<AtomicBool>,
}

impl ksni::Tray for CounterTray {
    /// Unique per PROCESS, because two concurrent uploads are two children each registering
    /// their own item and a host that de-duplicated by id would show only one of them. The
    /// recording tray can use a bare constant precisely because only one ever exists.
    ///
    /// DRAGON-516 moved the string itself into the shared tree
    /// ([`crate::cloud::upload::tray::item_id`]) so the "keyed by pid" rule is stated once,
    /// beside the module doc's table of what identifies the item on the other two platforms.
    fn id(&self) -> String {
        crate::cloud::upload::tray::item_id(std::process::id())
    }

    fn title(&self) -> String {
        tooltip(&self.label, self.face)
    }

    /// The tooltip a panel shows on hover: the same sentence as the title, from the ONE
    /// shared builder, so the two can never say different things.
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: tooltip(&self.label, self.face),
            ..Default::default()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        if let Some((cached, icons)) = &*self.icon_cache.borrow()
            && *cached == self.face
        {
            return icons.clone();
        }
        // FITTED (DRAGON-500): each face's own ink fills the panel's cell. See the module doc
        // for why the counter needs this and the recording icon does not.
        let icons = crate::tray::render_icon_fitted(&face_svg(self.face), self.accent)
            .map(|i| vec![i])
            .unwrap_or_default();
        *self.icon_cache.borrow_mut() = Some((self.face, icons.clone()));
        icons
    }

    /// One entry: Cancel. The same `StandardItem` + `activate` shape the recording tray's own
    /// `menu()` uses; the closure only sets the flag, never touches the transfer itself.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: CANCEL_LABEL.to_string(),
                activate: Box::new(|t: &mut Self| t.canceled.store(true, Ordering::Relaxed)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// A live counter item. Dropping it removes the item from the panel.
pub struct Item {
    handle: ksni::blocking::Handle<CounterTray>,
}

/// How long any one `ksni` call may take before we stop waiting on it (DRAGON-482).
///
/// Three seconds. A panel that is going to answer answers in milliseconds; one that is not
/// must not hold up the transfer it is reporting on. Shorter than the keyring's five, because
/// these calls sit on the upload's own progress path rather than running once per session.
const TRAY_BUDGET: std::time::Duration = std::time::Duration::from_secs(3);

/// Run one blocking `ksni` call on its own thread, bounded by [`TRAY_BUDGET`].
///
/// The same shape `platform/linux/secrets.rs` uses, and for the same reason: the thread is
/// DETACHED on a timeout rather than joined, because it is blocked inside zbus and waiting for
/// it is exactly the hang this exists to prevent. It holds only its own handle and ends when
/// the bus eventually answers or the process exits.
fn with_budget<T: Send + 'static>(
    what: &'static str,
    op: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name(format!("cck-upload-tray-{what}"))
        .spawn(move || {
            let _ = tx.send(op());
        })
        .is_err()
    {
        return None;
    }
    match rx.recv_timeout(TRAY_BUDGET) {
        Ok(value) => Some(value),
        Err(_) => {
            log::debug!("cloud upload: the tray did not answer the {what} within {TRAY_BUDGET:?}");
            None
        }
    }
}

/// Raise the counter showing `n` for an upload to `label`. `None` when no StatusNotifierItem
/// host is available, which is an ordinary answer (a bare WM, a panel without the applet):
/// the upload then runs with no visible progress rather than failing.
///
/// `spawn` is a D-Bus handshake, so it is bounded: a host that never answers would otherwise
/// hold the upload before its first byte.
pub fn start(label: &str, face: Face, canceled: Arc<AtomicBool>) -> Option<Item> {
    use ksni::blocking::TrayMethods;
    let tray = CounterTray {
        label: label.to_string(),
        face,
        accent: {
            let [r, g, b, _] = crate::app::theme::resolved_appearance_accent_rgba();
            [r, g, b]
        },
        icon_cache: RefCell::new(None),
        canceled,
    };
    match with_budget("counter", move || tray.spawn())? {
        Ok(handle) => Some(Item { handle }),
        Err(e) => {
            log::debug!("cloud upload: no tray host for the progress counter ({e})");
            None
        }
    }
}

impl Item {
    /// Draw `n` (and re-title for the tooltip).
    ///
    /// **This DOES block**, which is why it is bounded (DRAGON-482). `ksni::blocking::Handle`
    /// is the blocking API: `update` is `block_on` around a round trip to the item's own
    /// thread and out to the bus, so a wedged panel would stall the upload's progress callback
    /// and, through it, the transfer. An earlier comment here claimed the call did not block;
    /// it always did. On a timeout the detached thread keeps the update and applies it if the
    /// bus ever answers, so nothing is lost, and the worst case is a counter one bucket stale.
    pub fn set(&self, label: &str, face: Face) {
        let handle = self.handle.clone();
        let label = label.to_string();
        let _ = with_budget("counter update", move || {
            handle.update(|t: &mut CounterTray| {
                t.label.clear();
                t.label.push_str(&label);
                t.face = face;
            });
        });
    }
}

impl Drop for Item {
    /// Take the item away. `shutdown` hands back an awaiter and is NOT awaited here, so this
    /// is the one `ksni` call on this path that genuinely does not block: dropping the awaiter
    /// leaves the service to close on its own.
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `id()` / `title()` are `ksni::Tray` trait methods; the trait must be in scope to call
    // them (the `impl` alone does not import it), the same note the recording tray carries.
    use ksni::Tray as _;

    fn tray(label: &str, face: Face) -> CounterTray {
        CounterTray {
            label: label.to_string(),
            face,
            accent: [0x33, 0x99, 0xff],
            icon_cache: RefCell::new(None),
            canceled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The Cancel entry really sets the flag, and only that: no other side effect, no
    /// touching the transfer directly (this file never talks to the provider layer at all).
    #[test]
    fn the_cancel_entry_sets_the_flag_and_nothing_else() {
        let mut t = tray("Work Drive", Face::Percent(40));
        assert!(!t.canceled.load(Ordering::Relaxed));
        let menu = t.menu();
        assert_eq!(menu.len(), 1, "one entry: Cancel");
        let ksni::MenuItem::Standard(item) = &menu[0] else {
            panic!("the Cancel entry must be a StandardItem");
        };
        assert_eq!(item.label, CANCEL_LABEL);
        (item.activate)(&mut t);
        assert!(t.canceled.load(Ordering::Relaxed), "activating Cancel must set the flag");
    }

    /// Two uploads at once are two processes, so the id must not be the same string in both
    /// or a de-duplicating host shows one counter for two transfers.
    ///
    /// DRAGON-516: it now comes from the shared builder, so this asserts the WIRING (this item
    /// asks for its own pid) and the shared test asserts the string's properties.
    #[test]
    fn the_item_id_is_per_process() {
        let t = tray("Work Drive", Face::Percent(40));
        assert_eq!(t.id(), crate::cloud::upload::tray::item_id(std::process::id()));
        assert!(t.id().starts_with("dev.frosthaven.CosmicCaptureKit.Upload."));
        assert!(t.id().ends_with(&std::process::id().to_string()));
        // A sibling child's item would be a different one, which is what lets two counters sit
        // side by side.
        assert_ne!(t.id(), crate::cloud::upload::tray::item_id(std::process::id() + 1));
    }

    /// The title and the tooltip come from the one shared builder, so a panel that shows
    /// either says the same thing.
    #[test]
    fn the_title_and_tooltip_agree_and_name_the_account() {
        let t = tray("Work Drive", Face::Percent(40));
        assert_eq!(t.title(), tooltip("Work Drive", Face::Percent(40)));
        assert_eq!(t.tool_tip().title, t.title());
        assert!(t.title().contains("Work Drive"));
    }

    /// The share of the pixmap's edge that `icons`' ink actually spans, longest axis, plus
    /// how far off-centre it sits: the two numbers DRAGON-500 is about. Measured from the
    /// ALPHA channel of the real rendered pixmap, which is the only thing a panel ever sees.
    fn ink_span(icon: &ksni::Icon) -> (f32, f32) {
        let (w, h) = (icon.width as usize, icon.height as usize);
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
        for y in 0..h {
            for x in 0..w {
                // ARGB32, alpha first (see `tray::render_hex`).
                if icon.data[(y * w + x) * 4] > 8 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        assert!(x1 >= x0 && y1 >= y0, "the face drew no ink at all");
        let (iw, ih) = ((x1 - x0) as f32, (y1 - y0) as f32);
        let off = (((x0 + x1) as f32) / 2.0 - w as f32 / 2.0)
            .abs()
            .max((((y0 + y1) as f32) / 2.0 - h as f32 / 2.0).abs());
        (iw.max(ih) / w as f32, off / w as f32)
    }

    /// DRAGON-500, the owner's report: EVERY face has to fill the cell the panel gives it.
    /// Rendered 1:1 from the shared 24-unit viewBox, `Face::Indeterminate`'s lone cloud mark
    /// covered 42% of the pixmap and sat 7% off-centre, which on a ~22px panel slot is a
    /// glyph roughly nine pixels across — "really small" — while the digit faces beside it
    /// covered 92%, so the item also changed size as the upload ran. Fitted, every face
    /// spans essentially the whole cell and is centred in it.
    #[test]
    fn every_face_fills_the_panel_cell_and_is_centred() {
        for face in [Face::Percent(7), Face::Percent(95), Face::Indeterminate, Face::Done, Face::Failed] {
            let t = tray("Work Drive", face);
            let icons = t.icon_pixmap();
            assert_eq!(icons.len(), 1, "{face:?} rendered no pixmap");
            let (span, off_centre) = ink_span(&icons[0]);
            assert!(span > 0.90, "{face:?} spans only {:.0}% of its cell", span * 100.0);
            assert!(off_centre < 0.02, "{face:?} sits {:.0}% off-centre", off_centre * 100.0);
        }
    }

    /// The icon really rasterizes on this machine, and the cache answers the second call
    /// with the same pixels (a host may ask on every redraw).
    #[test]
    fn the_icon_renders_and_is_cached_per_number() {
        let t = tray("Work Drive", Face::Percent(40));
        let first = t.icon_pixmap();
        assert_eq!(first.len(), 1, "the counter renders one pixmap");
        assert!(first[0].width > 0 && !first[0].data.is_empty());
        assert_eq!(t.icon_pixmap()[0].data, first[0].data, "the cache must not re-render");
        // A different number is a different picture, and so is each end state.
        for other in [Face::Percent(75), Face::Done, Face::Failed] {
            let t = tray("Work Drive", other);
            assert_ne!(t.icon_pixmap()[0].data, first[0].data, "{other:?} drew the same icon");
        }
    }
}
