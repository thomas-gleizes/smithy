//! Listes de dépôts de l'accueil (les miens, suivis, étoilés, recherche) et fiche d'un dépôt.
//! Même forme sur Forgejo 10 à 15 (sonde `dev/dev.sh probe-browse`) : une liste JSON, sauf la
//! recherche, qui répond `{"ok": true, "data": [...]}`. Une liste vide peut être `null`.

use serde::Deserialize;
use smithy_core::{Repo, RepoInfo, RepoSource, Result};

use crate::{Forgejo, MAX_PAGES, PAGE_SIZE};

#[derive(Deserialize)]
pub(crate) struct ApiRepo {
    name: String,
    owner: ApiOwner,
    #[serde(default)]
    description: String,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    empty: bool,
    #[serde(default)]
    stars_count: u64,
    #[serde(default)]
    open_pr_counter: u64,
    #[serde(default)]
    open_issues_count: u64,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    default_branch: String,
}

#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}

#[derive(Deserialize)]
struct ApiSearch {
    #[serde(default)]
    data: Option<Vec<ApiRepo>>,
}

impl From<ApiRepo> for RepoInfo {
    fn from(r: ApiRepo) -> Self {
        RepoInfo {
            repo: Repo {
                owner: r.owner.login,
                name: r.name,
            },
            description: r.description,
            private: r.private,
            fork: r.fork,
            archived: r.archived,
            empty: r.empty,
            stars: r.stars_count,
            open_prs: r.open_pr_counter,
            open_issues: r.open_issues_count,
            updated: Some(r.updated_at).filter(|s| !s.is_empty()),
            default_branch: r.default_branch,
        }
    }
}

impl Forgejo {
    pub(crate) async fn fetch_repos(&self, source: &RepoSource) -> Result<Vec<RepoInfo>> {
        let path = match source {
            RepoSource::Mine => "/user/repos",
            RepoSource::Watched => "/user/subscriptions",
            RepoSource::Starred => "/user/starred",
            RepoSource::Search(q) => return self.search_repos(q).await,
        };
        let mut repos = Vec::new();
        for page in 1..=MAX_PAGES {
            // `null` pour une liste vide : mesuré sur certaines versions, toléré partout.
            let batch = self
                .get::<Option<Vec<ApiRepo>>>(
                    path,
                    &[("limit", PAGE_SIZE.to_string()), ("page", page.to_string())],
                )
                .await?
                .unwrap_or_default();
            let n = batch.len();
            repos.extend(batch.into_iter().map(RepoInfo::from));
            if n < PAGE_SIZE {
                break;
            }
        }
        Ok(repos)
    }

    /// Une seule page : une recherche trop large se précise, elle ne se pagine pas.
    async fn search_repos(&self, query: &str) -> Result<Vec<RepoInfo>> {
        let found: ApiSearch = self
            .get(
                "/repos/search",
                &[
                    ("q", query.to_owned()),
                    ("limit", PAGE_SIZE.to_string()),
                    ("sort", "updated".to_owned()),
                    ("order", "desc".to_owned()),
                ],
            )
            .await?;
        Ok(found
            .data
            .unwrap_or_default()
            .into_iter()
            .map(RepoInfo::from)
            .collect())
    }

