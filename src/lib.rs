//! qbittorrent service backend — qBittorrent BitTorrent transfer client.
//!
//! Implements `ServiceBackend` so the generic `service.*` tools
//! (deploy/backup/restore/configure/status/connect/sync) drive qbittorrent,
//! alongside the `qbittorrent.` WebUI tools in [`tools`]. The only orca dep is
//! `plugin-toolkit`. See orca/docs/PLUGIN-PROGRAM.md.
#![allow(clippy::disallowed_types)]

pub mod execute;
pub mod listen_port;
pub mod tools;

use plugin_toolkit::service::{
    BoxFuture, Routes, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceStatus,
    WorkloadSpec,
};

/// qbittorrent backend. Holds only the provider name; per-instance endpoint/creds
/// come from the instance + `Routes` the generic `service.*` tools hand each op.
#[derive(Debug, Clone)]
pub struct QbittorrentBackend {
    provider: &'static str,
}

impl QbittorrentBackend {
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

impl ServiceBackend for QbittorrentBackend {
    fn provider(&self) -> &str {
        self.provider
    }

    /// Runtimes qbittorrent can be placed on. `service.deploy` hands the
    /// `workload_spec` below to a matching deploy target — this backend never
    /// drives pct/docker itself (that mechanic lives in the deploy-target domain).
    fn runtimes(&self) -> Vec<Runtime> {
        vec![Runtime::Docker, Runtime::Podman, Runtime::Lxc, Runtime::Vm]
    }

    fn capabilities(&self) -> Vec<ServiceCapability> {
        vec![
            ServiceCapability::Deploy,
            ServiceCapability::Backup,
            ServiceCapability::Restore,
            ServiceCapability::Configure,
            ServiceCapability::Status,
        ]
    }

    fn default_port(&self) -> u16 {
        8080
    }

    /// In-workload paths holding config/data. This is ALL qbittorrent declares for
    /// backup — the generic pluggable backup (tar for containers/LXC, PBS for
    /// Proxmox guests when available) snapshots these. No backup/restore code
    /// here; those are inherited from ServiceBackend's defaults.
    fn data_paths(&self) -> Vec<String> {
        vec!["/config".to_string()]
    }

    fn workload_spec<'a>(
        &'a self,
        _runtime: Runtime,
        _instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>> {
        // TODO: describe the qbittorrent workload (image/template, ports, mounts,
        // env) for the chosen runtime. The deploy target turns this into a
        // compose service / LXC config / VM. See deploy-target::WorkloadSpec.
        Box::pin(async move { Err(ServiceError::unimplemented("qbittorrent.workload_spec")) })
    }

    fn configure<'a>(
        &'a self,
        _instance: &'a str,
        _routes: &'a Routes,
        _config: &'a str,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        // TODO: apply qbittorrent-specific config idempotently.
        Box::pin(async move { Err(ServiceError::unimplemented("qbittorrent.configure")) })
    }

    fn status<'a>(
        &'a self,
        _instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<ServiceStatus, ServiceError>> {
        // TODO: real health/diagnostics.
        Box::pin(async move { Err(ServiceError::unimplemented("qbittorrent.status")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_provider() {
        let b = QbittorrentBackend::new("qbittorrent");
        assert_eq!(b.provider(), "qbittorrent");
    }
}
