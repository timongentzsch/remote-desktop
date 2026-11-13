//! What every use of the Wayland compositor here starts with: the connection, the list of what the compositor
//! offers, and one wording for what can go wrong.

use std::fmt::Display;
use std::ops::RangeInclusive;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_v1::{self, ZxdgOutputV1};

use crate::screen::Screen;

/// Whether the session has a Wayland compositor. If not, it may be an X11 session.
pub fn available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
}

/// Connects to the session's compositor and asks what it offers.
pub fn connect<State>() -> Result<(Connection, GlobalList, EventQueue<State>), String>
where
    State: Dispatch<WlRegistry, GlobalListContents> + 'static,
{
    let connection = Connection::connect_to_env().map_err(|error| format!("no Wayland display: {error}"))?;
    let (globals, events) = registry_queue_init(&connection).map_err(|error| error.to_string())?;
    Ok((connection, globals, events))
}

/// Binds the object through which one of the compositor's protocols is used, in the protocol's first version.
pub fn bind<Interface, State>(globals: &GlobalList, queue: &QueueHandle<State>) -> Result<Interface, String>
where
    Interface: Proxy + 'static,
    State: Dispatch<Interface, ()> + 'static,
{
    bind_versions(globals, queue, 1..=1)
}

/// Like `bind`, in the newest of `versions` that the compositor has.
pub fn bind_versions<Interface, State>(
    globals: &GlobalList,
    queue: &QueueHandle<State>,
    versions: RangeInclusive<u32>,
) -> Result<Interface, String>
where
    Interface: Proxy + 'static,
    State: Dispatch<Interface, ()> + 'static,
{
    globals.bind(queue, versions, ()).map_err(|error| format!("the compositor lacks a protocol this needs: {error}"))
}

pub fn lost(error: impl Display) -> String {
    format!("lost the Wayland connection: {error}")
}

/// One turn of talking to the compositor: sends what is pending, waits up to `limit` for it to say something
/// or for `other` to become readable, and lets `state` handle what it said.
pub fn turn<State>(
    events: &mut EventQueue<State>,
    state: &mut State,
    other: Option<RawFd>,
    limit: Duration,
) -> Result<(), String> {
    events.flush().map_err(lost)?;
    if let Some(guard) = events.prepare_read() {
        let [readable, _] = crate::platform::unix::fd::wait_readable(
            [guard.connection_fd().as_raw_fd(), other.unwrap_or(-1)],
            Some(limit),
        );
        if readable {
            guard.read().map_err(lost)?;
        }
    }
    events.dispatch_pending(state).map_err(lost)?;
    Ok(())
}

/// The names of the protocols the compositor offers.
pub fn protocols() -> Result<Vec<String>, String> {
    struct Asking;
    impl Dispatch<WlRegistry, GlobalListContents> for Asking {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    let (_, globals, _) = connect::<Asking>()?;
    Ok(globals.contents().with_list(|list| list.iter().map(|global| global.interface.clone()).collect()))
}

/// What the compositor says about one output while the outputs are listed.
#[derive(Default)]
struct Told {
    name: Option<String>,
    /// Where it lies and how many pixels it has, as the output itself says,
    position: (i32, i32),
    pixels: (i32, i32),
    scale: i32,
    /// and in the desktop's units, which take scaling and rotation into account, where the compositor says.
    logical_position: Option<(i32, i32)>,
    logical_size: Option<(i32, i32)>,
}

/// The outputs being listed, each event carrying the place of its output in here.
struct Listing(Vec<Told>);

impl Dispatch<WlOutput, usize> for Listing {
    fn event(
        listing: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let told = &mut listing.0[*index];
        match event {
            wl_output::Event::Geometry { x, y, .. } => told.position = (x, y),
            wl_output::Event::Mode { flags, width, height, .. }
                if flags.into_result().is_ok_and(|flags| flags.contains(wl_output::Mode::Current)) =>
            {
                told.pixels = (width, height)
            }
            wl_output::Event::Scale { factor } => told.scale = factor,
            wl_output::Event::Name { name } => told.name = Some(name),
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, usize> for Listing {
    fn event(
        listing: &mut Self,
        _: &ZxdgOutputV1,
        event: zxdg_output_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zxdg_output_v1::Event::LogicalPosition { x, y } => listing.0[*index].logical_position = Some((x, y)),
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                listing.0[*index].logical_size = Some((width, height))
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Listing {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Listing: ZxdgOutputManagerV1);

/// The compositor's outputs in its own order, each with the number it has in the compositor's registry, which
/// is the same on every connection.
fn outputs() -> Result<Vec<(u32, Screen)>, String> {
    let (_connection, globals, mut events) = connect::<Listing>()?;
    let queue = events.handle();
    let found: Vec<(u32, u32)> = globals.contents().with_list(|list| {
        list.iter()
            .filter(|global| global.interface == "wl_output")
            .map(|global| (global.name, global.version))
            .collect()
    });
    let logical = bind_versions::<ZxdgOutputManagerV1, _>(&globals, &queue, 1..=3).ok();
    let mut listing = Listing(found.iter().map(|_| Told::default()).collect());
    for (index, &(number, version)) in found.iter().enumerate() {
        // An output says its name from the protocol's fourth version on.
        let output: WlOutput = globals.registry().bind(number, version.min(4), &queue, index);
        if let Some(logical) = &logical {
            logical.get_xdg_output(&output, &queue, index);
        }
    }
    events.roundtrip(&mut listing).map_err(lost)?;
    let screen = |(index, told): (usize, Told)| {
        let scale = told.scale.max(1);
        let (x, y) = told.logical_position.unwrap_or(told.position);
        let (width, height) = told.logical_size.unwrap_or((told.pixels.0 / scale, told.pixels.1 / scale));
        Screen { name: told.name.unwrap_or_else(|| format!("Screen {}", index + 1)), x, y, width, height }
    };
    Ok(found.into_iter().map(|(number, _)| number).zip(listing.0.into_iter().enumerate().map(screen)).collect())
}

/// The compositor's outputs as screens, in its own order.
pub fn screens() -> Result<Vec<Screen>, String> {
    Ok(outputs()?.into_iter().map(|(_, screen)| screen).collect())
}

/// Binds the output called `wanted`, or the compositor's first where none is wanted or none is called that.
pub fn output<State>(globals: &GlobalList, queue: &QueueHandle<State>, wanted: Option<&str>) -> Result<WlOutput, String>
where
    State: Dispatch<WlOutput, ()> + 'static,
{
    let number = match wanted {
        Some(wanted) => outputs()?.into_iter().find(|(_, screen)| screen.name == wanted).map(|(number, _)| number),
        None => None,
    };
    match number {
        Some(number) => Ok(globals.registry().bind(number, 1, queue, ())),
        None => bind(globals, queue),
    }
}