    pub(crate) async fn fetch_repo_info(&self, repo: &Repo) -> Result<RepoInfo> {
        let r: ApiRepo = self
            .get(&format!("/repos/{}/{}", repo.owner, repo.name), &[])
            .await?;
        Ok(r.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smithy_core::{Error, Provider};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn api_repo(name: &str) -> serde_json::Value {
        json!({
            "name": name, "owner": {"login": "o", "id": 1},
            "description": "une description", "private": true, "fork": false, "archived": false,
            "empty": false, "stars_count": 3, "open_pr_counter": 2, "open_issues_count": 5,
            "updated_at": "2026-10-13T09:49:19Z", "default_branch": "main",
            "allow_squash_merge": true
        })
    }

    async fn serve(server: &MockServer, p: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(p))
            .and(header("Authorization", "token secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn forgejo(server: &MockServer) -> Forgejo {
        Forgejo::new(server.uri(), "secret")
    }

    #[tokio::test]
    async fn les_champs_de_l_accueil_sont_lus() {
        let server = MockServer::start().await;
        serve(&server, "/api/v1/user/repos", json!([api_repo("demo")])).await;
        let r = forgejo(&server)
            .list_repos(&RepoSource::Mine)
            .await
            .unwrap();
        assert_eq!(r.len(), 1);
        let i = &r[0];
        assert_eq!(
            i.repo,
            Repo {
                owner: "o".into(),
                name: "demo".into()
            }
        );
        assert_eq!(i.description, "une description");
        assert!(i.private && !i.fork && !i.archived && !i.empty);
        assert_eq!((i.stars, i.open_prs, i.open_issues), (3, 2, 5));
        assert_eq!(i.updated.as_deref(), Some("2026-10-13T09:49:19Z"));
        assert_eq!(i.default_branch, "main");
    }

    #[tokio::test]
    async fn champs_absents_tolerés() {
        let server = MockServer::start().await;
        serve(
            &server,
            "/api/v1/user/repos",
            json!([{"name": "x", "owner": {"login": "o"}}]),
        )
        .await;
        let r = forgejo(&server)
            .list_repos(&RepoSource::Mine)
            .await
            .unwrap();
        assert_eq!(r[0].description, "");
        assert_eq!((r[0].stars, r[0].open_prs), (0, 0));
        assert_eq!(r[0].updated, None);
    }

    #[tokio::test]
    async fn chaque_source_a_son_chemin() {
        let server = MockServer::start().await;
        serve(
            &server,
            "/api/v1/user/subscriptions",
            json!([api_repo("suivi")]),
        )
        .await;
        serve(&server, "/api/v1/user/starred", json!([api_repo("favori")])).await;
        let f = forgejo(&server);
        assert_eq!(
            f.list_repos(&RepoSource::Watched).await.unwrap()[0]
                .repo
                .name,
            "suivi"
        );
        assert_eq!(
            f.list_repos(&RepoSource::Starred).await.unwrap()[0]
                .repo
                .name,
            "favori"
        );
    }

    #[tokio::test]
    async fn pagination_des_listes() {
        let server = MockServer::start().await;
        let page = |n: usize, prefix: &str| {
            ResponseTemplate::new(200).set_body_json(
                (0..n)
                    .map(|i| api_repo(&format!("{prefix}{i}")))
                    .collect::<Vec<_>>(),
            )
        };
        for (p, n, prefix) in [("1", PAGE_SIZE, "a"), ("2", 3, "b")] {
            Mock::given(method("GET"))
                .and(path("/api/v1/user/starred"))
                .and(query_param("page", p))
                .respond_with(page(n, prefix))
                .mount(&server)
                .await;
        }
        let r = forgejo(&server)
            .list_repos(&RepoSource::Starred)
            .await
            .unwrap();
        assert_eq!(r.len(), PAGE_SIZE + 3);
    }

    #[tokio::test]
    async fn liste_vide_null() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/starred"))
            .respond_with(ResponseTemplate::new(200).set_body_string("null"))
            .mount(&server)
            .await;
        assert!(
            forgejo(&server)
                .list_repos(&RepoSource::Starred)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recherche_enveloppee_dans_data() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/search"))
            .and(query_param("q", "tre e"))
            .and(query_param("limit", "50"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"ok": true, "data": [api_repo("tree")]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/search"))
            .and(query_param("q", "rien"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true, "data": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/search"))
            .and(query_param("q", "nul"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"ok": true, "data": null})),
            )
            .mount(&server)
            .await;
        let f = forgejo(&server);
        let r = f
            .list_repos(&RepoSource::Search("tre e".into()))
            .await
            .unwrap();
        assert_eq!(r[0].repo.name, "tree", "la requête est encodée (espace)");
        assert!(
            f.list_repos(&RepoSource::Search("rien".into()))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            f.list_repos(&RepoSource::Search("nul".into()))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn fiche_d_un_depot_et_depot_inconnu() {
        let server = MockServer::start().await;
        serve(&server, "/api/v1/repos/o/demo", api_repo("demo")).await;
        let f = forgejo(&server);
        let repo = Repo {
            owner: "o".into(),
            name: "demo".into(),
        };
        assert_eq!(f.repo_info(&repo).await.unwrap().open_prs, 2);
        let absent = Repo {
            owner: "o".into(),
            name: "nope".into(),
        };
        assert!(matches!(
            f.repo_info(&absent).await,
            Err(Error::NotFound(_))
        ));
    }
}
