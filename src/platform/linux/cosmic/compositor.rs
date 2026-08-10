//! COSMIC toplevel enumeration + activation via the cosmic toplevel-info /
//! toplevel-management protocols (cctk). The Linux body of the
//! [`crate::platform::compositor`] facade: [`crate::platform::compositor`] keeps
//! the portable [`Toplevel`](crate::platform::compositor::Toplevel)/`WinRect`
//! identities and the macOS / fallback arms, and `pub use`-re-exports the four
//! entry points below on Linux, so every caller of `platform::compositor::*`
//! resolves unchanged.
//!
//! One-shot enumeration of toplevel identities on the active workspace, per
//! output, via the cosmic toplevel-info protocol (cctk). Window selection uses
//! this to lay out the picker grid; the actual pixels come from
//! [`crate::record`], which captures each toplevel directly by handle (so
//! occlusion is a non-issue — no focusing required). Modeled on
//! xdg-desktop-portal-cosmic's wayland module.

use crate::platform::compositor::Toplevel;
use cosmic_client_toolkit::sctk;
use cosmic_client_toolkit::sctk::output::{OutputHandler, OutputState};
use cosmic_client_toolkit::sctk::registry::{ProvidesRegistryState, RegistryState};
use cosmic_client_toolkit::sctk::seat::{Capability, SeatHandler, SeatState};
use cosmic_client_toolkit::toplevel_info::{ToplevelInfo, ToplevelInfoHandler, ToplevelInfoState};
use cosmic_client_toolkit::toplevel_management::{ToplevelManagerHandler, ToplevelManagerState};
use cosmic_client_toolkit::workspace::{WorkspaceHandler, WorkspaceState};
use cosmic_protocols::toplevel_info::v1::client::zcosmic_toplevel_handle_v1;
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1;
use std::collections::{HashMap, HashSet};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::ext::workspace::v1::client::ext_workspace_handle_v1;

struct WaylandState {
    registry_state: RegistryState,
    output_state: OutputState,
    workspace_state: WorkspaceState,
    toplevel_info_state: ToplevelInfoState,
    toplevel_manager_state: ToplevelManagerState,
    seat_state: SeatState,
}

impl ProvidesRegistryState for WaylandState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState];
}

impl OutputHandler for WaylandState {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl WorkspaceHandler for WaylandState {
    fn workspace_state(&mut self) -> &mut WorkspaceState {
        &mut self.workspace_state
    }
    fn done(&mut self) {}
}

impl ToplevelInfoHandler for WaylandState {
    fn toplevel_info_state(&mut self) -> &mut ToplevelInfoState {
        &mut self.toplevel_info_state
    }
    fn new_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn update_toplevel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
    fn toplevel_closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &ExtForeignToplevelHandleV1) {}
}

