//! Incomplete-download placement. In-progress torrents do many small random
//! writes; on a network share they stall or fail when the NAS is I/O-starved,
//! so they belong on a local temp dir with only completed files on the share.

use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;

use crate::listen_port::WebUi;

#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct TempPathPrefs {
    #[serde(default)]
    pub save_path: String,
    #[serde(default)]
    pub temp_path: String,
    #[serde(default)]
    pub temp_path_enabled: bool,
}

/// One entry of `/api/v2/torrents/categories`.
#[derive(Debug, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Category {
    #[serde(default, rename = "savePath")]
    pub save_path: String,
    /// Per-category incomplete path (qBittorrent 4.5+): a string overrides the
    /// global temp path, `false` disables it, absent/`null` inherits.
    #[serde(default)]
    pub download_path: Option<plugin_toolkit::serde_json::Value>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempPathStatus {
    pub save_path: String,
    pub temp_path: String,
    pub temp_path_enabled: bool,
    /// Whether incomplete downloads land on a local path for every category.
    pub ok: bool,
    pub problems: Vec<String>,
}

/// Lexical normalization: collapses `//`, `.` and `..`; never touches the fs.
/// Returns `None` for a relative path.
pub(crate) fn normalize(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// `path` equals `prefix` or sits beneath it, compared component-wise after
/// normalization. Kept in step with the copy in the sabnzbd plugin until it
/// moves to plugin-toolkit.
pub(crate) fn under(path: &str, prefix: &str) -> bool {
    match (normalize(path), normalize(prefix)) {
        (Some(p), Some(pre)) => p.starts_with(&pre),
        _ => false,
    }
}

/// `path` made absolute against `base`, as qBittorrent resolves relative
/// category paths. Left as is when `base` is relative too.
pub(crate) fn resolve(path: &str, base: &str) -> String {
    if path.starts_with('/') || !base.starts_with('/') {
        path.to_string()
    } else {
        format!("{}/{path}", base.trim_end_matches('/'))
    }
}

/// Problems with one effective incomplete path.
fn check_incomplete(
    label: &str,
    incomplete: &str,
    save_paths: &[(String, String)],
    network_prefixes: &[String],
) -> Vec<String> {
    if let Some(net) = network_prefixes.iter().find(|n| under(incomplete, n)) {
        return vec![format!("{label} {incomplete} is on network mount {net}")];
    }
    save_paths
        .iter()
        .filter(|(_, sp)| under(incomplete, sp))
        .map(|(owner, sp)| format!("{label} {incomplete} is inside {owner} save path {sp}"))
        .collect()
}

/// Paths are container-side, so the network mounts are supplied by the caller.
/// An incomplete path inside any save path is flagged regardless.
pub fn assess(
    prefs: TempPathPrefs,
    categories: &BTreeMap<String, Category>,
    network_prefixes: &[String],
) -> TempPathStatus {
    let mut save_paths = vec![("default".to_string(), prefs.save_path.clone())];
    save_paths.extend(
        categories
            .iter()
            .filter(|(_, c)| !c.save_path.is_empty())
            .map(|(n, c)| {
                (
                    format!("category '{n}'"),
                    resolve(&c.save_path, &prefs.save_path),
                )
            }),
    );
    let mut problems = Vec::new();
    if !prefs.temp_path_enabled {
        problems.push(format!(
            "temp path disabled: incomplete downloads are written straight to the save path {}",
            prefs.save_path
        ));
    } else if prefs.temp_path.trim().is_empty() {
        problems.push("temp path enabled but empty".to_string());
    } else {
        problems.extend(check_incomplete(
            "temp path",
            &prefs.temp_path,
            &save_paths,
            network_prefixes,
        ));
    }
    // Relative category download paths sit under the global temp path, which
    // qBittorrent keeps configured even while it is disabled.
    let download_base = if prefs.temp_path.trim().is_empty() {
        prefs.save_path.as_str()
    } else {
        prefs.temp_path.as_str()
    };
    for (name, cat) in categories {
        let label = format!("category '{name}' download path");
        match &cat.download_path {
            Some(plugin_toolkit::serde_json::Value::String(p)) if !p.trim().is_empty() => {
                let p = resolve(p.trim(), download_base);
                problems.extend(check_incomplete(&label, &p, &save_paths, network_prefixes));
            }
            Some(plugin_toolkit::serde_json::Value::Bool(false)) => problems.push(format!(
                "{label} disabled: incomplete downloads go straight to {}",
                if cat.save_path.is_empty() {
                    &prefs.save_path
                } else {
                    &cat.save_path
                }
            )),
            _ => {}
        }
    }
    TempPathStatus {
        ok: problems.is_empty(),
        problems,
        save_path: prefs.save_path,
        temp_path: prefs.temp_path,
        temp_path_enabled: prefs.temp_path_enabled,
    }
}

pub async fn status(ui: &WebUi, network_prefixes: &[String]) -> Result<TempPathStatus> {
    let prefs: TempPathPrefs = ui.get("/api/v2/app/preferences").await?;
    let categories: BTreeMap<String, Category> = ui.get("/api/v2/torrents/categories").await?;
    Ok(assess(prefs, &categories, network_prefixes))
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempPathSync {
    pub target: String,
    /// Global temp path before this call.
    pub temp_path: String,
    pub temp_path_enabled: bool,
    pub in_sync: bool,
    /// True only when this call wrote preferences; a dry run never does.
    pub changed: bool,
    pub dry_run: bool,
}

/// Point the global incomplete path at `target` and enable it. Refuses a target
/// on a network mount or inside a save path, the conditions `status` flags.
/// Per-category download paths are left as configured.
pub async fn sync(
    ui: &WebUi,
    target: &str,
    network_prefixes: &[String],
    dry_run: bool,
) -> Result<TempPathSync> {
    let Some(want) = normalize(target) else {
        bail!("temp path must be absolute: {target}");
    };
    let prefs: TempPathPrefs = ui.get("/api/v2/app/preferences").await?;
    let categories: BTreeMap<String, Category> = ui.get("/api/v2/torrents/categories").await?;
    let mut save_paths = vec![("default".to_string(), prefs.save_path.clone())];
    save_paths.extend(
        categories
            .iter()
            .filter(|(_, c)| !c.save_path.is_empty())
            .map(|(n, c)| {
                (
                    format!("category '{n}'"),
                    resolve(&c.save_path, &prefs.save_path),
                )
            }),
    );
    if let Some(problem) = check_incomplete("target", target, &save_paths, network_prefixes)
        .into_iter()
        .next()
    {
        bail!("{problem}");
    }
    let in_sync = prefs.temp_path_enabled && normalize(&prefs.temp_path) == Some(want);
    let changed = !dry_run && !in_sync;
    if changed {
        ui.set_preferences(&plugin_toolkit::serde_json::json!({
            "temp_path": target, "temp_path_enabled": true
        }))
        .await?;
        let after: TempPathPrefs = ui.get("/api/v2/app/preferences").await?;
        if !after.temp_path_enabled || normalize(&after.temp_path) != normalize(target) {
            bail!(
                "qbittorrent did not take temp path {target}: temp_path={} temp_path_enabled={}",
                after.temp_path,
                after.temp_path_enabled
            );
        }
    }
    Ok(TempPathSync {
        target: target.to_string(),
        temp_path: prefs.temp_path,
        temp_path_enabled: prefs.temp_path_enabled,
        in_sync,
        changed,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::{self, json};

    fn prefs(save: &str, temp: &str, enabled: bool) -> TempPathPrefs {
        TempPathPrefs {
            save_path: save.into(),
            temp_path: temp.into(),
            temp_path_enabled: enabled,
        }
    }

    fn cats(v: serde_json::Value) -> BTreeMap<String, Category> {
        serde_json::from_value(v).unwrap()
    }

    fn none() -> BTreeMap<String, Category> {
        BTreeMap::new()
    }

    #[test]
    fn local_temp_path_is_ok() {
        let s = assess(
            prefs("/data/torrents", "/incomplete", true),
            &none(),
            &["/data".into()],
        );
        assert!(s.ok, "{:?}", s.problems);
    }

    #[test]
    fn disabled_temp_path_is_flagged() {
        assert!(!assess(prefs("/data", "/incomplete", false), &none(), &[]).ok);
    }

    #[test]
    fn temp_path_on_network_mount_is_flagged() {
        let s = assess(
            prefs("/media", "/downloads/incomplete/", true),
            &none(),
            &["/downloads/".into()],
        );
        assert!(s.problems[0].contains("network mount /downloads/"));
    }

    #[test]
    fn temp_path_inside_save_path_is_flagged() {
        assert!(
            !assess(
                prefs("/data/torrents", "/data/torrents/temp", true),
                &none(),
                &[]
            )
            .ok
        );
    }

    #[test]
    fn paths_are_compared_lexically_normalized() {
        assert!(under("/data//x/./y", "/data/x"));
        assert!(under("/local/../data/inc", "/data"));
        assert!(!under("/datastore/inc", "/data"));
        assert!(under("/anything", "/"));
        assert!(!under("relative/inc", "/"));
        let s = assess(prefs("/media", "/incomplete", true), &none(), &["/".into()]);
        assert!(!s.ok);
    }

    #[test]
    fn temp_path_inside_a_category_save_path_is_flagged() {
        let c = cats(json!({"radarr": {"name": "radarr", "savePath": "/incomplete/movies"}}));
        let s = assess(prefs("/data", "/incomplete/movies/tmp", true), &c, &[]);
        assert!(
            s.problems[0].contains("category 'radarr' save path"),
            "{:?}",
            s.problems
        );
    }

    #[test]
    fn category_download_path_overrides_are_checked() {
        let c = cats(json!({
            "sonarr": {"name": "sonarr", "savePath": "/data/tv", "download_path": "/data/tv/.inc"},
            "lidarr": {"name": "lidarr", "savePath": "", "download_path": false},
            "radarr": {"name": "radarr", "savePath": "/data/movies", "download_path": null},
            "ok":     {"name": "ok", "savePath": "/data/ok", "download_path": "/incomplete/ok"}
        }));
        let s = assess(prefs("/data", "/incomplete", true), &c, &["/data".into()]);
        assert_eq!(s.problems.len(), 2, "{:?}", s.problems);
        assert!(s
            .problems
            .iter()
            .any(|p| p.starts_with("category 'sonarr' download path")));
        assert!(s
            .problems
            .iter()
            .any(|p| p.contains("'lidarr' download path disabled")));
    }

    #[test]
    fn relative_category_paths_resolve_against_their_bases() {
        let c = cats(json!({
            "tv": {"name": "tv", "savePath": "tv", "download_path": "tv-inc"},
            "nested": {"name": "nested", "savePath": "/local/x", "download_path": "../data/tv/inc"}
        }));
        let s = assess(prefs("/data", "/local/inc", true), &c, &["/data".into()]);
        // tv-inc -> /local/inc/tv-inc (local); ../data/tv/inc -> /local/data/tv/inc (local).
        assert!(s.ok, "{:?}", s.problems);
        let s = assess(prefs("/data", "/data/inc", true), &c, &[]);
        // Temp inside the default save path, and tv-inc -> /data/inc/tv-inc inside it too.
        assert!(
            s.problems
                .iter()
                .any(|p| p.contains("category 'tv' download path /data/inc/tv-inc")),
            "{:?}",
            s.problems
        );
        let s = assess(
            prefs("/data", "/incomplete", true),
            &cats(json!({"tv": {"name": "tv", "savePath": "tv"}})),
            &[],
        );
        assert!(s.ok);
        let s = assess(
            prefs("/data", "/data/tv/inc", true),
            &cats(json!({"tv": {"name": "tv", "savePath": "tv"}})),
            &[],
        );
        assert!(
            s.problems
                .iter()
                .any(|p| p.contains("category 'tv' save path /data/tv")),
            "{:?}",
            s.problems
        );
    }

    #[test]
    fn global_temp_disabled_with_a_category_download_path() {
        let c = cats(json!({
            "tv": {"name": "tv", "savePath": "/data/tv", "download_path": "/incomplete/tv"},
            "movies": {"name": "movies", "savePath": "/data/movies"}
        }));
        let s = assess(prefs("/data", "", false), &c, &["/data".into()]);
        // The category's own local path is fine; categories inheriting the
        // disabled global setting are covered by the one global problem.
        assert_eq!(s.problems.len(), 1, "{:?}", s.problems);
        assert!(s.problems[0].starts_with("temp path disabled"));
        let s = assess(
            prefs("/data", "", false),
            &cats(json!({"tv": {"name": "tv", "savePath": "/data/tv", "download_path": "inc"}})),
            &[],
        );
        // Relative to the save path when no temp path is configured.
        assert!(
            s.problems
                .iter()
                .any(|p| p.contains("download path /data/inc")),
            "{:?}",
            s.problems
        );
    }

    mod sync {
        use super::super::*;
        use plugin_toolkit::serde_json::json;
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        async fn server(prefs: serde_json::Value) -> MockServer {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v2/auth/login"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("set-cookie", "SID=t; HttpOnly; path=/")
                        .set_body_string("Ok."),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v2/app/preferences"))
                .respond_with(ResponseTemplate::new(200).set_body_json(prefs))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v2/torrents/categories"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
                .mount(&server)
                .await;
            server
        }

        use plugin_toolkit::serde_json;

        #[tokio::test]
        async fn dry_run_reports_drift_without_writing() {
            let server = server(
                json!({"save_path": "/downloads", "temp_path": "", "temp_path_enabled": false}),
            )
            .await;
            Mock::given(method("POST"))
                .and(path("/api/v2/app/setPreferences"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&server)
                .await;
            let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
            let s = sync(&ui, "/incomplete", &["/downloads".into()], true)
                .await
                .unwrap();
            assert!(!s.in_sync && !s.changed && s.dry_run);
        }

        #[tokio::test]
        async fn in_sync_target_writes_nothing() {
            let server = server(json!({"save_path": "/downloads", "temp_path": "/incomplete/", "temp_path_enabled": true})).await;
            Mock::given(method("POST"))
                .and(path("/api/v2/app/setPreferences"))
                .respond_with(ResponseTemplate::new(200))
                .expect(0)
                .mount(&server)
                .await;
            let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
            let s = sync(&ui, "/incomplete", &[], false).await.unwrap();
            assert!(s.in_sync && !s.changed);
        }

        #[tokio::test]
        async fn execute_fails_when_the_write_does_not_take() {
            let server = server(
                json!({"save_path": "/downloads", "temp_path": "", "temp_path_enabled": false}),
            )
            .await;
            Mock::given(method("POST"))
                .and(path("/api/v2/app/setPreferences"))
                .and(body_string_contains("temp_path_enabled"))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
            let err = sync(&ui, "/incomplete", &[], false).await.unwrap_err();
            assert!(err.to_string().contains("did not take"), "{err}");
        }

        #[tokio::test]
        async fn refuses_a_network_or_save_path_target() {
            let server = server(
                json!({"save_path": "/downloads", "temp_path": "", "temp_path_enabled": false}),
            )
            .await;
            let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
            let err = sync(&ui, "/mnt/share/inc", &["/mnt/share".into()], true)
                .await
                .unwrap_err();
            assert!(err.to_string().contains("network mount"), "{err}");
            let err = sync(&ui, "/downloads/inc", &[], true).await.unwrap_err();
            assert!(
                err.to_string().contains("inside default save path"),
                "{err}"
            );
            assert!(sync(&ui, "inc", &[], true).await.is_err());
        }
    }
}
