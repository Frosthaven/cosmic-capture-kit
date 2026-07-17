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
use wayland_client::{Connection, QueueHandle, WEnum};
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

/// Per output name, the toplevels on the active workspace as global rects + ids.
// The transient `HashMap`/`HashSet` below are keyed by Wayland proxies. Those carry
// interior mutability (hence `mutable_key_type`), but we only ever use them as
// stable identity keys, so the lint is a false positive here.
#[allow(clippy::mutable_key_type)]
pub fn list_toplevels() -> HashMap<String, Vec<Toplevel>> {
    let mut result: HashMap<String, Vec<Toplevel>> = HashMap::new();
    let Ok(conn) = Connection::connect_to_env() else {
        return result;
    };
    let Ok((globals, mut queue)) = registry_queue_init::<WaylandState>(&conn) else {
        return result;
    };
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    // Workspace must be initialized before toplevel-info (matches the portal).
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let toplevel_info_state = ToplevelInfoState::new(&registry_state, &qh);
    let toplevel_manager_state = ToplevelManagerState::new(&registry_state, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let mut data = WaylandState {
        registry_state,
        output_state,
        workspace_state,
        toplevel_info_state,
        toplevel_manager_state,
        seat_state,
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
    let Ok(conn) = Connection::connect_to_env() else {
        return;
    };
    let Ok((globals, mut queue)) = registry_queue_init::<WaylandState>(&conn) else {
        return;
    };
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let toplevel_info_state = ToplevelInfoState::new(&registry_state, &qh);
    let toplevel_manager_state = ToplevelManagerState::new(&registry_state, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let mut data = WaylandState {
        registry_state,
        output_state,
        workspace_state,
        toplevel_info_state,
        toplevel_manager_state,
        seat_state,
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
    let Ok(conn) = Connection::connect_to_env() else {
        return false;
    };
    let Ok((globals, mut queue)) = registry_queue_init::<WaylandState>(&conn) else {
        return false;
    };
    let qh = queue.handle();
    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let workspace_state = WorkspaceState::new(&registry_state, &qh);
    let toplevel_info_state = ToplevelInfoState::new(&registry_state, &qh);
    let toplevel_manager_state = ToplevelManagerState::new(&registry_state, &qh);
    let seat_state = SeatState::new(&globals, &qh);
    let mut data = WaylandState {
        registry_state,
        output_state,
        workspace_state,
        toplevel_info_state,
        toplevel_manager_state,
        seat_state,
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
