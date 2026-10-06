//! BitTorrent listen port over the qBittorrent WebUI API (`/api/v2`).
//!
//! The target port is supplied by the caller: either the VPN's forwarded port
//! (when inbound traffic reaches the client unmapped) or the internal port of a
//! fixed NAT mapping such as `PIA-PF-port -> host:6881`.

use plugin_toolkit::http::Client as HttpClient;
use plugin_toolkit::prelude::*;
use plugin_toolkit::serde_json;

/// An authenticated WebUI session.
pub struct WebUi {
    http: HttpClient,
    base: String,
    cookie: Option<String>,
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

    pub async fn set_listen_port(&self, port: u16) -> Result<()> {
        let prefs = serde_json::json!({ "listen_port": port, "random_port": false });
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

/// `random_port` counts as drift: it replaces the port on the next restart.
pub fn in_sync(prefs: &Preferences, target: u16) -> bool {
    prefs.listen_port == target && !prefs.random_port
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPortStatus {
    pub listen_port: u16,
    pub random_port: bool,
    pub upnp: bool,
    /// `connected`, `firewalled` (no inbound peers reached it), or `disconnected`.
    pub connection_status: String,
    pub expected_port: Option<u16>,
    /// Whether the listen port equals `expected_port`; absent without one.
    pub matches: Option<bool>,
}

pub async fn status(ui: &WebUi, expected_port: Option<u16>) -> Result<ListenPortStatus> {
    let prefs = ui.preferences().await?;
    let connection_status = ui.connection_status().await?;
    Ok(ListenPortStatus {
        matches: expected_port.map(|p| in_sync(&prefs, p)),
        listen_port: prefs.listen_port,
        random_port: prefs.random_port,
        upnp: prefs.upnp,
        connection_status,
        expected_port,
    })
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenPortSync {
    pub target_port: u16,
    /// Listen port before this call.
    pub listen_port: u16,
    pub random_port: bool,
    pub in_sync: bool,
    /// True only when this call wrote the port; a dry run never does.
    pub changed: bool,
    pub dry_run: bool,
}

pub async fn sync(ui: &WebUi, target: u16, execute: bool) -> Result<ListenPortSync> {
    let prefs = ui.preferences().await?;
    let already = in_sync(&prefs, target);
    let changed = execute && !already;
    if changed {
        ui.set_listen_port(target).await?;
        let after = ui.preferences().await?;
        if !in_sync(&after, target) {
            bail!(
                "qbittorrent did not take listen port {target}: now {} (random_port={})",
                after.listen_port,
                after.random_port
            );
        }
    }
    Ok(ListenPortSync {
        target_port: target,
        listen_port: prefs.listen_port,
        random_port: prefs.random_port,
        in_sync: already,
        changed,
        dry_run: !execute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SID: &str = "SID=abc123";

    async fn server(listen_port: u16, random_port: bool) -> MockServer {
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
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "listen_port": listen_port, "random_port": random_port, "upnp": false
            })))
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
    async fn status_reports_port_reachability_and_match() {
        let server = server(6881, false).await;
        let ui = login(&server).await;
        let s = status(&ui, Some(51234)).await.unwrap();
        assert_eq!(s.listen_port, 6881);
        assert_eq!(s.connection_status, "firewalled");
        assert_eq!(s.matches, Some(false));
        assert_eq!(status(&ui, None).await.unwrap().matches, None);
        assert_eq!(status(&ui, Some(6881)).await.unwrap().matches, Some(true));
    }

    #[tokio::test]
    async fn dry_run_reports_drift_and_writes_nothing() {
        let server = server(6881, false).await;
        let ui = login(&server).await;
        let s = sync(&ui, 51234, false).await.unwrap();
        assert!(!s.in_sync && !s.changed && s.dry_run);
        assert_eq!(s.listen_port, 6881);
        assert!(writes(&server).await.is_empty());
    }

    #[tokio::test]
    async fn execute_in_sync_writes_nothing() {
        let server = server(51234, false).await;
        let ui = login(&server).await;
        let s = sync(&ui, 51234, true).await.unwrap();
        assert!(s.in_sync && !s.changed && !s.dry_run);
        assert!(writes(&server).await.is_empty());
    }

    async fn accept_writes(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v2/app/setPreferences"))
            .and(header("cookie", SID))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn execute_writes_port_and_verifies_it_took() {
        // The default-priority mock answers the re-read after the write.
        let server = server(51234, false).await;
        Mock::given(method("GET"))
            .and(path("/api/v2/app/preferences"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "listen_port": 6881, "random_port": false, "upnp": false
            })))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let s = sync(&ui, 51234, true).await.unwrap();
        assert!(!s.in_sync && s.changed && !s.dry_run);
        assert_eq!(s.listen_port, 6881);
        let w = writes(&server).await;
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("%22listen_port%22%3A51234"), "{}", w[0]);
        assert!(w[0].contains("%22random_port%22%3Afalse"), "{}", w[0]);
    }

    #[tokio::test]
    async fn execute_errors_when_the_write_does_not_take() {
        let server = server(6881, false).await;
        accept_writes(&server).await;
        let ui = login(&server).await;
        let err = sync(&ui, 51234, true).await.unwrap_err();
        assert!(
            err.to_string().contains("did not take listen port 51234"),
            "{err}"
        );
        assert_eq!(writes(&server).await.len(), 1);
    }

    #[tokio::test]
    async fn random_port_is_drift_even_when_port_matches() {
        let server = server(51234, true).await;
        let ui = login(&server).await;
        let s = sync(&ui, 51234, false).await.unwrap();
        assert!(!s.in_sync);
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
