//! BitTorrent listen and announce ports over the qBittorrent WebUI API
//! (`/api/v2`).
//!
//! The target port is supplied by the caller. Behind a fixed NAT mapping such
//! as `PIA-PF-port -> host:6881` the listen port stays `6881` and the PIA port
//! goes to `announce_port`, the port reported to trackers ([`Mode::Announce`]).
//! When inbound traffic reaches the client unmapped, the PIA port is the
//! listen port itself ([`Mode::Listen`]).

use plugin_toolkit::http::{Client as HttpClient, HttpError, RequestBuilder, Response};
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
    /// Deprecated; reports `listen_port == 0`, where libtorrent binds an
    /// ephemeral port.
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

#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct BuildInfo {
    libtorrent: String,
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

    fn request(&self, req: RequestBuilder) -> RequestBuilder {
        let req = req.header("Referer", self.base.clone());
        match &self.cookie {
            Some(c) => req.header("Cookie", c.clone()),
            None => req,
        }
    }

    async fn send(&self, req: RequestBuilder, method: &str, path: &str) -> Result<Response> {
        match self.request(req).send().await {
            Ok(resp) => Ok(resp),
            Err(HttpError::Status {
                status, summary, ..
            }) => bail!("{method} {path}: HTTP {status}: {summary}"),
            Err(e) => bail!("{method} {path}: {e}"),
        }
    }

    pub(crate) async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self
            .send(self.http.get(format!("{}{path}", self.base)), "GET", path)
            .await?;
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

    /// The libtorrent version qBittorrent was built against, e.g. `2.0.11.0`.
    pub async fn libtorrent_version(&self) -> Result<String> {
        let info: BuildInfo = self.get("/api/v2/app/buildInfo").await?;
        Ok(info.libtorrent)
    }

    /// In listen mode `clear_announce` also resets `announce_port` to `0`, so
    /// a stale value cannot override what trackers are told. Pass it only when
    /// the client has the preference.
    pub async fn set_port(&self, mode: Mode, port: u16, clear_announce: bool) -> Result<()> {
        let prefs = match mode {
            Mode::Listen if clear_announce => serde_json::json!({
                "listen_port": port, "random_port": false, "announce_port": 0
            }),
            Mode::Listen => serde_json::json!({ "listen_port": port, "random_port": false }),
            Mode::Announce => serde_json::json!({ "announce_port": port }),
        };
        let path = "/api/v2/app/setPreferences";
        self.send(
            self.http
                .post(format!("{}{path}", self.base))
                .form(vec![("json".to_string(), prefs.to_string())]),
            "POST",
            path,
        )
        .await?;
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

/// qBittorrent applies `announce_port` only when built against libtorrent
/// 2.0.11+; on older builds it saves and reads back but has no effect.
const MIN_ANNOUNCE_LIBTORRENT: (u32, u32, u32) = (2, 0, 11);

/// Whether a libtorrent version string (`2.0.11.0`) supports `announce_port`.
/// Unparseable versions count as unsupported.
pub fn libtorrent_supports_announce(version: &str) -> bool {
    let mut parts = version.trim().split('.').map(|p| p.parse::<u32>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c))) => (a, b, c) >= MIN_ANNOUNCE_LIBTORRENT,
        _ => false,
    }
}

const NO_ANNOUNCE_PORT: &str = "this qBittorrent has no announce_port preference (added in 5.1); \
     upgrade it, or use mode=listen with a NAT that forwards the PIA port unmapped";

const ANNOUNCE_RANDOM_PORT: &str = "random_port is on, so the listen port is not fixed; run \
     mode=listen with the NAT's internal port first";

/// Whether `announce_port` reaches trackers on this client.
fn announce_effective(prefs: &Preferences, libtorrent: &str) -> bool {
    prefs.announce_port.is_some() && libtorrent_supports_announce(libtorrent)
}

