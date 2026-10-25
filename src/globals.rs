use anyhow::{Context, Result};
use wayland_client::{
    protocol::{
        wl_compositor::WlCompositor,
        wl_output::WlOutput,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
        wl_shm::WlShm,
    },
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1::ZxdgOutputManagerV1;
use wayland_protocols_wlr::{
    layer_shell::v1::client::zwlr_layer_shell_v1::ZwlrLayerShellV1,
    screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use crate::app::{App, Output, OutputInfo};

#[derive(Default)]
pub struct GlobalState {
    output_manager: Option<ZxdgOutputManagerV1>,
    screencopy_manager: Option<ZwlrScreencopyManagerV1>,
    shm: Option<WlShm>,
    compositor: Option<WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
}

impl Dispatch<WlRegistry, (), App> for GlobalState {
    fn event(
        state: &mut App,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<App>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_output" => {
                    state.outputs.push(Output {
                        proxy: registry.bind::<WlOutput, _, _>(name, version, qh, ()),
                        info: OutputInfo::default(),
                        dimming_layer: None,
                        dimming_buffer: None,
                    });
                }
                "zxdg_output_manager_v1" => {
                    state.globals.output_manager =
                        Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwlr_screencopy_manager_v1" => {
                    state.globals.screencopy_manager =
                        Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wl_shm" => {
                    state.globals.shm = Some(registry.bind(name, version, qh, ()));
                }
                "wl_compositor" => {
                    state.globals.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.globals.layer_shell = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_seat" => {
                    registry.bind::<WlSeat, _, _>(name, version.min(8), qh, ());
                }
                _ => {}
            }
        }
    }
}

pub struct Globals {
    pub output_manager: ZxdgOutputManagerV1,
    pub screencopy_manager: ZwlrScreencopyManagerV1,
    pub shm: WlShm,
    pub compositor: WlCompositor,
    pub layer_shell: ZwlrLayerShellV1,
}

impl GlobalState {
    pub fn extract_globals(&mut self) -> Result<Globals> {
        Ok(Globals {
            output_manager: self
                .output_manager
                .take()
                .context("Could not obtain xdg output manager!")?,
            screencopy_manager: self
                .screencopy_manager
                .take()
                .context("Could not obtain screencopy manager!")?,
            shm: self.shm.take().context("Could not get shm!")?,
            compositor: self
                .compositor
                .take()
                .context("Could not obtain compositor!")?,
            layer_shell: self
                .layer_shell
                .take()
                .context("Could not obtain layer shell!")?,
        })
    }
}
