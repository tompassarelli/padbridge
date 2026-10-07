//! The desktop pointer through the compositor's virtual pointer
//! (zwlr_virtual_pointer_v1, as wlrctl uses): relative motion in logical
//! pixels and button presses. Warcraft III under xwayland-satellite follows the
//! compositor's pointer; XTEST motion inside the X server does not move it.

use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, protocol::wl_registry};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

#[derive(Default)]
struct State {
    manager: Option<ZwlrVirtualPointerManagerV1>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            if interface == "zwlr_virtual_pointer_manager_v1" {
                state.manager = Some(registry.bind(name, version.min(1), qh, ()));
            }
        }
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrVirtualPointerManagerV1, _: <ZwlrVirtualPointerManagerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(_: &mut Self, _: &ZwlrVirtualPointerV1, _: <ZwlrVirtualPointerV1 as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Linux button codes, as wl_pointer uses them.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

pub struct VirtualPointer {
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
    started: std::time::Instant,
}

impl VirtualPointer {
    /// On the compositor named by WAYLAND_DISPLAY.
    pub fn new() -> Result<Self, String> {
        let connection = Connection::connect_to_env().map_err(|e| format!("connect to the compositor: {e}"))?;
        let mut queue = connection.new_event_queue();
        let qh = queue.handle();
        connection.display().get_registry(&qh, ());
        let mut state = State::default();
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        let manager = state.manager.clone().ok_or("the compositor offers no virtual pointer")?;
        let pointer = manager.create_virtual_pointer(None, &qh, ());
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
        Ok(Self { queue, state, pointer, started: std::time::Instant::now() })
    }

    fn time(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    fn flush(&mut self) -> Result<(), String> {
        self.pointer.frame();
        self.queue.flush().map_err(|e| e.to_string())?;
        self.queue.dispatch_pending(&mut self.state).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn motion(&mut self, dx: f64, dy: f64) -> Result<(), String> {
        self.pointer.motion(self.time(), dx, dy);
        self.flush()
    }

    pub fn absolute(&mut self, x: f64, y: f64, width: u32, height: u32) -> Result<(), String> {
        self.pointer.motion_absolute(self.time(), x.round() as u32, y.round() as u32, width, height);
        self.flush()
    }

    pub fn button(&mut self, right: bool, down: bool) -> Result<(), String> {
        use wayland_client::protocol::wl_pointer::ButtonState;
        self.pointer.button(self.time(), if right { BTN_RIGHT } else { BTN_LEFT }, if down { ButtonState::Pressed } else { ButtonState::Released });
        self.flush()
    }
}

#[cfg(test)]
mod tests {
    /// Run by hand on a desktop: connects and moves by nothing.
    #[test]
    #[ignore]
    fn the_compositor_takes_a_virtual_pointer() {
        let mut pointer = super::VirtualPointer::new().unwrap();
        pointer.motion(0.0, 0.0).unwrap();
    }
}

impl Drop for VirtualPointer {
    fn drop(&mut self) {
        self.pointer.destroy();
        let _ = self.queue.flush();
    }
}
