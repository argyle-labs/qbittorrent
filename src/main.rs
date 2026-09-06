//! Dynamic (subprocess) entrypoint for the qbittorrent plugin.
//!
//! Serves this plugin over the orca socket via the typed `Plugin` builder.
//! The plugin is a `[[bin]]`, owns no runtime, and reaches orca only through
//! the socket. Advertises a single `service` backend.
plugin_toolkit::instrument::bootstrap!();
use plugin_toolkit::plugin::Plugin;
use qbittorrent::QbittorrentBackend;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    Plugin::named("qbittorrent")
        .version(env!("CARGO_PKG_VERSION"))
        .service(QbittorrentBackend::new("qbittorrent"))
        .serve()
}