/// Why announce mode cannot converge on this client, or `None` when it can.
fn announce_blocked(prefs: &Preferences, libtorrent: &str) -> Option<String> {
    if prefs.announce_port.is_none() {
        return Some(NO_ANNOUNCE_PORT.to_string());
    }
    if !libtorrent_supports_announce(libtorrent) {
        return Some(format!(
            "this qBittorrent is built against libtorrent {libtorrent}; announce_port only takes \
             effect with libtorrent 2.0.11+ (it saves but trackers keep getting the listen port). \
             Upgrade qBittorrent, or use mode=listen with a NAT that forwards the PIA port unmapped"
        ));
    }
    prefs.random_port.then(|| ANNOUNCE_RANDOM_PORT.to_string())
}

/// How listen mode treats a leftover non-zero `announce_port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leftover {
    Drift,
    Ignore,
}

/// `random_port` counts as drift in both modes: it reports `listen_port == 0`,
/// where libtorrent binds an ephemeral port, so the NAT target is not fixed.
/// In listen mode, with [`Leftover::Drift`], a non-zero `announce_port` other
/// than the target is drift too, since trackers would be told that port instead.
fn in_sync(prefs: &Preferences, mode: Mode, target: u16, leftover: Leftover) -> bool {
    let port_ok = match mode {
        Mode::Listen => {
            prefs.listen_port == target
                && (leftover == Leftover::Ignore || leftover_announce_port(prefs, target).is_none())
        }
        Mode::Announce => prefs.reported_port() == Some(target),
    };
    port_ok && !prefs.random_port
}

/// A non-zero `announce_port` that would make trackers hear something other
/// than `listen_port`.
fn leftover_announce_port(prefs: &Preferences, listen_port: u16) -> Option<u16> {
    prefs.announce_port.filter(|&a| a != 0 && a != listen_port)
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
    /// libtorrent version qBittorrent was built against.
    pub libtorrent: String,
    /// Whether `announce_port` reaches trackers: the preference exists and
    /// libtorrent is 2.0.11+.
    pub announce_effective: bool,
    /// `connected`, `firewalled` (no inbound peers reached it), or `disconnected`.
    pub connection_status: String,
    pub expected_port: Option<u16>,
    /// Whether the selected mode's port equals `expected_port` with
    /// `random_port` off. Absent without `expected_port`, or in announce mode
    /// when `announce_effective` is false.
    pub matches: Option<bool>,
    /// Listen mode only: a non-zero `announce_port` that has no effect on this
    /// libtorrent but takes over once qBittorrent is upgraded. `matches`
    /// ignores it; `listen_port.sync --execute` clears it.
    pub leftover_announce_port: Option<u16>,
}

