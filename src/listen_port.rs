//! BitTorrent listen and announce ports over the qBittorrent WebUI API
//! (`/api/v2`).
//!
//! The target port is supplied by the caller. Behind a fixed NAT mapping such
//! as `PIA-PF-port -> host:6881` the listen port stays `6881` and the PIA port
//! goes to `announce_port`, the port reported to trackers ([`Mode::Announce`]).
//! When inbound traffic reaches the client unmapped, the PIA port is the
//! listen port itself ([`Mode::Listen`]).

use plugin_toolkit::http::Client as HttpClient;
use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json;

/// An authenticated WebUI session.
pub struct WebUi {
    http: HttpClient,
    base: String,
    cookie: Option<String>,
}

/// Which preference carries the target port.
#[orca_struct(args)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// `listen_port`: the port qBittorrent binds.
    Listen,
    /// `announce_port`: the port reported to trackers; the listen port is left
    /// alone.
    #[default]
    Announce,
}

#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Preferences {
    pub listen_port: u16,
    /// qBittorrent picks a fresh port at every start when set, discarding
    /// `listen_port`.
    #[serde(default)]
    pub random_port: bool,
    #[serde(default)]
    pub upnp: bool,
    /// Absent before qBittorrent 5.1. `0` reports the listen port.
    #[serde(default)]
    pub announce_port: Option<u16>,
}

impl Preferences {
    /// The port trackers are told, or `None` when the client has no
    /// `announce_port` preference.
    pub fn reported_port(&self) -> Option<u16> {
        self.announce_port
            .map(|p| if p == 0 { self.listen_port } else { p })
    }
}

#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct TransferInfo {
    connection_status: String,
}

impl WebUi {
    pub async fn login(base_url: &str, username: &str, password: &str) -> Result<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        let http = HttpClient::new();
        // qBittorrent's CSRF check rejects requests whose Referer/Origin does
        // not match the WebUI host.
        let resp = http
            .post(format!("{base}/api/v2/auth/login"))
            .header("Referer", base.clone())
            .form(vec![
                ("username".to_string(), username.to_string()),
                ("password".to_string(), password.to_string()),
            ])
            .send()
            .await
            .map_err(|e| anyhow!("qbittorrent login: {e}"))?;
        if resp.text().trim() != "Ok." {
            bail!("qbittorrent login rejected: check the endpoint's username/password");
        }
        // No cookie means the WebUI bypasses auth for this client's subnet.
        let cookie = resp
            .headers
            .get("set-cookie")
            .and_then(|c| c.split(';').next())
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        Ok(Self { http, base, cookie })
    }

    fn request(
        &self,
        req: plugin_toolkit::http::RequestBuilder,
    ) -> plugin_toolkit::http::RequestBuilder {
        let req = req.header("Referer", self.base.clone());
        match &self.cookie {
            Some(c) => req.header("Cookie", c.clone()),
            None => req,
        }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self
            .request(self.http.get(format!("{}{path}", self.base)))
            .send()
            .await
            .map_err(|e| anyhow!("GET {path}: {e}"))?;
        resp.json().map_err(|e| anyhow!("decode {path}: {e}"))
    }

    pub async fn preferences(&self) -> Result<Preferences> {
        self.get("/api/v2/app/preferences").await
    }

    /// `connected`, `firewalled` (no inbound peers reached it), or `disconnected`.
    pub async fn connection_status(&self) -> Result<String> {
        let info: TransferInfo = self.get("/api/v2/transfer/info").await?;
        Ok(info.connection_status)
    }

    pub async fn set_port(&self, mode: Mode, port: u16) -> Result<()> {
        let prefs = match mode {
            Mode::Listen => serde_json::json!({ "listen_port": port, "random_port": false }),
            Mode::Announce => serde_json::json!({ "announce_port": port }),
        };
        self.request(
            self.http
                .post(format!("{}/api/v2/app/setPreferences", self.base))
                .form(vec![("json".to_string(), prefs.to_string())]),
        )
        .send()
        .await
        .map_err(|e| anyhow!("setPreferences: {e}"))?;
        Ok(())
    }
}