impl SeatHandler for WaylandState {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl ToplevelManagerHandler for WaylandState {
    fn toplevel_manager_state(&mut self) -> &mut ToplevelManagerState {
        &mut self.toplevel_manager_state
    }
    fn capabilities(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: Vec<WEnum<zcosmic_toplevel_manager_v1::ZcosmicToplelevelManagementCapabilitiesV1>>,
    ) {
    }
}

sctk::delegate_output!(WaylandState);
sctk::delegate_registry!(WaylandState);
sctk::delegate_seat!(WaylandState);
cosmic_client_toolkit::delegate_toplevel_manager!(WaylandState);
cosmic_client_toolkit::delegate_workspace!(WaylandState);
cosmic_client_toolkit::delegate_toplevel_info!(WaylandState);

/// Whether the COSMIC toplevel protocols are reachable from THIS process, checked before any
/// of the four entry points below builds a `ToplevelInfoState` or a `ToplevelManagerState`.
///
/// **This guard exists because the toolkit panics rather than degrading** (`lab/flatpak`).
/// `ToplevelInfoState::new` and `ToplevelManagerState::new` are both
/// `Self::try_new(..).unwrap()`, so on a compositor that does not advertise the global the
/// constructor aborts the process:
///
/// ```text
/// thread 'main' (2) panicked at client-toolkit/src/toplevel_info.rs:132:37:
/// called `Option::unwrap()` on a `None` value
/// ```
///
/// That is a real crash observed on the first sandboxed launch, not a hypothetical. cosmic-comp
/// hides `zcosmic_toplevel_info_v1` and `ext_foreign_toplevel_list_v1` from clients carrying a
/// security context, which is every Flatpak, so the very first window enumeration killed the
/// app before any UI appeared. Note the `(2)` in that trace: PID 2, because each `flatpak run`
/// has its own PID namespace.
///
/// Asking the shared probe rather than a sandbox check is deliberate, for the same reason
/// `platform::layer_overlay_available` does: the protocol can be missing because we are not
/// allowed to see it OR because the compositor never implemented it, and every caller here
/// wants the same answer either way. A session that DOES advertise them is byte-identical to
/// before this guard existed.
///
/// # It was testing the WRONG TERM, which is not the same as being absent (DRAGON-620)
///
/// This read `WaylandProtocols.toplevel_list`, and the probe set that flag from
/// `ext_foreign_toplevel_list_v1` OR `zcosmic_toplevel_info_v1`. wlroots >= 0.18 advertises the
/// first. So on Sway the guard PASSED, `ToplevelInfoState::new` then succeeded because its
/// global really was there, and `ToplevelManagerState::new` aborted on the very next line: its
/// global, `zcosmic_toplevel_manager_v1`, is COSMIC-only and nothing in the guard had asked
/// about it. A guard that passes for the wrong reason is worse than no guard, because it reads
/// as covered.
///
/// It now asks [`crate::platform::backend::window_list_supported`], the same predicate behind
/// `Caps.window_list`, so the capability the UI reads and the guard the plugin enforces cannot
/// answer differently. The `try_new` calls below are the belt to this braces: the probe matches
/// interface NAMES and never versions, so it is the honest CAPABILITY answer while `try_new` is
/// what actually makes an abort impossible.
fn cosmic_toplevels_reachable() -> bool {
    crate::platform::backend::window_list_supported(&crate::platform::backend::wayland_protocols())
}

/// Open a throwaway Wayland connection and bind every toplevel state the four entry points
/// below share. `None` when this session cannot support them, which each caller turns into
/// its own honest empty answer.
///
/// All four used to inline this, which is how the missing manager guard came to be missing in
/// four places at once. Binding in ONE place means the next protocol added here cannot be
/// guarded in three of them.
///
/// Both `try_new` calls are load-bearing, not defensive noise. cctk's `new` is
/// `try_new(..).unwrap()`, and the probe that [`cosmic_toplevels_reachable`] consults matches
/// interface NAMES out of the registry while `bind_one` additionally demands a VERSION range
/// (`2..=3` for toplevel-info, `1..=4` for the manager). So the probe can legitimately say yes
/// to a global whose version we cannot bind, and only asking the toolkit itself closes that
/// gap. Anything unbindable ends the session's window features rather than the process.
fn connect_toplevels() -> Option<(Connection, EventQueue<WaylandState>, WaylandState)> {
    if !cosmic_toplevels_reachable() {
        return None;
    }
    let conn = Connection::connect_to_env().ok()?;
    let (globals, queue) = registry_queue_init::<WaylandState>(&conn).ok()?;
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    // Workspace must be initialized before toplevel-info (matches the portal).
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let toplevel_info_state = ToplevelInfoState::try_new(&registry_state, &qh)?;
    let toplevel_manager_state = ToplevelManagerState::try_new(&registry_state, &qh)?;
    let seat_state = SeatState::new(&globals, &qh);
    Some((
        conn,
        queue,
        WaylandState {
            registry_state,
            output_state,
            workspace_state,
            toplevel_info_state,
            toplevel_manager_state,
            seat_state,
        },
    ))
}

/// Per output name, the toplevels on the active workspace as global rects + ids.
// The transient `HashMap`/`HashSet` below are keyed by Wayland proxies. Those carry
// interior mutability (hence `mutable_key_type`), but we only ever use them as
// stable identity keys, so the lint is a false positive here.
#[allow(clippy::mutable_key_type)]
pub fn list_toplevels() -> HashMap<String, Vec<Toplevel>> {
    let mut result: HashMap<String, Vec<Toplevel>> = HashMap::new();
    // `_conn` is never read again, but the connection must outlive the queue that borrows it.
    let Some((_conn, mut queue, mut data)) = connect_toplevels() else {
        return result;
    };
    // The ext-foreign-toplevel-list enumeration + cosmic-info handshake arrive
    // asynchronously: back-to-back roundtrips don't give the compositor
    // wall-clock time to deliver them. Dispatch over a short real-time window,
    // stopping early once the toplevel count settles (non-zero & stable).
    let mut last = 0usize;
    let mut stable = 0;
    for _ in 0..40 {
        if queue.roundtrip(&mut data).is_err() {
            return result;
        }
        let n = data.toplevel_info_state.toplevels().count();
        if n > 0 && n == last {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
        }
        last = n;
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    // output -> (name, logical position)
    let mut out_info: HashMap<WlOutput, (String, (i32, i32))> = HashMap::new();
    for output in data.output_state.outputs() {
        if let Some(info) = data.output_state.info(&output)
            && let (Some(name), Some(pos)) = (info.name.clone(), info.logical_position) {
                out_info.insert(output, (name, pos));
            }
    }

    // active workspace handles
    let mut active: HashSet<ext_workspace_handle_v1::ExtWorkspaceHandleV1> = HashSet::new();
    for wg in data.workspace_state.workspace_groups() {
        for handle in &wg.workspaces {
            if let Some(w) = data.workspace_state.workspace_info(handle)
                && w.state.contains(ext_workspace_handle_v1::State::Active) {
                    active.insert(handle.clone());
                }
        }
    }

    for info in data.toplevel_info_state.toplevels() {
        let on_active =
            info.workspace.is_empty() || info.workspace.iter().any(|w| active.contains(w));
        if !on_active {
            continue;
        }
        let active = info.state.contains(&zcosmic_toplevel_handle_v1::State::Activated);
        for (output, geo) in &info.geometry {
            if let Some((name, pos)) = out_info.get(output) {
                result.entry(name.clone()).or_default().push(Toplevel {
                    rect: (pos.0 + geo.x, pos.1 + geo.y, geo.width, geo.height),
                    id: info.identifier.clone(),
                    active,
                    title: info.title.clone(),
                });
            }
        }
    }
    result
}

// ============================================================================
// Cursor-output probe: the reliable per-output cursor signal for an
// overlay-less IMMEDIATE capture (`--active-monitor` / `--active-window`).
//
// Wayland exposes NO global pointer position without a mapped surface, so — exactly
// like the interactive picker's `capture_pointer_output` (a real capture overlay's
// first `wl_pointer` enter) — we momentarily mint ONE transparent, focus-neutral
// layer surface PER OUTPUT (`Layer::Overlay`, `KeyboardInteractivity::None`), let
// the compositor route the pointer to whichever surface the cursor sits over, and
// read that surface's output name off the first `wl_pointer.enter`. The surfaces are
// fully transparent (invisible — no flicker) and torn down the instant we resolve;
// keyboard-None means the currently-Activated toplevel is NOT deactivated (so the
// `--active-window` arm still sees it). Runs on its own throwaway Wayland connection
// (like `list_toplevels`), so it never touches libcosmic's event loop.
// ============================================================================

use cosmic_client_toolkit::sctk::compositor::{CompositorHandler, CompositorState};
use cosmic_client_toolkit::sctk::seat::pointer::{
    PointerData, PointerEvent, PointerEventKind, PointerHandler,
};
use cosmic_client_toolkit::sctk::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use cosmic_client_toolkit::sctk::shell::WaylandSurface;
use cosmic_client_toolkit::sctk::shm::{slot::SlotPool, Shm, ShmHandler};
use wayland_client::protocol::{wl_pointer, wl_shm};

/// One momentary transparent probe surface pinned to a specific output.
struct ProbeSurface {
    layer: LayerSurface,
    output_name: String,
    logical_size: (i32, i32),
    drawn: bool,
}

struct PointerProbe {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    shm: Shm,
    pool: SlotPool,
    surfaces: Vec<ProbeSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    /// The name of the output the cursor first entered: the answer.
    entered: Option<String>,
}

impl PointerProbe {
    /// Attach a fully-transparent buffer to `idx`'s surface (once), mapping it so the
    /// compositor will route the pointer to it. Transparent = invisible = no flicker.
    fn draw(&mut self, idx: usize) {
        let (lw, lh) = self.surfaces[idx].logical_size;
        let (w, h) = (lw.max(1), lh.max(1));
        let stride = w * 4;
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // Argb8888, all zero => fully transparent.
        canvas.fill(0);
        let surface = self.surfaces[idx].layer.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        if buffer.attach_to(surface).is_ok() {
            self.surfaces[idx].layer.commit();
            self.surfaces[idx].drawn = true;
        }
    }
}

impl ProvidesRegistryState for PointerProbe {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    sctk::registry_handlers![OutputState, SeatState];
}

impl OutputHandler for PointerProbe {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl SeatHandler for PointerProbe {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && self.pointer.is_none()
            && let Ok(ptr) = self.seat_state.get_pointer(qh, &seat)
        {
            self.pointer = Some(ptr);
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl CompositorHandler for PointerProbe {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_surface::WlSurface, _: wayland_client::protocol::wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_surface::WlSurface, _: &WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wayland_client::protocol::wl_surface::WlSurface, _: &WlOutput) {}
}

impl LayerShellHandler for PointerProbe {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {}
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        if let Some(idx) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface() == layer.wl_surface())
            && !self.surfaces[idx].drawn
        {
            self.draw(idx);
        }
    }
}

impl PointerHandler for PointerProbe {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            if !matches!(event.kind, PointerEventKind::Enter { .. }) {
                continue;
            }
            if let Some(s) = self.surfaces.iter().find(|s| *s.layer.wl_surface() == event.surface) {
                self.entered = Some(s.output_name.clone());
                return;
            }
        }
    }
}