pub async fn status(
    ui: &WebUi,
    mode: Mode,
    expected_port: Option<u16>,
) -> Result<ListenPortStatus> {
    let prefs = ui.preferences().await?;
    let libtorrent = ui.libtorrent_version().await?;
    let connection_status = ui.connection_status().await?;
    let announce_effective = announce_effective(&prefs, &libtorrent);
    let matches = expected_port
        .filter(|_| mode == Mode::Listen || announce_effective)
        .map(|p| {
            let leftover = if announce_effective {
                Leftover::Drift
            } else {
                Leftover::Ignore
            };
            in_sync(&prefs, mode, p, leftover)
        });
    let leftover_announce_port = (mode == Mode::Listen && !announce_effective)
        .then(|| leftover_announce_port(&prefs, prefs.listen_port))
        .flatten();
    Ok(ListenPortStatus {
        matches,
        leftover_announce_port,
        mode,
        listen_port: prefs.listen_port,
        announce_port: prefs.announce_port,
        random_port: prefs.random_port,
        upnp: prefs.upnp,
        libtorrent,
        announce_effective,
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
    /// False whenever `blocked` is set.
    pub in_sync: bool,
    /// Why announce mode cannot converge: no `announce_port` preference,
    /// libtorrent before 2.0.11, or `random_port` on. A dry run reports it;
    /// `execute` fails with the same text.
    pub blocked: Option<String>,
    /// True only when this call wrote the port; a dry run never does.
    pub changed: bool,
    pub dry_run: bool,
}

pub async fn sync(ui: &WebUi, mode: Mode, target: u16, execute: bool) -> Result<ListenPortSync> {
    let prefs = ui.preferences().await?;
    let blocked = match mode {
        Mode::Listen => None,
        Mode::Announce => announce_blocked(&prefs, &ui.libtorrent_version().await?),
    };
    // Listen mode clears a leftover announce_port even where libtorrent ignores
    // it today: it takes effect once qBittorrent is upgraded.
    let already = blocked.is_none() && in_sync(&prefs, mode, target, Leftover::Drift);
    if execute {
        if let Some(reason) = &blocked {
            bail!("{reason}");
        }
    }
    let changed = execute && !already;
    if changed {
        ui.set_port(mode, target, prefs.announce_port.is_some())
            .await?;
        let after = ui.preferences().await?;
        if !in_sync(&after, mode, target, Leftover::Drift) {
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
        blocked,
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
        server_lt(prefs, "2.0.11.0").await
    }

    async fn server_lt(prefs: Value, libtorrent: &str) -> MockServer {
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
        Mock::given(method("GET"))
            .and(path("/api/v2/app/buildInfo"))
            .and(header("cookie", SID))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"qt": "6.7.3", "libtorrent": libtorrent})),
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
        assert!(!w[0].contains("announce_port"), "{}", w[0]);
    }

    #[tokio::test]
    async fn listen_stale_announce_port_is_drift_and_the_write_clears_it() {
        let server = server(prefs(51234, false, Some(0))).await;
        first_read(&server, prefs(51234, false, Some(40000))).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Listen, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed);
        assert_eq!(s.announce_port, Some(40000));
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22listen_port%22%3A51234"), "{}", w[0]);
        assert!(w[0].contains("%22announce_port%22%3A0"), "{}", w[0]);
    }

    #[tokio::test]
    async fn listen_status_flags_a_stale_announce_port() {
        let server = server(prefs(51234, false, Some(40000))).await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Listen, Some(51234)).await.unwrap();
        assert_eq!(s.matches, Some(false));
    }

    #[tokio::test]
    async fn listen_execute_turns_off_random_port() {
        let server = server(prefs(51234, false, None)).await;
        first_read(&server, prefs(0, true, None)).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = sync(&ui, Mode::Listen, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed && s.blocked.is_none());
        assert!(s.random_port);
        assert_eq!(s.listen_port, 0);
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22listen_port%22%3A51234"), "{}", w[0]);
        assert!(w[0].contains("%22random_port%22%3Afalse"), "{}", w[0]);
    }

    #[tokio::test]
    async fn set_preferences_error_status_fails_with_status_and_path() {
        let server = server(prefs(6881, false, None)).await;
        Mock::given(method("POST"))
            .and(path("/api/v2/app/setPreferences"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let ui = login(&server).await;
        let err = sync(&ui, Mode::Listen, 51234, true).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("POST /api/v2/app/setPreferences: HTTP 500"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn get_error_status_fails_with_status_and_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/auth/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Ok."))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/preferences"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let ui = login(&server).await;
        let err = ui.preferences().await.err().unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("GET /api/v2/app/preferences: HTTP 403"),
            "{msg}"
        );
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
    async fn random_port_is_drift_even_when_announce_port_matches() {
        let server = server(prefs(0, true, Some(51234))).await;
        let ui = login(&server).await;
        assert!(!sync(&ui, Mode::Listen, 51234, false).await.unwrap().in_sync);
        let s = sync(&ui, Mode::Announce, 51234, false).await.unwrap();
        assert!(!s.in_sync);
        assert!(s.blocked.is_some());
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

    /// A dry run reports the refusal in `blocked` alongside the port data;
    /// execute fails with the same text and writes nothing.
    async fn assert_announce_blocked(server: &MockServer, target: u16, want: &str) -> String {
        accept_writes(server).await;
        let ui = login(server).await;
        let s = sync(&ui, Mode::Announce, target, false).await.unwrap();
        assert!(!s.in_sync && !s.changed && s.dry_run);
        let reason = s.blocked.expect("blocked");
        assert!(reason.contains(want), "{reason}");
        let err = sync(&ui, Mode::Announce, target, true).await.unwrap_err();
        assert_eq!(err.to_string(), reason);
        assert!(writes(server).await.is_empty());
        reason
    }

    #[tokio::test]
    async fn announce_blocks_without_the_preference() {
        let server = server(prefs(6881, false, None)).await;
        assert_announce_blocked(&server, 51234, "no announce_port preference").await;
    }

    #[tokio::test]
    async fn announce_blocks_with_random_port_on() {
        let server = server(prefs(0, true, Some(0))).await;
        assert_announce_blocked(&server, 51234, "random_port is on").await;
    }

    #[tokio::test]
    async fn announce_blocks_on_libtorrent_before_2_0_11() {
        let server = server_lt(prefs(6881, false, Some(0)), "2.0.10.0").await;
        let reason = assert_announce_blocked(&server, 51234, "libtorrent 2.0.10.0").await;
        assert!(reason.contains("2.0.11+"), "{reason}");
    }

    #[tokio::test]
    async fn announce_blocked_is_never_in_sync_even_when_the_port_matches() {
        let server = server_lt(prefs(6881, false, Some(51234)), "2.0.10.0").await;
        assert_announce_blocked(&server, 51234, "libtorrent 2.0.10.0").await;
    }

    #[tokio::test]
    async fn listen_clears_an_ineffective_announce_port_and_status_flags_it() {
        let server = server_lt(prefs(51234, false, Some(0)), "2.0.10.0").await;
        // One stale read for status, one for sync; the re-read after the write
        // gets the server default.
        for _ in 0..2 {
            first_read(&server, prefs(51234, false, Some(40000))).await;
        }
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Listen, Some(51234)).await.unwrap();
        assert_eq!(s.matches, Some(true));
        assert_eq!(s.leftover_announce_port, Some(40000));
        let s = sync(&ui, Mode::Listen, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed && s.blocked.is_none());
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22announce_port%22%3A0"), "{}", w[0]);
        let s = status(&ui, Mode::Listen, Some(51234)).await.unwrap();
        assert_eq!(s.leftover_announce_port, None);
    }

    #[tokio::test]
    async fn status_on_libtorrent_2_0_10_marks_announce_ineffective() {
        let server = server_lt(prefs(6881, false, Some(51234)), "2.0.10.0").await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Announce, Some(51234)).await.unwrap();
        assert_eq!(s.libtorrent, "2.0.10.0");
        assert!(!s.announce_effective);
        assert_eq!(s.matches, None);
        let s = status(&ui, Mode::Listen, Some(6881)).await.unwrap();
        assert_eq!(
            s.matches,
            Some(true),
            "ineffective announce_port is ignored"
        );
    }

    #[tokio::test]
    async fn status_on_libtorrent_2_0_11_marks_announce_effective() {
        let server = server_lt(prefs(6881, false, Some(51234)), "2.0.11.0").await;
        let ui = login(&server).await;
        let s = status(&ui, Mode::Announce, Some(51234)).await.unwrap();
        assert_eq!(s.libtorrent, "2.0.11.0");
        assert!(s.announce_effective);
        assert_eq!(s.matches, Some(true));
    }

    #[test]
    fn libtorrent_version_gate() {
        assert!(libtorrent_supports_announce("2.0.11.0"));
        assert!(libtorrent_supports_announce("2.0.11"));
        assert!(libtorrent_supports_announce("2.1.0.0"));
        assert!(!libtorrent_supports_announce("2.0.10.0"));
        assert!(!libtorrent_supports_announce("1.2.19.0"));
        assert!(!libtorrent_supports_announce(""));
        assert!(!libtorrent_supports_announce("unknown"));
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