/// The port to converge on: exactly one of an explicit port or a file holding
/// it (gluetun-style `forwarded_port`, read on the host running this plugin).
pub fn target_port(port: Option<u16>, port_file: Option<&str>) -> Result<u16> {
    match (port, port_file) {
        (Some(p), None) => valid_port(p).ok_or_else(|| anyhow!("port must be 1-65535")),
        (None, Some(path)) => {
            let raw =
                std::fs::read_to_string(path).with_context(|| format!("read port file {path}"))?;
            raw.trim()
                .parse::<u16>()
                .ok()
                .and_then(valid_port)
                .ok_or_else(|| anyhow!("port file {path} does not hold a port number"))
        }
        (Some(_), Some(_)) => bail!("pass either port or port_file, not both"),
        (None, None) => bail!("pass the target port, or a port_file holding it"),
    }
}

fn valid_port(p: u16) -> Option<u16> {
    (p != 0).then_some(p)
}

const NO_ANNOUNCE_PORT: &str = "this qBittorrent has no announce_port preference (added in 5.1); \
     upgrade it, or use mode=listen with a NAT that forwards the PIA port unmapped";

/// `random_port` counts as drift in both modes: it replaces the listen port on
/// the next restart, which also moves the NAT target.
pub fn in_sync(prefs: &Preferences, mode: Mode, target: u16) -> Result<bool> {
    let port_ok = match mode {
        Mode::Listen => prefs.listen_port == target,
        Mode::Announce => {
            prefs
                .reported_port()
                .ok_or_else(|| anyhow!(NO_ANNOUNCE_PORT))?
                == target
        }
    };
    Ok(port_ok && !prefs.random_port)
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPortStatus {
    pub mode: Mode,
    pub listen_port: u16,
    /// Absent before qBittorrent 5.1. `0` reports the listen port.
    pub announce_port: Option<u16>,
    pub random_port: bool,
    pub upnp: bool,
    /// `connected`, `firewalled` (no inbound peers reached it), or `disconnected`.
    pub connection_status: String,
    pub expected_port: Option<u16>,
    /// Whether the selected mode's port equals `expected_port` with
    /// `random_port` off. Absent without `expected_port`, or in announce mode
    /// on a client without `announce_port`.
    pub matches: Option<bool>,
}

pub async fn status(
    ui: &WebUi,
    mode: Mode,
    expected_port: Option<u16>,
) -> Result<ListenPortStatus> {
    let prefs = ui.preferences().await?;
    let connection_status = ui.connection_status().await?;
    Ok(ListenPortStatus {
        matches: expected_port.and_then(|p| in_sync(&prefs, mode, p).ok()),
        mode,
        listen_port: prefs.listen_port,
        announce_port: prefs.announce_port,
        random_port: prefs.random_port,
        upnp: prefs.upnp,
        connection_status,
        expected_port,
    })
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPortSync {
    pub mode: Mode,
    pub target_port: u16,
    /// Listen port before this call.
    pub listen_port: u16,
    /// Announce port before this call.
    pub announce_port: Option<u16>,
    pub random_port: bool,
    pub in_sync: bool,
    /// True only when this call wrote the port; a dry run never does.
    pub changed: bool,
    pub dry_run: bool,
}

pub async fn sync(ui: &WebUi, mode: Mode, target: u16, execute: bool) -> Result<ListenPortSync> {
    let prefs = ui.preferences().await?;
    let already = in_sync(&prefs, mode, target)?;
    let changed = execute && !already;
    if changed {
        // With random_port on the WebUI reports listen_port 0, so there is no
        // fixed port to keep while turning it off.
        if mode == Mode::Announce && prefs.random_port {
            bail!(
                "random_port is on, so the listen port is not fixed; run mode=listen with \
                 the NAT's internal port first"
            );
        }
        ui.set_port(mode, target).await?;
        let after = ui.preferences().await?;
        if !in_sync(&after, mode, target)? {
            bail!(
                "qbittorrent did not take {mode:?} port {target}: listen_port={} \
                 announce_port={:?} random_port={}",
                after.listen_port,
                after.announce_port,
                after.random_port
            );
        }
    }
    Ok(ListenPortSync {
        mode,
        target_port: target,
        listen_port: prefs.listen_port,
        announce_port: prefs.announce_port,
        random_port: prefs.random_port,
        in_sync: already,
        changed,
        dry_run: !execute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::Value;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SID: &str = "SID=abc123";

    fn prefs(listen_port: u16, random_port: bool, announce_port: Option<u16>) -> Value {
        let mut p = serde_json::json!({
            "listen_port": listen_port, "random_port": random_port, "upnp": false
        });
        if let Some(a) = announce_port {
            p["announce_port"] = a.into();
        }
        p
    }

    async fn server(prefs: Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .and(body_string_contains("username=admin"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", format!("{SID}; HttpOnly; path=/"))
                    .set_body_string("Ok."),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/preferences"))
            .and(header("cookie", SID))
            .respond_with(ResponseTemplate::new(200).set_body_json(prefs))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/transfer/info"))
            .and(header("cookie", SID))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"connection_status": "firewalled"})),
            )
            .mount(&server)
            .await;
        server
    }

    /// Answers the first preferences read with `before`; the server's default
    /// mock answers the re-read after the write.
    async fn first_read(server: &MockServer, before: Value) {
        Mock::given(method("GET"))
            .and(path("/api/v2/app/preferences"))
            .respond_with(ResponseTemplate::new(200).set_body_json(before))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(server)
            .await;
    }

    async fn accept_writes(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v2/app/setPreferences"))
            .and(header("cookie", SID))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    async fn login(server: &MockServer) -> WebUi {
        WebUi::login(&format!("{}/", server.uri()), "admin", "pw")
            .await
            .unwrap()
    }

    async fn writes(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.url.path() == "/api/v2/app/setPreferences")
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect()
    }

    #[tokio::test]
    async fn status_reports_both_ports_reachability_and_match() {
        let server = server(prefs(6881, false, Some(51234))).await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Listen, Some(51234)).await.unwrap();
        assert_eq!(s.listen_port, 6881);
        assert_eq!(s.announce_port, Some(51234));
        assert_eq!(s.connection_status, "firewalled");
        assert_eq!(s.matches, Some(false));
        let s = status(&ui, Mode::Announce, Some(51234)).await.unwrap();
        assert_eq!(s.matches, Some(true));
        let s = status(&ui, Mode::Announce, None).await.unwrap();
        assert_eq!(s.matches, None);
    }

    #[tokio::test]
    async fn status_without_announce_port_has_no_announce_match() {
        let server = server(prefs(6881, false, None)).await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Announce, Some(6881)).await.unwrap();
        assert_eq!(s.announce_port, None);
        assert_eq!(s.matches, None);
    }

    #[tokio::test]
    async fn listen_dry_run_reports_drift_and_writes_nothing() {
        let server = server(prefs(6881, false, None)).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Listen, 51234, false).await.unwrap();
        assert!(!s.in_sync && !s.changed && s.dry_run);
        assert_eq!(s.listen_port, 6881);
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn listen_execute_in_sync_writes_nothing() {
        let server = server(prefs(51234, false, None)).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Listen, 51234, true).await.unwrap();
        assert!(s.in_sync && !s.changed && !s.dry_run);
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn listen_execute_writes_port_and_verifies_it_took() {
        let server = server(prefs(51234, false, None)).await;
        first_read(&server, prefs(6881, false, None)).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Listen, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed && !s.dry_run);
        assert_eq!(s.listen_port, 6881);
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22listen_port%22%3A51234"), "{}", w[0]);
        assert!(w[0].contains("%22random_port%22%3Afalse"), "{}", w[0]);
    }

    #[tokio::test]
    async fn listen_execute_errors_when_the_write_does_not_take() {
        let server = server(prefs(6881, false, None)).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let err = sync(&ui, Mode::Listen, 51234, true).await.unwrap_err();
        assert!(
            err.to_string().contains("did not take Listen port 51234"),
            "{err}"
        );
        assert_eq!(writes(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn random_port_is_drift_even_when_port_matches() {
        let server = server(prefs(51234, true, Some(51234))).await;
        let ui = login(&server).await;
        assert!(!sync(&ui, Mode::Listen, 51234, false).await.unwrap().in_sync);
        assert!(
            !sync(&ui, Mode::Announce, 51234, false)
                .await
                .unwrap()
                .in_sync
        );
    }

    #[tokio::test]
    async fn announce_dry_run_reports_drift_and_writes_nothing() {
        let server = server(prefs(6881, false, Some(0))).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Announce, 51234, false).await.unwrap();
        assert!(!s.in_sync && !s.changed && s.dry_run);
        assert_eq!(s.announce_port, Some(0));
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn announce_zero_reports_the_listen_port() {
        let server = server(prefs(6881, false, Some(0))).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Announce, 6881, true).await.unwrap();
        assert!(s.in_sync && !s.changed);
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn announce_execute_writes_only_announce_port_and_verifies_it() {
        let server = server(prefs(6881, false, Some(51234))).await;
        first_read(&server, prefs(6881, false, Some(0))).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Announce, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed && !s.dry_run);
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22announce_port%22%3A51234"), "{}", w[0]);
        assert!(!w[0].contains("listen_port"), "{}", w[0]);
    }

    #[tokio::test]
    async fn announce_execute_errors_when_the_write_does_not_take() {
        let server = server(prefs(6881, false, Some(0))).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let err = sync(&ui, Mode::Announce, 51234, true).await.unwrap_err();
        assert!(
            err.to_string().contains("did not take Announce port 51234"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn announce_refuses_without_the_preference() {
        let server = server(prefs(6881, false, None)).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        for execute in [false, true] {
            let err = sync(&ui, Mode::Announce, 51234, execute).await.unwrap_err();
            assert!(
                err.to_string().contains("no announce_port preference"),
                "{err}"
            );
        }
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn announce_execute_refuses_with_random_port_on() {
        let server = server(prefs(0, true, Some(0))).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let err = sync(&ui, Mode::Announce, 51234, true).await.unwrap_err();
        assert!(err.to_string().contains("random_port is on"), "{err}");
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn rejected_login_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Fails."))
            .mount(&server)
            .await;
        let err = WebUi::login(&server.uri(), "admin", "bad")
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("login rejected"), "{err}");
    }

    #[test]
    fn mode_defaults_to_announce_and_parses_lowercase() {
        assert_eq!(Mode::default(), Mode::Announce);
        let m: Mode = serde_json::from_value(serde_json::json!("listen")).unwrap();
        assert_eq!(m, Mode::Listen);
    }

    #[test]
    fn target_port_takes_exactly_one_source() {
        assert_eq!(target_port(Some(51234), None).unwrap(), 51234);
        assert!(target_port(Some(0), None).is_err());
        assert!(target_port(None, None).is_err());
        assert!(target_port(Some(1), Some("/x")).is_err());
    }

    #[test]
    fn target_port_reads_a_port_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("forwarded_port");
        std::fs::write(&f, "51234\n").unwrap();
        assert_eq!(target_port(None, f.to_str()).unwrap(), 51234);
        std::fs::write(&f, "not-a-port").unwrap();
        assert!(target_port(None, f.to_str()).is_err());
        std::fs::write(&f, "0").unwrap();
        assert!(target_port(None, f.to_str()).is_err());
        assert!(target_port(None, dir.path().join("missing").to_str()).is_err());
    }
}
