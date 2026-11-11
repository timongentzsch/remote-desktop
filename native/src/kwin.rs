//! Screen capture on KDE. KWin, KDE's compositor, casts an output to a PipeWire stream when asked through its
//! own Wayland protocol. Plasma before 6.8 only lets programs use that protocol whose desktop file declares
//! it, which is why `install` writes one.

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_stream_unstable_v1::{self as cast, ZkdeScreencastStreamUnstableV1};
use wayland_protocols_plasma::screencast::v1::client::zkde_screencast_unstable_v1::ZkdeScreencastUnstableV1;

use crate::pipewire;
use crate::stream::Stream;

/// The protocol's name, by which a compositor announces it.
pub const PROTOCOL: &str = "zkde_screencast_unstable_v1";
/// KWin's pointer modes: left out of the picture, or painted into it.
const POINTER_HIDDEN: u32 = 1;
const POINTER_EMBEDDED: u32 = 2;

/// What KWin has said about the cast so far.
#[derive(Default)]
struct Cast {
    node: Option<u32>,
    failed: Option<String>,
}

/// Captures the first output until the stream is no longer wanted or KDE ends the cast.
pub fn run(stream: Stream) -> Result<(), String> {
    let lost = |error| format!("lost the Wayland connection: {error}");
    let connection = Connection::connect_to_env().map_err(|error| format!("no Wayland display: {error}"))?;
    let (globals, mut events) = registry_queue_init::<Cast>(&connection).map_err(|error| error.to_string())?;
    let queue = events.handle();
    let missing = |error| format!("the compositor lacks a protocol this needs: {error}");
    let output: WlOutput = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let screencast: ZkdeScreencastUnstableV1 = globals.bind(&queue, 1..=1, ()).map_err(missing)?;
    let pointer = if stream.settings.cursor { POINTER_EMBEDDED } else { POINTER_HIDDEN };
    // The cast lasts as long as this object and the connection it belongs to.
    let _cast = screencast.stream_output(&output, pointer, &queue, ());

    let mut said = Cast::default();
    while said.node.is_none() && said.failed.is_none() {
        events.blocking_dispatch(&mut said).map_err(lost)?;
    }
    match (said.node, said.failed) {
        (Some(node), _) => pipewire::run(node, stream, "KDE"),
        (None, reason) => Err(format!("KDE refused the screen cast: {}", reason.unwrap_or_default())),
    }
}

impl Dispatch<ZkdeScreencastStreamUnstableV1, ()> for Cast {
    fn event(state: &mut Self, _: &ZkdeScreencastStreamUnstableV1, event: cast::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            cast::Event::Created { node } => state.node = Some(node),
            cast::Event::Failed { error } => state.failed = Some(error),
            cast::Event::Closed => state.failed = Some("it was closed".into()),
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Cast {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(Cast: ignore WlOutput);
delegate_noop!(Cast: ZkdeScreencastUnstableV1);
