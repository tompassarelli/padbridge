//! Read-only foreign-toplevel subscription for an explicitly selected private
//! compositor namespace. This protocol exposes app IDs and titles, not PIDs.
use std::collections::HashMap;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, protocol::wl_registry};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
};

#[derive(Default)]
struct Window {
    app_id: String,
    title: String,
    active: bool,
    done: bool,
}
#[derive(Default)]
struct State {
    manager: Option<ZwlrForeignToplevelManagerV1>,
    windows: HashMap<wayland_client::backend::ObjectId, Window>,
    finished: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "zwlr_foreign_toplevel_manager_v1" {
                state.manager = Some(registry.bind(name, version.min(3), qh, ()));
            }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            manager::Event::Toplevel { toplevel } => {
                state.windows.insert(toplevel.id(), Window::default());
            }
            manager::Event::Finished => {
                state.finished = true;
                state.windows.clear();
            }
            _ => {}
        }
    }
    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [0 => (ZwlrForeignToplevelHandleV1, ())]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwlrForeignToplevelHandleV1,
        event: handle::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, handle::Event::Closed) {
            state.windows.remove(&proxy.id());
            proxy.destroy();
            return;
        }
        let Some(window) = state.windows.get_mut(&proxy.id()) else {
            return;
        };
        match event {
            handle::Event::Title { title } => {
                window.title = title;
                window.done = false;
            }
            handle::Event::AppId { app_id } => {
                window.app_id = app_id;
                window.done = false;
            }
            handle::Event::State { state } => {
                window.active = state.chunks_exact(4).any(|bytes| {
                    u32::from_ne_bytes(bytes.try_into().expect("four-byte chunk"))
                        == handle::State::Activated as u32
                });
                window.done = false;
            }
            handle::Event::Done => window.done = true,
            _ => {}
        }
    }
}

pub struct Monitor {
    queue: EventQueue<State>,
    state: State,
    app_id: String,
}
impl Monitor {
    pub fn new(app_id: String) -> Result<Self, String> {
        if app_id.is_empty() {
            return Err("private compositor app ID must be nonempty".into());
        }
        let connection = Connection::connect_to_env().map_err(|e| e.to_string())?;
        let mut queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        if state.manager.is_none() {
            return Err("compositor does not expose foreign-toplevel management".into());
        }
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        Ok(Self {
            queue,
            state,
            app_id,
        })
    }

    pub fn eligible(&mut self) -> Result<bool, String> {
        // A sync barrier consumes the compositor's current state on the existing
        // subscription; no subprocess, activation request or cached success.
        self.queue
            .roundtrip(&mut self.state)
            .map_err(|e| e.to_string())?;
        if self.state.finished {
            return Err("compositor ended foreground observation".into());
        }
        let candidates: Vec<_> = self
            .state
            .windows
            .values()
            .filter(|window| window.app_id == self.app_id && window.title == "Warcraft III")
            .collect();
        Ok(candidates.len() == 1 && candidates[0].active && candidates[0].done)
    }
}
