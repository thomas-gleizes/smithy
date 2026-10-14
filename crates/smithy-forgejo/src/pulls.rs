//! Pull requests : la liste d'un dépôt et le détail d'une PR (commits, revues, conversation,
//! statuts de CI de la tête, correctif). Comme l'historique, tout passe par l'API REST
//! documentée avec le token. Comportement mesuré avec `dev/dev.sh probe-pulls` (10, 13, 15 :
//! identiques, la 15 ajoute seulement un champ `flow`).
//!
//! Droits : la conversation (`/issues/{n}/comments`) exige `read:issue`, que le token créé au
//! login n'a pas (403) ; elle est alors annoncée comme illisible, sans faire échouer le reste.
//! Tout le reste passe avec `read:repository`.

use reqwest::StatusCode;
use serde::Deserialize;
use smithy_core::{
    CheckStatus, Comment, Commit, Error, PullDetail, PullFilter, PullPage, PullRequest, PullState,
    PullStats, Repo, Result, Review, ReviewComment, ReviewState, Status,
};

use crate::{Forgejo, PAGE_SIZE, history::ApiCommit};

#[derive(Deserialize)]
struct ApiPull {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    user: Option<ApiUser>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    head: ApiBranchRef,
    #[serde(default)]
    base: ApiBranchRef,
    #[serde(default)]
    labels: Option<Vec<ApiLabel>>,
    #[serde(default)]
    comments: u64,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changed_files: u64,
    #[serde(default)]
    mergeable: bool,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    html_url: String,
}

#[derive(Deserialize, Default)]
struct ApiUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize, Default)]
struct ApiBranchRef {
    #[serde(default, rename = "ref")]
    reference: String,
    #[serde(default)]
    sha: String,
}

#[derive(Deserialize)]
struct ApiLabel {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct ApiReview {
    id: u64,
    #[serde(default)]
    user: Option<ApiUser>,
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    dismissed: bool,
    #[serde(default)]
    stale: bool,
    #[serde(default)]
    comments_count: u64,
}

#[derive(Deserialize)]
struct ApiReviewComment {
    #[serde(default)]
    path: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    diff_hunk: String,
}

#[derive(Deserialize)]
struct ApiComment {
    #[serde(default)]
    user: Option<ApiUser>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct ApiCombined {
    #[serde(default)]
    state: String,
    #[serde(default)]
    statuses: Option<Vec<ApiStatus>>,
}

#[derive(Deserialize)]
struct ApiStatus {
    #[serde(default)]
    context: String,
    /// L'état d'un statut est dans `status` (`state` reste nul, mesuré).
    #[serde(default)]
    status: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    target_url: String,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

fn login(user: Option<ApiUser>) -> String {
    user.map(|u| u.login).unwrap_or_default()
}

impl ApiPull {
    fn into_pull(self) -> PullRequest {
        let state = match (self.state.as_str(), self.merged) {
            (_, true) => PullState::Merged,
            ("closed", false) => PullState::Closed,
            _ => PullState::Open,
        };
        PullRequest {
            number: self.number,
            title: self.title,
            body: self.body.unwrap_or_default(),
            author: login(self.user),
            state,
            draft: self.draft,
            head: self.head.reference,
            base: self.base.reference,
            head_sha: self.head.sha,
            labels: self
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(|l| l.name)
                .collect(),
            stats: Some(PullStats {
                additions: self.additions,
                deletions: self.deletions,
                changed_files: self.changed_files,
                comments: self.comments,
            }),
            // Sans objet pour une PR fermée (annoncée fusionnable) ou un brouillon (annoncé
            // non fusionnable alors qu'il n'a pas de conflit, mesuré).
            mergeable: (state == PullState::Open && !self.draft).then_some(self.mergeable),
            created: non_empty(self.created_at),
            updated: non_empty(self.updated_at),
            closed: non_empty(self.merged_at).or_else(|| non_empty(self.closed_at)),
            url: Some(self.html_url).filter(|u| !u.is_empty()),
        }
    }
}

fn review_state(s: &str) -> ReviewState {
    match s {
        "APPROVED" => ReviewState::Approved,
        "REQUEST_CHANGES" => ReviewState::ChangesRequested,
        "COMMENT" => ReviewState::Commented,
        "REQUEST_REVIEW" | "PENDING" => ReviewState::Requested,
        _ => ReviewState::Other,
    }
}

/// États des statuts de commit : `pending`, `success`, `error`, `failure`, `warning`, et `""`
/// pour un état combiné sans aucun statut.
fn check_status(s: &str) -> Option<Status> {
    match s {
        "" => None,
        "success" => Some(Status::Success),
        "failure" | "error" => Some(Status::Failure),
        "pending" => Some(Status::Running),
        _ => Some(Status::Unknown),
    }
}

fn filter_param(filter: PullFilter) -> &'static str {
    match filter {
        PullFilter::Open => "open",
        PullFilter::Closed => "closed",
        PullFilter::All => "all",
    }
}

impl Forgejo {
    pub(crate) async fn fetch_pulls(
        &self,
        repo: &Repo,
        filter: PullFilter,
        page: u32,
    ) -> Result<PullPage> {
        let api = format!("/repos/{}/{}/pulls", repo.owner, repo.name);
        let query = [
            ("state", filter_param(filter).to_owned()),
            ("limit", PAGE_SIZE.to_string()),
            ("page", page.max(1).to_string()),
        ];
        let list: Option<Vec<ApiPull>> = match self.get(&api, &query).await {
            Ok(list) => list,
            // Dépôt sans commit : 404 (mesuré), comme une liste vide.
            Err(Error::NotFound(_)) => None,
            Err(e) => return Err(e),
        };
        let list = list.unwrap_or_default();
        let has_more = list.len() >= PAGE_SIZE;
        Ok(PullPage {
            pulls: list.into_iter().map(ApiPull::into_pull).collect(),
            has_more,
        })
    }

