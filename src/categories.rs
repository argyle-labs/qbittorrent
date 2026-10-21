use std::collections::BTreeMap;

use plugin_toolkit::prelude::*;

use crate::listen_port::WebUi;
use crate::temp_path::{normalize, resolve, Category, TempPathPrefs};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Desired {
    pub name: String,
    pub save_path: String,
}

pub fn parse_desired(specs: &[String]) -> Result<Vec<Desired>> {
    let mut out: Vec<Desired> = Vec::new();
    for spec in specs {
        let Some((name, save_path)) = spec.split_once('=') else {
            bail!("category must be name=save_path: {spec}");
        };
        let (name, save_path) = (name.trim(), save_path.trim());
        if name.is_empty() || save_path.is_empty() {
            bail!("category must be name=save_path: {spec}");
        }
        if out.iter().any(|d| d.name == name) {
            bail!("category {name} given twice");
        }
        out.push(Desired {
            name: name.to_string(),
            save_path: save_path.to_string(),
        });
    }
    Ok(out)
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryDrift {
    pub name: String,
    pub want_save_path: String,
    pub have_save_path: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoriesStatus {
    pub missing: Vec<CategoryDrift>,
    pub mismatched: Vec<CategoryDrift>,
    pub unmanaged: Vec<String>,
    pub shared_save_paths: Vec<String>,
    pub ok: bool,
}

fn same_path(a: &str, b: &str, base: &str) -> bool {
    match (normalize(&resolve(a, base)), normalize(&resolve(b, base))) {
        (Some(a), Some(b)) => a == b,
        _ => a == b,
    }
}

pub fn assess(
    desired: &[Desired],
    have: &BTreeMap<String, Category>,
    default_save_path: &str,
) -> CategoriesStatus {
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    for d in desired {
        match have.get(&d.name) {
            None => missing.push(CategoryDrift {
                name: d.name.clone(),
                want_save_path: d.save_path.clone(),
                have_save_path: None,
            }),
            Some(c) if !same_path(&c.save_path, &d.save_path, default_save_path) => mismatched
                .push(CategoryDrift {
                    name: d.name.clone(),
                    want_save_path: d.save_path.clone(),
                    have_save_path: Some(c.save_path.clone()),
                }),
            Some(_) => {}
        }
    }
    let unmanaged: Vec<String> = have
        .keys()
        .filter(|n| !desired.iter().any(|d| &d.name == *n))
        .cloned()
        .collect();
    let mut effective: Vec<(String, String)> = desired
        .iter()
        .map(|d| (d.name.clone(), d.save_path.clone()))
        .collect();
    effective.extend(
        unmanaged
            .iter()
            .map(|n| (n.clone(), have[n].save_path.clone())),
    );
    let mut shared_save_paths = Vec::new();
    for (i, (a, pa)) in effective.iter().enumerate() {
        for (b, pb) in &effective[i + 1..] {
            if !pa.is_empty() && !pb.is_empty() && same_path(pa, pb, default_save_path) {
                shared_save_paths.push(format!(
                    "categories '{a}' and '{b}' share save path {}",
                    resolve(pa, default_save_path)
                ));
            }
        }
    }
    CategoriesStatus {
        ok: missing.is_empty() && mismatched.is_empty() && shared_save_paths.is_empty(),
        missing,
        mismatched,
        unmanaged,
        shared_save_paths,
    }
}

async fn read(ui: &WebUi, desired: &[Desired]) -> Result<CategoriesStatus> {
    let prefs: TempPathPrefs = ui.get("/api/v2/app/preferences").await?;
    let have: BTreeMap<String, Category> = ui.get("/api/v2/torrents/categories").await?;
    Ok(assess(desired, &have, &prefs.save_path))
}

pub async fn status(ui: &WebUi, desired: &[Desired]) -> Result<CategoriesStatus> {
    read(ui, desired).await
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoriesSync {
    pub before: CategoriesStatus,
    pub created: Vec<String>,
    pub edited: Vec<String>,
    pub dry_run: bool,
}

async fn write(ui: &WebUi, path: &str, drift: &CategoryDrift) -> Result<()> {
    ui.post_form(
        path,
        vec![
            ("category".to_string(), drift.name.clone()),
            ("savePath".to_string(), drift.want_save_path.clone()),
        ],
    )
    .await
}

pub async fn sync(ui: &WebUi, desired: &[Desired], dry_run: bool) -> Result<CategoriesSync> {
    let before = read(ui, desired).await?;
    let mut created = Vec::new();
    let mut edited = Vec::new();
    if !dry_run {
        for d in &before.missing {
            write(ui, "/api/v2/torrents/createCategory", d).await?;
            created.push(d.name.clone());
        }
        for d in &before.mismatched {
            write(ui, "/api/v2/torrents/editCategory", d).await?;
            edited.push(d.name.clone());
        }
        let after = read(ui, desired).await?;
        if !after.missing.is_empty() || !after.mismatched.is_empty() {
            bail!(
                "qbittorrent did not take categories: missing={:?} mismatched={:?}",
                after.missing.iter().map(|d| &d.name).collect::<Vec<_>>(),
                after.mismatched.iter().map(|d| &d.name).collect::<Vec<_>>()
            );
        }
    }
    Ok(CategoriesSync {
        before,
        created,
        edited,
        dry_run,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::{self, json};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn desired() -> Vec<Desired> {
        parse_desired(&[
            "radarr=/data/movies".into(),
            "radarr-4k=/data/movies-4k".into(),
            "sonarr=tv".into(),
        ])
        .unwrap()
    }

    fn have(v: serde_json::Value) -> BTreeMap<String, Category> {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn parse_rejects_malformed_and_duplicate_specs() {
        assert!(parse_desired(&["radarr".into()]).is_err());
        assert!(parse_desired(&["=/x".into()]).is_err());
        assert!(parse_desired(&["a=/x".into(), "a=/y".into()]).is_err());
    }

    #[test]
    fn reports_missing_mismatched_unmanaged_and_shared() {
        let s = assess(
            &desired(),
            &have(json!({
                "radarr": {"savePath": "/data/movies/"},
                "sonarr": {"savePath": "/data/series"},
                "lidarr": {"savePath": "/data/movies"}
            })),
            "/data",
        );
        assert_eq!(s.missing.len(), 1);
        assert_eq!(s.missing[0].name, "radarr-4k");
        assert_eq!(s.mismatched.len(), 1);
        assert_eq!(
            s.mismatched[0].have_save_path.as_deref(),
            Some("/data/series")
        );
        assert_eq!(s.unmanaged, vec!["lidarr".to_string()]);
        assert_eq!(s.shared_save_paths.len(), 1, "{:?}", s.shared_save_paths);
        assert!(s.shared_save_paths[0].contains("'radarr' and 'lidarr'"));
        assert!(!s.ok);
    }

    #[test]
    fn relative_save_paths_compare_against_the_default() {
        let s = assess(
            &desired(),
            &have(json!({
                "radarr": {"savePath": "/data/movies"},
                "radarr-4k": {"savePath": "/data/movies-4k"},
                "sonarr": {"savePath": "/data/tv"}
            })),
            "/data",
        );
        assert!(s.ok, "{s:?}");
    }

    async fn base_server() -> MockServer {
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
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"save_path": "/data"})))
            .mount(&server)
            .await;
        server
    }

    async fn server(cats: serde_json::Value) -> MockServer {
        let server = base_server().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(cats))
            .mount(&server)
            .await;
        server
    }

    async fn mount_writes(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/createCategory"))
            .and(body_string_contains("category=radarr"))
            .respond_with(ResponseTemplate::new(200))
            .expect(2)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/editCategory"))
            .and(body_string_contains("category=sonarr"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn dry_run_writes_nothing() {
        let server = server(json!({})).await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/createCategory"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
        let s = sync(&ui, &desired(), true).await.unwrap();
        assert_eq!(s.before.missing.len(), 3);
        assert!(s.created.is_empty());
    }

    #[tokio::test]
    async fn execute_creates_and_edits_then_verifies() {
        let server = base_server().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/categories"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"sonarr": {"savePath": "/data/series"}})),
            )
            .up_to_n_times(1)
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/torrents/categories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "radarr": {"savePath": "/data/movies"},
                "radarr-4k": {"savePath": "/data/movies-4k"},
                "sonarr": {"savePath": "/data/tv"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        mount_writes(&server).await;
        let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
        let s = sync(&ui, &desired(), false).await.unwrap();
        assert_eq!(
            s.created,
            vec!["radarr".to_string(), "radarr-4k".to_string()]
        );
        assert_eq!(s.edited, vec!["sonarr".to_string()]);
        assert_eq!(s.before.missing.len(), 2);
        assert!(!s.dry_run);
    }

    #[tokio::test]
    async fn execute_fails_when_read_back_still_drifts() {
        let server = server(json!({"sonarr": {"savePath": "/data/series"}})).await;
        mount_writes(&server).await;
        let ui = WebUi::login(&server.uri(), "admin", "pw").await.unwrap();
        let err = sync(&ui, &desired(), false).await.unwrap_err();
        assert!(err.to_string().contains("did not take"), "{err}");
    }
}
