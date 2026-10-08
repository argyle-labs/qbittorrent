//! Dynamic (subprocess) entrypoint for the qbittorrent plugin.
//!
//! Serves this plugin over the orca socket via the typed `Plugin` builder.
//! The plugin is a `[[bin]]`, owns no runtime, and reaches orca only through
//! the socket. Advertises the `service` backend and the `qbittorrent.` tools.
plugin_toolkit::instrument::bootstrap!();
use plugin_toolkit::plugin::Plugin;
use qbittorrent::QbittorrentBackend;

// The builder does not force-link the lib, and without this the linker drops
// every `#[orca_tool]` / `#[endpoint_resource]` registration.
#[allow(unused_imports)]
use qbittorrent::tools as _;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("qbittorrent")
        .version(env!("CARGO_PKG_VERSION"))
        .service(QbittorrentBackend::new("qbittorrent"))
        .tools(["qbittorrent."])
        .serve()
}
