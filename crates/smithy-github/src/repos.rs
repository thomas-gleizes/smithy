//! Listes de dépôts de l'accueil et fiche d'un dépôt.
//!
//! GitHub n'a pas de compteur de PR ouvertes dans la fiche : `open_issues_count` **inclut les
//! PR** (mesuré). On le range dans `open_issues` et `open_prs` reste à 0.

use serde::Deserialize;
use smithy_core::{Repo, RepoInfo, RepoSource, Result};

use crate::{Http, MAX_PER_PAGE};

/// Pages de 100 dépôts au plus par liste.
const MAX_PAGES: usize = 10;
/// La recherche : une seule page (comme Forgejo), elle se précise plutôt qu'elle ne se pagine.
const SEARCH_PAGE: usize = 50;

#[derive(Deserialize)]
pub(crate) struct ApiRepo {
    name: String,
    owner: ApiOwner,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    archived: bool,
    /// 0 pour un dépôt sans commit.
    #[serde(default)]
    size: u64,
    #[serde(default)]
    stargazers_count: u64,
    #[serde(default)]
    open_issues_count: u64,
    #[serde(default)]
    pushed_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
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
    items: Vec<ApiRepo>,
}

impl From<ApiRepo> for RepoInfo {
    fn from(r: ApiRepo) -> Self {
        RepoInfo {
            repo: Repo {
                owner: r.owner.login,
                name: r.name,
            },
            description: r.description.unwrap_or_default(),
            private: r.private,
            fork: r.fork,
            archived: r.archived,
            empty: r.size == 0,
            stars: r.stargazers_count,
            open_prs: 0,
            open_issues: r.open_issues_count,
            // Le dernier push dit mieux qu'`updated_at` (qui bouge avec une étoile) quand le
            // code a changé.
            updated: r.pushed_at.or(r.updated_at).filter(|s| !s.is_empty()),
            default_branch: r.default_branch,
        }
    }
}

impl Http {
    pub(crate) async fn fetch_repos(&self, source: &RepoSource) -> Result<Vec<RepoInfo>> {
        let (path, extra): (&str, Vec<(&str, String)>) = match source {
            // Les siens, ceux de ses organisations et ceux où l'on collabore, récents d'abord.
            RepoSource::Mine => ("/user/repos", vec![("sort", "pushed".to_owned())]),
            RepoSource::Watched => ("/user/subscriptions", Vec::new()),
            RepoSource::Starred => ("/user/starred", Vec::new()),
            RepoSource::Search(q) => return self.search_repos(q).await,
        };
        let mut repos = Vec::new();
        for page in 1..=MAX_PAGES {
            let mut query = vec![
                ("per_page", MAX_PER_PAGE.to_string()),
                ("page", page.to_string()),
            ];
            query.extend(extra.iter().cloned());
            let batch: Vec<ApiRepo> = self.get(path, &query).await?;
            let n = batch.len();
            repos.extend(batch.into_iter().map(RepoInfo::from));
            if n < MAX_PER_PAGE {
                break;
            }
        }
        Ok(repos)
    }

    async fn search_repos(&self, query: &str) -> Result<Vec<RepoInfo>> {
        let found: ApiSearch = self
            .get(
                "/search/repositories",
                &[
                    ("q", query.to_owned()),
                    ("per_page", SEARCH_PAGE.to_string()),
                ],
            )
            .await?;
        Ok(found.items.into_iter().map(RepoInfo::from).collect())
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
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::{Provider, RepoSource};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo_json(name: &str) -> serde_json::Value {
        // Réduit d'une réponse réelle (`/user/repos`).
        json!({
            "id": 1, "name": name, "full_name": format!("octo/{name}"),
            "owner": {"login": "octo", "id": 9},
            "description": null, "private": true, "fork": false, "archived": false,
            "size": 2215, "stargazers_count": 3, "open_issues_count": 4,
            "pushed_at": "2026-10-14T10:00:00Z", "updated_at": "2026-10-13T10:00:00Z",
            "default_branch": "main", "visibility": "private"
        })
    }

    #[tokio::test]
    async fn listes_paginees_recherche_et_fiche() {
        let server = MockServer::start().await;
        let full: Vec<_> = (0..100).map(|i| repo_json(&format!("r{i}"))).collect();
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .and(query_param("page", "1"))
            .and(query_param("per_page", "100"))
            .and(query_param("sort", "pushed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full))
            .mount(&server)
            .await;
        let mut empty = repo_json("vide");
        empty["size"] = json!(0);
        Mock::given(method("GET"))
            .and(path("/user/repos"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([empty])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .and(query_param("q", "tui"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 1, "incomplete_results": false, "items": [repo_json("trouve")]
            })))
            .mount(&server)
            .await;
        let g = github(&server);
        let mine = g.list_repos(&RepoSource::Mine).await.unwrap();
        assert_eq!(mine.len(), 101);
        let r = &mine[0];
        assert_eq!(r.repo.full_name(), "octo/r0");
        assert_eq!((r.private, r.empty, r.stars), (true, false, 3));
        assert_eq!((r.open_prs, r.open_issues), (0, 4), "tickets et PR mêlés");
        assert_eq!(
            r.updated.as_deref(),
            Some("2026-10-14T10:00:00Z"),
            "dernier push"
        );
        assert!(r.description.is_empty());
        assert!(mine[100].empty, "taille 0 : dépôt sans commit");
        let found = g
            .list_repos(&RepoSource::Search("tui".into()))
            .await
            .unwrap();
        assert_eq!(found[0].repo.name, "trouve");
    }
}