    pub(crate) async fn fetch_pull_detail(
        &self,
        repo: &Repo,
        number: u64,
        max_bytes: usize,
    ) -> Result<PullDetail> {
        let base = format!("/repos/{}/{}", repo.owner, repo.name);
        let pull_api = format!("{base}/pulls/{number}");
        let pull: ApiPull = self.get(&pull_api, &[]).await?;
        let pull = pull.into_pull();

        let commits_api = format!("{pull_api}/commits");
        let commits_query = [
            ("limit", PAGE_SIZE.to_string()),
            ("stat", "false".to_owned()),
            ("verification", "false".to_owned()),
            ("files", "false".to_owned()),
        ];
        let reviews_api = format!("{pull_api}/reviews");
        let status_api = format!("{base}/commits/{}/status", pull.head_sha);
        let comments_api = format!("{base}/issues/{number}/comments");
        let diff_api = format!("{pull_api}.diff");
        let (commits, reviews, comments, combined, patch) = tokio::join!(
            self.get::<Option<Vec<ApiCommit>>>(&commits_api, &commits_query),
            self.get::<Option<Vec<ApiReview>>>(&reviews_api, &[]),
            self.fetch_comments(&comments_api),
            async {
                if pull.head_sha.is_empty() {
                    return Ok(None);
                }
                match self.get::<ApiCombined>(&status_api, &[]).await {
                    Ok(c) => Ok(Some(c)),
                    // Branche supprimée après fusion : plus de commit à interroger.
                    Err(Error::NotFound(_)) => Ok(None),
                    Err(e) => Err(e),
                }
            },
            self.fetch_patch(&diff_api, max_bytes),
        );
        let commits: Vec<Commit> = commits?
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.into_commit().0)
            .collect();
        let (patch, patch_truncated) = patch?;
        let combined = combined?;