impl ShmHandler for PointerProbe {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

sctk::delegate_compositor!(PointerProbe);
sctk::delegate_output!(PointerProbe);
sctk::delegate_shm!(PointerProbe);
sctk::delegate_seat!(PointerProbe);
// Keep upstream's cursor-shape delegation exactly as `delegate_pointer!(PointerProbe)`
// writes it. Only the `wl_pointer` half below is ours.
sctk::delegate_pointer!(PointerProbe, pointer: []);

/// `wl_pointer` dispatch for the probe, wrapping smithay-client-toolkit's.
///
/// **This is the SAME bug, and the same fix, as the one carried in our iced fork**
/// (DRAGON-609; see `FORKED_CHANGES.md`, "The iced fork", patch 2). Two instances,
/// one defect: cosmic-comp sends `wl_pointer.enter` without the `wl_pointer.frame`
/// that has been mandatory since `wl_seat` version 5. sctk correctly buffers the
/// enter and drains only on a frame, so with no frame `pointer_frame` is never
/// called at all. iced needed it patching for the picker's loupe; this probe needs
/// it patching for its own answer, and neither fix reaches the other because iced
/// and this file talk to sctk independently.
///
/// **Without this, the probe never resolved.** Measured on two certified-clean
/// runs: four `wl_pointer` objects, four `enter` events, ZERO `frame` events, so
/// [`PointerProbe::entered`] stayed `None` and `pointer_output` burned its whole
/// budget and returned `None` every time. Every caller then fell back to the
/// primary output, which is why the overlay-less `--active-window` /
/// `--active-monitor` captures did not follow the cursor across monitors.
///
/// The departure from upstream is one thing: after an `Enter`, hand sctk a
/// synthetic `Frame` to flush the batch it is holding. It is a no-op on a
/// compliant compositor, because sctk calls `pointer_frame` only when its pending
/// buffer is non-empty, so the real frame that follows finds it already drained.
/// The full four-case argument lives on the fork's copy; keep the two in step.
///
/// # It is LAYER SURFACES specifically, and the no-op is observed, not just argued
///
/// This is the sharpest fact to know before touching any of this, and it
/// post-dates the fork's copy of the comment, so it is recorded here and in
/// `FORKED_CHANGES.md` rather than there. One session carrying both kinds of
/// surface, read off `WAYLAND_DEBUG=1`:
///
/// | surface | kind | enter | frame |
/// |---|---|---|---|
/// | `wl_surface#17` | `zwlr_layer_shell_v1.get_layer_surface` | 1 | **0** |
/// | `wl_surface#83` | `xdg_wm_base.get_xdg_surface` | 1 | **1** |
///
/// cosmic-comp DOES send the mandatory frame for xdg toplevels and omits it only
/// for layer surfaces. That is why every overlay we own was affected while the
/// settings and preview windows never showed a symptom, and it is why this looked
/// for a long time like an iced bug rather than a compositor one.
///
/// It also turns the "harmless on a compliant path" claim from reasoning into a
/// measurement. In that same session the xdg toplevel received its real frame,
/// our synthetic frame had already drained the buffer, sctk's
/// `if !pending.is_empty()` guard made the real one a no-op, and the window
/// tracked all 35 of its pointer motions normally. Case 2 is confirmed live.
impl wayland_client::Dispatch<wl_pointer::WlPointer, PointerData> for PointerProbe {
    fn event(
        state: &mut Self,
        proxy: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        data: &PointerData,
        conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        let was_enter = matches!(event, wl_pointer::Event::Enter { .. });

        <SeatState as wayland_client::Dispatch<wl_pointer::WlPointer, PointerData, Self>>::event(
            state, proxy, event, data, conn, qhandle,
        );

        // THE DEPARTURE FROM UPSTREAM. Everything else on this path is sctk's.
        if was_enter {
            <SeatState as wayland_client::Dispatch<wl_pointer::WlPointer, PointerData, Self>>::event(
                state,
                proxy,
                wl_pointer::Event::Frame,
                data,
                conn,
                qhandle,
            );
        }
    }
}
sctk::delegate_layer!(PointerProbe);
sctk::delegate_registry!(PointerProbe);

/// The NAME of the output the pointer sits on, or `None` when it can't be resolved
/// (layer-shell/shm unavailable, no outputs, or no pointer enter within the budget — the
/// caller then falls back to the primary output / single window).
///
/// DRAGON-601 briefly had this return the POINT as well, to seed the colour picker's loupe
/// on a layer surface where the toolkit reported no cursor until the pointer moved. That
/// seed is gone (DRAGON-609). It never once fired on a clean run, because the probe was
/// hitting the very same missing-frame bug it was built to work around, so it could not
/// answer either. The real fix is two dispatches up: iced's, in our fork, and this file's,
/// just above. The picker now learns its position from the `wl_pointer.enter` directly,
/// which is both earlier and more accurate than a probe on a second connection.
///
/// WHY a probe at all: an overlay-less immediate capture (`--active-monitor` /
/// `--active-window`) mints NO picker surface, and Wayland exposes NO global pointer
/// position without a mapped surface. So we momentarily mint ONE transparent,
/// focus-neutral (`KeyboardInteractivity::None`) `Layer::Overlay` surface PER OUTPUT,
/// let the compositor route the pointer to whichever surface the cursor is over, and read
/// that surface's output off the first `wl_pointer.enter` — the SAME signal the
/// interactive picker learns as `capture_pointer_output` (its real overlay's first
/// pointer-enter). Focus-None means the currently-`Activated` toplevel is NOT deactivated,
/// so the `--active-window` arm still sees it. The surfaces are fully transparent
/// (invisible) and torn down (dropped) the instant we resolve. Runs on its own throwaway
/// connection, so it never touches libcosmic's event loop; blocks up to ~350ms.
pub fn pointer_output() -> Option<String> {
    let Ok(conn) = Connection::connect_to_env() else {
        return None;
    };
    let Ok((globals, mut queue)) = registry_queue_init::<PointerProbe>(&conn) else {
        return None;
    };
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let (Ok(compositor), Ok(layer_shell), Ok(shm)) = (
        CompositorState::bind(&globals, &qh),
        LayerShell::bind(&globals, &qh),
        Shm::bind(&globals, &qh),
    ) else {
        return None;
    };
    let Ok(pool) = SlotPool::new(256, &shm) else {
        return None;
    };
    let mut probe = PointerProbe {
        registry_state,
        output_state,
        seat_state,
        shm,
        pool,
        surfaces: Vec::new(),
        pointer: None,
        entered: None,
    };
    // Settle the output + seat enumeration before minting surfaces.
    for _ in 0..20 {
        if queue.roundtrip(&mut probe).is_err() {
            return None;
        }
        if probe.output_state.outputs().next().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // One transparent Overlay-layer surface per named output, pinned to that output.
    let outputs: Vec<(WlOutput, String, (i32, i32))> = probe
        .output_state
        .outputs()
        .filter_map(|o| {
            let info = probe.output_state.info(&o)?;
            let name = info.name.clone()?;
            let size = info.logical_size?;
            Some((o, name, size))
        })
        .collect();
    if outputs.is_empty() {
        return None;
    }
    for (output, name, size) in outputs {
        let surface = compositor.create_surface(&qh);
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("cck-cursor-probe"),
            Some(&output),
        );
        // Pin to the output's top-left and size it to the whole output so the pointer,
        // wherever it is on that output, lands on this surface. Focus-neutral.
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(size.0.max(1) as u32, size.1.max(1) as u32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.commit();
        probe.surfaces.push(ProbeSurface {
            layer,
            output_name: name,
            logical_size: size,
            drawn: false,
        });
    }

    // Dispatch until the pointer enters one of the probe surfaces (or the budget lapses).
    for _ in 0..35 {
        if queue.roundtrip(&mut probe).is_err() {
            break;
        }
        if probe.entered.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    probe.entered
}

/// Focus/activate the toplevel with this stable `identifier`, returning focus to
/// the monitor we launched on so the annotation window opens where expected.
/// No-op if the window or a seat can't be found.
pub fn activate(identifier: &str) {
    let id = identifier.to_string();
    activate_where(move |t| t.identifier == id);
}

/// Focus/activate the (first) toplevel whose title matches — used to focus an
/// already-open settings window in another instance instead of warning.
pub fn activate_title(title: &str) {
    let title = title.to_string();
    activate_where(move |t| t.title == title);
}

/// Activate the first toplevel matching `pred` (via the cosmic toplevel manager).
/// No-op if no match, no `cosmic_toplevel` handle, or no seat is found.
#[allow(clippy::mutable_key_type)]
fn activate_where(pred: impl Fn(&ToplevelInfo) -> bool) {
    let Some((conn, mut queue, mut data)) = connect_toplevels() else {
        return;
    };
    // Wait for the async toplevel enumeration to settle (same as list_toplevels()).
    let mut last = 0usize;
    let mut stable = 0;
    for _ in 0..40 {
        if queue.roundtrip(&mut data).is_err() {
            return;
        }
        let n = data.toplevel_info_state.toplevels().count();
        if n > 0 && n == last {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
        }
        last = n;
        std::thread::sleep(std::time::Duration::from_millis(15));
    }

    let Some(handle) = data
        .toplevel_info_state
        .toplevels()
        .find(|t| pred(t))
        .and_then(|t| t.cosmic_toplevel.clone())
    else {
        return;
    };
    let Some(seat) = data.seat_state.seats().next() else {
        return;
    };
    data.toplevel_manager_state.manager.activate(&handle, &seat);
    let _ = conn.flush();
    let _ = queue.roundtrip(&mut data);
}

/// EXPERIMENTAL (DRAGON-317): move the toplevel titled `title` to the ACTIVE
/// workspace of the output named `output_name`, via the cosmic toplevel-management
/// protocol's `move_to_ext_workspace(toplevel, workspace, output)` request. This is
/// the ONLY client-side lever that can relocate a mapped xdg_toplevel to another
/// output on COSMIC — an xdg_toplevel cannot choose its own output, and cosmic-comp
/// maps a fresh window on the seat's active (pointer) output, not the capture-origin
/// one. Used to re-home the WINDOWED preview editor onto the trigger monitor after
/// it maps (the overlay preview already pins its output at layer-surface creation).
///
/// FLOATING RETENTION — the key caveat (verified against cosmic-comp `Shell::move_window`,
/// src/shell/mod.rs @ 15ed782e): on move the destination LAYER is chosen SOLELY by the
/// destination workspace's `tiling_enabled` flag; the window's own floating tiling-exception
/// rule is NOT consulted (that only applies at INITIAL map). So the preview stays floating
/// on the trigger monitor ONLY when that monitor's active workspace is NOT auto-tiling
/// (the COSMIC default). On an auto-tiling destination workspace the moved window is
/// force-tiled regardless of any float rule — the DRAGON-222 blocker, which no client-side
/// sequence can dodge (the clean fix is compositor-side: preserve the source layer in
/// `move_window`).
///
/// Runs on its own throwaway Wayland connection (like [`activate`]) so it never
/// touches libcosmic's event loop, and is fire-and-forget from a detached thread —
/// the settle + poll loop below blocks up to ~2.4s. Returns whether the move request
/// was actually sent (target found, not already on the output, manager ≥ v4).
#[allow(clippy::mutable_key_type)]
pub fn move_toplevel_to_output(title: &str, output_name: &str) -> bool {
    let Some((conn, mut queue, mut data)) = connect_toplevels() else {
        return false;
    };
    // The preview toplevel's title is set asynchronously after its surface maps, so poll
    // (not just settle a count): dispatch over a real-time window until a toplevel with
    // our exact title has been enumerated with a cosmic handle, up to ~2.4s.
    let mut found = false;
    for _ in 0..80 {
        if queue.roundtrip(&mut data).is_err() {
            return false;
        }
        found = data
            .toplevel_info_state
            .toplevels()
            .any(|t| t.title == title && t.cosmic_toplevel.is_some());
        if found {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    if !found {
        return false;
    }

    // move_to_ext_workspace is a v4 request; a manager bound below it would make the call
    // a protocol violation (killing the connection), so bail cleanly on an older host.
    if data.toplevel_manager_state.manager.version() < 4 {
        log::debug!(
            "DRAGON-317: zcosmic_toplevel_manager_v1 < v4 — no move_to_ext_workspace, \
             preview stays on the pointer's monitor"
        );
        return false;
    }

    // Resolve the target output by name.
    let Some(target_output) = data.output_state.outputs().find(|o| {
        data.output_state
            .info(o)
            .and_then(|i| i.name)
            .is_some_and(|n| n == output_name)
    }) else {
        return false;
    };

    // The active workspace on the target output: the group owning that output, then the
    // member workspace flagged Active.
    let Some(target_workspace) = data
        .workspace_state
        .workspace_groups()
        .find(|g| g.outputs.contains(&target_output))
        .and_then(|g| {
            g.workspaces.iter().find(|h| {
                data.workspace_state
                    .workspace_info(h)
                    .is_some_and(|w| w.state.contains(ext_workspace_handle_v1::State::Active))
            })
        })
        .cloned()
    else {
        return false;
    };

    // Our toplevel handle — and skip if it's already (partly) on the target output, so a
    // preview that happened to map on the right monitor isn't needlessly remapped.
    let Some(handle) = data.toplevel_info_state.toplevels().find_map(|t| {
        if t.title != title {
            return None;
        }
        if t.geometry.keys().any(|o| o == &target_output) {
            return None; // already there
        }
        t.cosmic_toplevel.clone()
    }) else {
        return false;
    };

    data.toplevel_manager_state.manager.move_to_ext_workspace(
        &handle,
        &target_workspace,
        &target_output,
    );
    let _ = conn.flush();
    let _ = queue.roundtrip(&mut data);
    true
}

/// DRAGON-194 follow-up: activate the toplevel `target_id` and WAIT (bounded,
/// ~1.2s) until the toplevel `subject_id` reports the desired `Activated` state,
/// re-issuing the activation while it doesn't hold. A single fire-and-forget
/// [`activate`] provably races the compositor's own focus restoration: the
/// capture overlay has just closed, and cosmic-comp returns focus to the
/// pre-overlay toplevel on its own schedule, clobbering our activation
/// mid-settle (the picked settings window still captured unfocused). Mirrors
/// the macOS focus seam's bounded frontmost-confirmation poll. Returns whether
/// the desired state was observed (callers grab best-effort either way).
pub fn activate_until(target_id: &str, subject_id: &str, want_active: bool) -> bool {
    let Some((conn, mut queue, mut data)) = connect_toplevels() else {
        return false;
    };
    // Wait for the async toplevel enumeration to settle (same as list_toplevels()).
    let mut last = 0usize;
    let mut stable = 0;
    for _ in 0..40 {
        if queue.roundtrip(&mut data).is_err() {
            return false;
        }
        let n = data.toplevel_info_state.toplevels().count();
        if n > 0 && n == last {
            stable += 1;
            if stable >= 3 {
                break;
            }
        } else {
            stable = 0;
        }
        last = n;
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    // Poll-and-reissue with a STABILITY requirement. A first observation of the
    // desired state is NOT enough: the compositor's post-overlay focus
    // restoration can land AFTER it (confirmed at T, clobbered at T+50ms,
    // grabbed unfocused at T+200ms — the exact failure traced live on a real
    // capture). Count consecutive matching polls and return only once the state
    // has held ~400ms; any flip resets the streak and re-fires the activation.
    const STABLE_TICKS: u32 = 13; // ~400ms at the 30ms cadence
    let mut stable: u32 = 0;
    for attempt in 0..80u32 {
        if queue.roundtrip(&mut data).is_err() {
            return false;
        }
        let subject_active = data
            .toplevel_info_state
            .toplevels()
            .find(|t| t.identifier == subject_id)
            .map(|t| t.state.contains(&zcosmic_toplevel_handle_v1::State::Activated));
        match subject_active {
            Some(a) if a == want_active => {
                stable += 1;
                if stable >= STABLE_TICKS {
                    return true;
                }
            }
            None => return false, // the picked window vanished mid-capture
            _ => {
                stable = 0;
                if attempt % 4 == 0 {
                    let handle = data
                        .toplevel_info_state
                        .toplevels()
                        .find(|t| t.identifier == target_id)
                        .and_then(|t| t.cosmic_toplevel.clone());
                    if let (Some(handle), Some(seat)) =
                        (handle, data.seat_state.seats().next())
                    {
                        data.toplevel_manager_state.manager.activate(&handle, &seat);
                        let _ = conn.flush();
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    false
}
