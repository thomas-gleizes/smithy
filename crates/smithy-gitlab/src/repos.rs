//! Listes de dépôts de l'accueil et fiche d'un projet.
//!
//! GitLab n'a pas de « suivis » : cette liste est vide. Un projet se désigne par son chemin
//! complet (`groupe/sous-groupe/projet`), que `Repo` découpe au dernier `/`.

use serde::Deserialize;
use smithy_core::{Repo, RepoInfo, RepoSource, Result};

use crate::{Http, MAX_PER_PAGE, project};

/// Pages de 100 projets au plus par liste.
const MAX_PAGES: usize = 10;
/// La recherche : une seule page (comme Forgejo), elle se précise plutôt qu'elle ne se pagine.
const SEARCH_PAGE: usize = 50;

#[derive(Deserialize)]
pub(crate) struct ApiProject {
    path_with_namespace: String,
    #[serde(default)]
    description: Option<String>,
    /// `private`, `internal` ou `public`.
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    empty_repo: bool,
    #[serde(default)]
    star_count: u64,
    /// Absent des listes simplifiées, présent sur la fiche.
    #[serde(default)]
    open_issues_count: Option<u64>,
    #[serde(default)]
    last_activity_at: Option<String>,
    #[serde(default)]
    default_branch: Option<String>,
    #[serde(default)]
    forked_from_project: Option<serde_json::Value>,
}

/// `a/b/c` → propriétaire `a/b`, nom `c`.
pub(crate) fn split_path(full: &str) -> Repo {
    match full.rsplit_once('/') {
        Some((owner, name)) => Repo {
            owner: owner.to_owned(),
            name: name.to_owned(),
        },
        None => Repo {
            owner: String::new(),
            name: full.to_owned(),
        },
    }
}

impl From<ApiProject> for RepoInfo {
    fn from(p: ApiProject) -> Self {
        RepoInfo {
            repo: split_path(&p.path_with_namespace),
            description: p.description.unwrap_or_default(),
            private: p.visibility != "public",
            fork: p.forked_from_project.is_some(),
            archived: p.archived,
            empty: p.empty_repo,
            stars: p.star_count,
            open_prs: 0,
            open_issues: p.open_issues_count.unwrap_or(0),
            updated: p.last_activity_at.filter(|s| !s.is_empty()),
            default_branch: p.default_branch.unwrap_or_default(),
        }
    }
}

impl Http {
    pub(crate) async fn fetch_repos(&self, source: &RepoSource) -> Result<Vec<RepoInfo>> {
        let (per_page, extra): (usize, Vec<(&str, String)>) = match source {
            RepoSource::Mine => (
                MAX_PER_PAGE,
                vec![
                    ("membership", "true".to_owned()),
                    ("order_by", "last_activity_at".to_owned()),
                ],
            ),
            RepoSource::Starred => (MAX_PER_PAGE, vec![("starred", "true".to_owned())]),
            // Pas de suivis sur GitLab.
            RepoSource::Watched => return Ok(Vec::new()),
            RepoSource::Search(q) => {
                let (found, _) = self
                    .get_page::<Vec<ApiProject>>(
                        "/projects",
                        &[
                            ("search", q.clone()),
                            ("per_page", SEARCH_PAGE.to_string()),
                            ("order_by", "last_activity_at".to_owned()),
                        ],
                    )
                    .await?;
                return Ok(found.into_iter().map(RepoInfo::from).collect());
            }
        };
        let mut repos = Vec::new();
        let mut page = 1;
        for _ in 0..MAX_PAGES {
            let mut query = vec![
                ("per_page", per_page.to_string()),
                ("page", page.to_string()),
            ];
            query.extend(extra.iter().cloned());
            let (batch, next) = self
                .get_page::<Vec<ApiProject>>("/projects", &query)
                .await?;
            repos.extend(batch.into_iter().map(RepoInfo::from));
            match next {
                Some(n) => page = n,
                None => break,
            }
        }
        Ok(repos)
    }

    /// La fiche, plus le nombre de merge requests ouvertes (un en-tête, une page d'un seul élément).
    pub(crate) async fn fetch_repo_info(&self, repo: &Repo) -> Result<RepoInfo> {
        let id = project(repo);
        let p: ApiProject = self.get(&format!("/projects/{id}"), &[]).await?;
        let mut info = RepoInfo::from(p);
        // Le décompte est un plus : sa panne (droit manquant, quota) ne cache pas la fiche.
        if let Ok(Some(total)) = self
            .get_header(
                &format!("/projects/{id}/merge_requests"),
                &[("state", "opened".to_owned()), ("per_page", "1".to_owned())],
                "x-total",
            )
            .await
        {
            info.open_prs = total.parse().unwrap_or(0);
        }
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::{Provider, RepoSource};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn project_json(path: &str) -> serde_json::Value {
        // Réduit d'une réponse réelle (`/projects`).
        json!({
            "id": 1, "path_with_namespace": path, "name": "x",
            "description": null, "visibility": "private", "archived": false,
            "empty_repo": false, "star_count": 3, "forks_count": 1,
            "last_activity_at": "2026-10-22T11:18:57.866Z", "default_branch": "main"
        })
    }

    #[tokio::test]
    async fn listes_paginees_par_en_tete_recherche_suivis_et_fiche() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("membership", "true"))
            .and(query_param("page", "1"))
            .and(query_param("per_page", "100"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([project_json("g/s/un")])),
            )
            .mount(&server)
            .await;
        let mut vide = project_json("g/deux");
        vide["empty_repo"] = json!(true);
        vide["visibility"] = json!("public");
        vide["forked_from_project"] = json!({"id": 9});
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "")
                    .set_body_json(json!([vide])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects"))
            .and(query_param("search", "tui"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([project_json("o/tui")])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/g%2Fs%2Fun"))
            .respond_with(ResponseTemplate::new(200).set_body_json({
                let mut p = project_json("g/s/un");
                p["open_issues_count"] = json!(4);
                p
            }))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/g%2Fs%2Fun/merge_requests"))
            .and(query_param("state", "opened"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-total", "2")
                    .set_body_json(json!([{"iid": 1}])),
            )
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let mine = g.list_repos(&RepoSource::Mine).await.unwrap();
        assert_eq!(mine.len(), 2, "deux pages suivies par x-next-page");
        let r = &mine[0];
        assert_eq!(
            r.repo.owner, "g/s",
            "sous-groupe : le propriétaire contient un /"
        );
        assert_eq!(r.repo.name, "un");
        assert_eq!(r.repo.full_name(), "g/s/un");
        assert_eq!((r.private, r.empty, r.stars), (true, false, 3));
        assert_eq!(r.updated.as_deref(), Some("2026-10-22T11:18:57.866Z"));
        assert!(r.description.is_empty());
        assert!(mine[1].empty && !mine[1].private && mine[1].fork);
        let found = g
            .list_repos(&RepoSource::Search("tui".into()))
            .await
            .unwrap();
        assert_eq!(found[0].repo.full_name(), "o/tui");
        assert!(g.list_repos(&RepoSource::Watched).await.unwrap().is_empty());
        let fiche = g.repo_info(&mine[0].repo).await.unwrap();
        assert_eq!((fiche.open_issues, fiche.open_prs), (4, 2));
    }
}