        let mut out = Vec::new();
        for r in reviews?.unwrap_or_default() {
            let comments = if r.comments_count > 0 {
                let api = format!("{reviews_api}/{}/comments", r.id);
                self.get::<Option<Vec<ApiReviewComment>>>(&api, &[])
                    .await?
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| ReviewComment {
                        path: c.path,
                        body: c.body,
                        context: c.diff_hunk.lines().last().map(str::to_owned),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            out.push(Review {
                author: login(r.user),
                state: review_state(&r.state),
                body: r.body,
                date: non_empty(r.submitted_at),
                dismissed: r.dismissed,
                stale: r.stale,
                comments,
            });
        }

        let (checks, statuses) = match combined {
            Some(c) => (
                check_status(&c.state),
                c.statuses
                    .unwrap_or_default()
                    .into_iter()
                    .map(|s| CheckStatus {
                        status: check_status(&s.status).unwrap_or(Status::Unknown),
                        context: s.context,
                        description: s.description,
                        url: Some(s.target_url).filter(|u| !u.is_empty()),
                    })
                    .collect(),
            ),
            None => (None, Vec::new()),
        };
        Ok(PullDetail {
            commits_truncated: commits.len() >= PAGE_SIZE,
            commits,
            reviews: out,
            comments: comments?,
            checks,
            statuses,
            patch,
            patch_truncated,
            pull,
        })
    }

    /// La conversation, ou `None` si le token n'a pas le droit de la lire (`read:issue`).
    async fn fetch_comments(&self, api: &str) -> Result<Option<Vec<Comment>>> {
        let resp = self
            .client
            .get(self.api(api))
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;
        match resp.status() {
            s if s.is_success() => {}
            StatusCode::FORBIDDEN => return Ok(None),
            StatusCode::UNAUTHORIZED => return Err(Error::Unauthorized),
            StatusCode::NOT_FOUND => return Err(Error::NotFound(api.to_owned())),
            s => return Err(Error::Network(format!("HTTP {s} sur {api}"))),
        }
        let list: Option<Vec<ApiComment>> = resp
            .json()
            .await
            .map_err(|e| Error::Decode(e.to_string()))?;
        Ok(Some(
            list.unwrap_or_default()
                .into_iter()
                .map(|c| Comment {
                    author: login(c.user),
                    body: c.body,
                    date: non_empty(c.created_at),
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo() -> Repo {
        Repo {
            owner: "smithy".into(),
            name: "revue".into(),
        }
    }

    const HEAD: &str = "73506fb2ec268c3cdbd2ad80585b4ea2ce54606a";

    /// Réduit d'une capture réelle (Forgejo 15, `dev/seed-pulls.py`).
    fn pull_json(n: u64) -> serde_json::Value {
        json!({
            "id": 100 + n, "number": n, "title": format!("feat: n°{n}"),
            "body": "Ajoute une **page de garde**.\n",
            "user": {"id": 1, "login": "smithy"},
            "state": "open", "merged": false, "draft": false, "mergeable": true,
            "head": {"label": "feature/garde", "ref": "feature/garde", "sha": HEAD, "repo_id": 3},
            "base": {"label": "main", "ref": "main", "sha": "b", "repo_id": 3},
            "labels": [{"id": 1, "name": "docs", "color": "0075ca"}],
            "comments": 2, "review_comments": 2,
            "additions": 8, "deletions": 2, "changed_files": 4,
            "created_at": "2026-10-14T19:49:05Z", "updated_at": "2026-10-14T19:49:06Z",
            "merged_at": null, "closed_at": null, "merged_by": null,
            "html_url": format!("http://x/smithy/revue/pulls/{n}"),
            "flow": 0
        })
    }

    #[tokio::test]
    async fn liste_etats_pagination_et_depot_vide() {
        let server = MockServer::start().await;
        let full: Vec<_> = (1..=PAGE_SIZE as u64).map(pull_json).collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/revue/pulls"))
            .and(query_param("state", "open"))
            .and(query_param("limit", "50"))
            .and(query_param("page", "1"))
            .and(header("Authorization", "token secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full))
            .mount(&server)
            .await;
        let mut merged = pull_json(2);
        merged["state"] = json!("closed");
        merged["merged"] = json!(true);
        merged["merged_at"] = json!("2026-10-14T19:49:07Z");
        merged["closed_at"] = json!("2026-10-14T19:49:08Z");
        let mut closed = pull_json(3);
        closed["state"] = json!("closed");
        closed["closed_at"] = json!("2026-10-14T19:49:09Z");
        closed["labels"] = json!(null);
        closed["body"] = json!(null);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/revue/pulls"))
            .and(query_param("state", "closed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([merged, closed])))
            .mount(&server)
            .await;
        let empty = Repo {
            owner: "smithy".into(),
            name: "vide".into(),
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/vide/pulls"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let f = Forgejo::new(server.uri(), "secret");

        let open = f.list_pulls(&repo(), PullFilter::Open, 1).await.unwrap();
        assert!(open.has_more && open.pulls.len() == PAGE_SIZE);
        let p = &open.pulls[0];
        assert_eq!((p.number, p.title.as_str()), (1, "feat: n°1"));
        assert_eq!(
            (p.head.as_str(), p.base.as_str()),
            ("feature/garde", "main")
        );
        assert_eq!(
            (p.state, p.mergeable, p.draft),
            (PullState::Open, Some(true), false)
        );
        assert_eq!(p.labels, ["docs"]);
        assert_eq!(
            p.stats
                .map(|s| (s.additions, s.deletions, s.changed_files, s.comments)),
            Some((8, 2, 4, 2))
        );
        assert_eq!(p.author, "smithy");
        assert_eq!(p.head_sha, HEAD);

        let closed = f.list_pulls(&repo(), PullFilter::Closed, 1).await.unwrap();
        assert!(!closed.has_more);
        let (m, c) = (&closed.pulls[0], &closed.pulls[1]);
        assert_eq!(m.state, PullState::Merged);
        assert_eq!(
            m.closed.as_deref(),
            Some("2026-10-14T19:49:07Z"),
            "date de fusion"
        );
        assert_eq!(m.mergeable, None, "sans objet pour une PR fermée");
        let mut draft = pull_json(5);
        draft["draft"] = json!(true);
        draft["mergeable"] = json!(false);
        let draft = serde_json::from_value::<ApiPull>(draft)
            .unwrap()
            .into_pull();
        assert_eq!(
            (draft.draft, draft.mergeable),
            (true, None),
            "brouillon : sans objet"
        );
        assert_eq!(c.state, PullState::Closed);
        assert_eq!(c.closed.as_deref(), Some("2026-10-14T19:49:09Z"));
        assert!(c.labels.is_empty() && c.body.is_empty());

        // Dépôt sans commit : 404, traité comme une liste vide
        let none = f.list_pulls(&empty, PullFilter::All, 1).await.unwrap();
        assert!(none.pulls.is_empty() && !none.has_more);
    }

    async fn mount_detail(server: &MockServer, comments: ResponseTemplate) {
        let base = "/api/v1/repos/smithy/revue";
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pull_json(1)))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1/commits")))
            .and(query_param("files", "false"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"sha": "c2", "commit": {"message": "refactor: renomme\n", "author": {"name": "smithy", "date": "2026-10-14T19:49:02Z"}}},
                {"sha": "c1", "commit": {"message": "feat: page de garde\n", "author": {"name": "smithy", "date": "2026-10-14T19:49:01Z"}}}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1/reviews")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 7, "user": {"login": "relecteur"}, "state": "REQUEST_CHANGES",
                 "body": "Il manque un test.", "comments_count": 1, "dismissed": true, "stale": false,
                 "submitted_at": "2026-10-14T19:49:06Z"},
                {"id": 8, "user": {"login": "relecteur"}, "state": "APPROVED",
                 "body": "Ça me va.", "comments_count": 0, "dismissed": false, "stale": true}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1/reviews/7/comments")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 1, "path": "src/lib.rs", "body": "Et le dépassement ?", "position": 6,
                 "diff_hunk": "@@ -3,1 +3,4 @@\n }\n+\n+pub fn sub(a: i32, b: i32) -> i32 {\n+    a - b"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/issues/1/comments")))
            .respond_with(comments)
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/commits/{HEAD}/status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "pending", "sha": HEAD, "total_count": 2,
                "statuses": [
                    {"context": "ci/build", "status": "success", "state": null, "description": "compilation", "target_url": ""},
                    {"context": "ci/test", "status": "pending", "state": null, "description": "tests en cours", "target_url": "http://ci/1"}
                ]
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1.diff")))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(
                    "diff --git a/garde.md b/garde.md\nnew file mode 100644\n+x\n",
                ),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn detail_commits_revues_statuts_et_correctif() {
        let server = MockServer::start().await;
        mount_detail(
            &server,
            ResponseTemplate::new(200).set_body_json(json!([
                {"id": 1, "user": {"login": "relecteur"}, "body": "Un avis\nsur deux lignes",
                 "created_at": "2026-10-14T19:49:05Z"}
            ])),
        )
        .await;
        let d = Forgejo::new(server.uri(), "secret")
            .pull_detail(&repo(), 1, 1 << 20)
            .await
            .unwrap();
        assert_eq!(d.pull.number, 1);
        let commits: Vec<_> = d.commits.iter().map(|c| c.summary()).collect();
        assert_eq!(commits, ["refactor: renomme", "feat: page de garde"]);
        assert!(!d.commits_truncated);

        assert_eq!(d.reviews.len(), 2);
        let (r1, r2) = (&d.reviews[0], &d.reviews[1]);
        assert_eq!(
            (r1.state, r1.dismissed),
            (ReviewState::ChangesRequested, true)
        );
        assert_eq!(r1.author, "relecteur");
        assert_eq!(
            r1.comments,
            [ReviewComment {
                path: "src/lib.rs".into(),
                body: "Et le dépassement ?".into(),
                context: Some("+    a - b".into()),
            }]
        );
        assert_eq!((r2.state, r2.stale), (ReviewState::Approved, true));
        assert!(r2.comments.is_empty() && r2.date.is_none());

        let comments = d.comments.unwrap();
        assert_eq!(comments[0].author, "relecteur");
        assert_eq!(comments[0].body, "Un avis\nsur deux lignes");

        assert_eq!(d.checks, Some(Status::Running));
        let st: Vec<_> = d
            .statuses
            .iter()
            .map(|s| (s.context.as_str(), s.status, s.url.as_deref()))
            .collect();
        assert_eq!(
            st,
            [
                ("ci/build", Status::Success, None),
                ("ci/test", Status::Running, Some("http://ci/1"))
            ]
        );
        assert!(d.patch.starts_with("diff --git a/garde.md"));
        assert!(!d.patch_truncated);
    }

    #[tokio::test]
    async fn conversation_illisible_sans_le_droit_read_issue() {
        let server = MockServer::start().await;
        mount_detail(
            &server,
            ResponseTemplate::new(403).set_body_json(json!({
                "message": "token does not have at least one of required scope(s): [read:issue]"
            })),
        )
        .await;
        let d = Forgejo::new(server.uri(), "secret")
            .pull_detail(&repo(), 1, 1 << 20)
            .await
            .unwrap();
        assert_eq!(
            d.comments, None,
            "403 : le reste de la PR s'affiche quand même"
        );
        assert_eq!(d.reviews.len(), 2);
    }

    #[tokio::test]
    async fn statuts_absents_et_pr_inconnue() {
        assert_eq!(check_status(""), None);
        assert_eq!(check_status("error"), Some(Status::Failure));
        assert_eq!(check_status("warning"), Some(Status::Unknown));
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/revue/pulls/9999"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(matches!(
            Forgejo::new(server.uri(), "secret")
                .pull_detail(&repo(), 9999, 1 << 20)
                .await,
            Err(Error::NotFound(_))
        ));
    }
}
