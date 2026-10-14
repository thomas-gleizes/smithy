//! Pull requests : liste et détail.
//!
//! Mesuré : la liste n'a ni volume (`additions`, `changed_files`) ni nombre de commentaires ni
//! `mergeable` (seul `/pulls/{n}` les donne : `stats` vaut `None` dans la liste). La CI d'une PR
//! est dans les **check runs** de son commit de tête (GitHub Actions n'y pose pas de statut :
//! `/status` répond `pending` avec 0 statut) ; les statuts classiques (services externes) s'y
//! ajoutent. Les commentaires de ligne viennent en un seul appel (`/pulls/{n}/comments`) et sont
//! rattachés à leur revue.

use serde::Deserialize;
use smithy_core::{
    CheckStatus, Comment, Commit, PullDetail, PullFilter, PullPage, PullRequest, PullState,
    PullStats, Repo, Result, Review, ReviewComment, ReviewState, Status,
};

use crate::{Http, MAX_PER_PAGE, code::ApiCommit};

/// Taille d'une page de la liste (l'application charge au fil de l'eau).
const PULLS_PAGE: usize = 50;

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
    draft: bool,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    head: ApiRef,
    #[serde(default)]
    base: ApiRef,
    #[serde(default)]
    labels: Vec<ApiLabel>,
    #[serde(default)]
    html_url: Option<String>,
    // Détail seulement :
    #[serde(default)]
    mergeable: Option<bool>,
    #[serde(default)]
    additions: Option<u64>,
    #[serde(default)]
    deletions: Option<u64>,
    #[serde(default)]
    changed_files: Option<u64>,
    #[serde(default)]
    comments: Option<u64>,
}

#[derive(Deserialize, Default)]
struct ApiUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize, Default)]
struct ApiRef {
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
    body: Option<String>,
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    commit_id: Option<String>,
}

#[derive(Deserialize)]
struct ApiReviewComment {
    #[serde(default)]
    pull_request_review_id: Option<u64>,
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
    body: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct ApiCheckRuns {
    #[serde(default)]
    check_runs: Vec<ApiCheckRun>,
}

#[derive(Deserialize)]
struct ApiCheckRun {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    output: Option<ApiCheckOutput>,
}

#[derive(Deserialize)]
struct ApiCheckOutput {
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct ApiCombined {
    #[serde(default)]
    statuses: Vec<ApiStatus>,
}

#[derive(Deserialize)]
struct ApiStatus {
    #[serde(default)]
    context: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
}

fn login(user: Option<ApiUser>) -> String {
    user.map(|u| u.login).unwrap_or_default()
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

impl ApiPull {
    fn into_pull(self) -> PullRequest {
        let state = if self.merged_at.is_some() {
            PullState::Merged
        } else if self.state == "closed" {
            PullState::Closed
        } else {
            PullState::Open
        };
        let stats = match (self.additions, self.deletions, self.changed_files) {
            (Some(additions), Some(deletions), Some(changed_files)) => Some(PullStats {
                additions,
                deletions,
                changed_files,
                comments: self.comments.unwrap_or(0),
            }),
            _ => None,
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
            labels: self.labels.into_iter().map(|l| l.name).collect(),
            stats,
            // `null` pendant que GitHub le calcule ; sans objet fermée ou en brouillon.
            mergeable: self
                .mergeable
                .filter(|_| state == PullState::Open && !self.draft),
            created: non_empty(self.created_at),
            updated: non_empty(self.updated_at),
            closed: non_empty(self.merged_at).or_else(|| non_empty(self.closed_at)),
            url: non_empty(self.html_url),
        }
    }
}

fn review_state(s: &str) -> ReviewState {
    match s {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        "COMMENTED" => ReviewState::Commented,
        _ => ReviewState::Other,
    }
}

fn check_status(status: &str, conclusion: Option<&str>) -> Status {
    match (status, conclusion) {
        ("completed", Some("success")) => Status::Success,
        ("completed", Some("failure" | "timed_out" | "startup_failure" | "action_required")) => {
            Status::Failure
        }
        ("completed", Some("cancelled")) => Status::Cancelled,
        ("completed", Some("skipped" | "neutral" | "stale")) => Status::Skipped,
        ("completed", _) => Status::Unknown,
        ("in_progress", _) => Status::Running,
        _ => Status::Pending,
    }
}

fn commit_status(state: &str) -> Status {
    match state {
        "success" => Status::Success,
        "failure" | "error" => Status::Failure,
        "pending" => Status::Running,
        _ => Status::Unknown,
    }
}

/// Bilan de la CI : un échec l'emporte, puis ce qui tourne ; ignorés et annulés ne comptent pas.
fn combined(statuses: &[CheckStatus]) -> Option<Status> {
    if statuses.is_empty() {
        return None;
    }
    let has = |s: Status| statuses.iter().any(|c| c.status == s);
    Some(if has(Status::Failure) {
        Status::Failure
    } else if has(Status::Running) || has(Status::Pending) {
        Status::Running
    } else if has(Status::Success) {
        Status::Success
    } else {
        Status::Unknown
    })
}

fn filter_param(filter: PullFilter) -> &'static str {
    match filter {
        PullFilter::Open => "open",
        PullFilter::Closed => "closed",
        PullFilter::All => "all",
    }
}

impl Http {
    pub(crate) async fn fetch_pulls(
        &self,
        repo: &Repo,
        filter: PullFilter,
        page: u32,
    ) -> Result<PullPage> {
        let list: Vec<ApiPull> = self
            .get(
                &format!("/repos/{}/{}/pulls", repo.owner, repo.name),
                &[
                    ("state", filter_param(filter).to_owned()),
                    ("per_page", PULLS_PAGE.to_string()),
                    ("page", page.max(1).to_string()),
                ],
            )
            .await?;
        Ok(PullPage {
            has_more: list.len() >= PULLS_PAGE,
            pulls: list.into_iter().map(ApiPull::into_pull).collect(),
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
        let sha = pull.head_sha.clone();
        let page = [("per_page", MAX_PER_PAGE.to_string())];
        let (commits_api, reviews_api, line_api, comments_api) = (
            format!("{pull_api}/commits"),
            format!("{pull_api}/reviews"),
            format!("{pull_api}/comments"),
            format!("{base}/issues/{number}/comments"),
        );
        let (checks_api, status_api) = (
            format!("{base}/commits/{sha}/check-runs"),
            format!("{base}/commits/{sha}/status"),
        );
        let (commits, reviews, line_comments, comments, checks, statuses, patch) = tokio::join!(
            self.get::<Vec<ApiCommit>>(&commits_api, &page),
            self.get::<Vec<ApiReview>>(&reviews_api, &page),
            self.get::<Vec<ApiReviewComment>>(&line_api, &page),
            self.get::<Vec<ApiComment>>(&comments_api, &page),
            self.get::<ApiCheckRuns>(&checks_api, &page),
            self.get::<ApiCombined>(&status_api, &[]),
            self.fetch_diff(&pull_api, max_bytes),
        );
        let commits: Vec<Commit> = commits?.iter().map(ApiCommit::to_commit).collect();
        let mut line_comments = line_comments?;
        let reviews = reviews?
            .into_iter()
            // Une revue en brouillon (`PENDING`) n'est visible que de son auteur.
            .filter(|r| r.state != "PENDING")
            .map(|r| {
                let comments = line_comments
                    .iter_mut()
                    .filter(|c| c.pull_request_review_id == Some(r.id))
                    .map(|c| ReviewComment {
                        path: std::mem::take(&mut c.path),
                        body: std::mem::take(&mut c.body),
                        context: c.diff_hunk.lines().last().map(str::to_owned),
                    })
                    .collect();
                Review {
                    author: login(r.user),
                    state: review_state(&r.state),
                    body: r.body.unwrap_or_default(),
                    date: non_empty(r.submitted_at),
                    dismissed: r.state == "DISMISSED",
                    // Rendue sur un commit qui n'est plus la tête de la branche.
                    stale: r.commit_id.is_some_and(|c| c != sha),
                    comments,
                }
            })
            .collect();
        let comments = comments?
            .into_iter()
            .map(|c| Comment {
                author: login(c.user),
                body: c.body.unwrap_or_default(),
                date: non_empty(c.created_at),
            })
            .collect();
        let mut all: Vec<CheckStatus> = checks?
            .check_runs
            .into_iter()
            .map(|c| CheckStatus {
                status: check_status(&c.status, c.conclusion.as_deref()),
                description: c.output.and_then(|o| o.title).unwrap_or_default(),
                context: c.name,
                url: non_empty(c.html_url),
            })
            .collect();
        all.extend(statuses?.statuses.into_iter().map(|s| CheckStatus {
            status: commit_status(&s.state),
            description: s.description.unwrap_or_default(),
            context: s.context,
            url: non_empty(s.target_url),
        }));
        let (patch, patch_truncated) = patch?;
        Ok(PullDetail {
            commits_truncated: commits.len() >= MAX_PER_PAGE,
            commits,
            reviews,
            comments: Some(comments),
            checks: combined(&all),
            statuses: all,
            patch,
            patch_truncated,
            pull,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const HEAD: &str = "73506fb2ec268c3cdbd2ad80585b4ea2ce54606a";

    /// Réduit d'une réponse réelle (ratatui/ratatui) : la liste n'a pas les compteurs.
    fn pull_json(n: u64) -> serde_json::Value {
        json!({
            "number": n, "title": format!("feat: n°{n}"), "body": null, "state": "open",
            "draft": false, "user": {"login": "ana"}, "merged_at": null, "closed_at": null,
            "created_at": "2026-10-14T10:00:00Z", "updated_at": "2026-10-14T11:00:00Z",
            "head": {"ref": "feature/x", "sha": HEAD, "label": "ana:feature/x"},
            "base": {"ref": "main", "sha": "b"},
            "labels": [{"name": "docs", "color": "0075ca"}],
            "html_url": format!("https://github.com/octo/app/pull/{n}")
        })
    }

    #[tokio::test]
    async fn liste_sans_compteurs_et_etats() {
        let server = MockServer::start().await;
        let mut merged = pull_json(2);
        merged["state"] = json!("closed");
        merged["merged_at"] = json!("2026-10-14T12:00:00Z");
        merged["closed_at"] = json!("2026-10-14T12:00:00Z");
        let mut closed = pull_json(3);
        closed["state"] = json!("closed");
        closed["closed_at"] = json!("2026-10-14T13:00:00Z");
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/pulls"))
            .and(query_param("state", "all"))
            .and(query_param("per_page", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                pull_json(1),
                merged,
                closed
            ])))
            .mount(&server)
            .await;
        let page = github(&server)
            .list_pulls(&repo(), PullFilter::All, 1)
            .await
            .unwrap();
        assert!(!page.has_more);
        let states: Vec<_> = page.pulls.iter().map(|p| p.state).collect();
        assert_eq!(
            states,
            [PullState::Open, PullState::Merged, PullState::Closed]
        );
        let p = &page.pulls[0];
        assert_eq!((p.stats, p.mergeable), (None, None), "pas dans la liste");
        assert_eq!((p.head.as_str(), p.base.as_str()), ("feature/x", "main"));
        assert_eq!(p.labels, ["docs"]);
        assert!(p.body.is_empty());
        assert_eq!(
            page.pulls[1].closed.as_deref(),
            Some("2026-10-14T12:00:00Z")
        );
    }

    #[tokio::test]
    async fn detail_revues_commentaires_checks_et_diff() {
        let server = MockServer::start().await;
        let base = "/repos/octo/app";
        let mut detail = pull_json(1);
        detail["mergeable"] = json!(false);
        detail["additions"] = json!(33);
        detail["deletions"] = json!(3);
        detail["changed_files"] = json!(6);
        detail["comments"] = json!(1);
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1")))
            .and(header("Accept", "application/vnd.github.diff"))
            .respond_with(ResponseTemplate::new(200).set_body_string("diff --git a/x b/x\n+y\n"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/pulls/1")))
            .respond_with(ResponseTemplate::new(200).set_body_json(detail))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/pulls/1/commits")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"sha": "c1", "commit": {"message": "feat: un\n", "author": {"name": "Ana", "date": "2026-10-14T10:00:00Z"}}}
            ])))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/pulls/1/reviews")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": 7, "user": {"login": "bob"}, "state": "CHANGES_REQUESTED", "body": "Non.",
                 "commit_id": "vieux", "submitted_at": "2026-10-14T10:30:00Z"},
                {"id": 8, "user": {"login": "bob"}, "state": "APPROVED", "body": "", "commit_id": HEAD},
                {"id": 9, "user": {"login": "moi"}, "state": "PENDING", "body": "brouillon"},
                {"id": 10, "user": {"login": "eve"}, "state": "DISMISSED", "body": "écartée"}
            ])))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/pulls/1/comments")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"pull_request_review_id": 7, "path": "src/lib.rs", "body": "Et ça ?",
                 "diff_hunk": "@@ -1 +1,2 @@\n a\n+b"}
            ])))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/issues/1/comments")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"user": {"login": "eve"}, "body": "Merci", "created_at": "2026-10-14T11:00:00Z"}
            ])))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/commits/{HEAD}/check-runs")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 2, "check_runs": [
                    {"name": "test", "status": "completed", "conclusion": "success",
                     "html_url": "https://github.com/octo/app/runs/1", "output": {"title": null}},
                    {"name": "lint", "status": "in_progress", "conclusion": null, "output": {"title": "en cours"}}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(path(format!("{base}/commits/{HEAD}/status")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "state": "failure", "total_count": 1, "statuses": [
                    {"context": "codecov", "state": "failure", "description": "baisse", "target_url": ""}
                ]
            })))
            .mount(&server)
            .await;
        let d = github(&server)
            .pull_detail(&repo(), 1, 1 << 20)
            .await
            .unwrap();
        let st = d.pull.stats.unwrap();
        assert_eq!(
            (st.additions, st.deletions, st.changed_files, st.comments),
            (33, 3, 6, 1)
        );
        assert_eq!(d.pull.mergeable, Some(false), "conflit");
        assert_eq!(d.commits[0].summary(), "feat: un");
        let reviews: Vec<_> = d
            .reviews
            .iter()
            .map(|r| (r.author.as_str(), r.state, r.dismissed, r.stale))
            .collect();
        assert_eq!(
            reviews,
            [
                ("bob", ReviewState::ChangesRequested, false, true),
                ("bob", ReviewState::Approved, false, false),
                ("eve", ReviewState::Other, true, false)
            ],
            "la revue en brouillon n'est pas montrée"
        );
        assert_eq!(
            d.reviews[0].comments,
            [ReviewComment {
                path: "src/lib.rs".into(),
                body: "Et ça ?".into(),
                context: Some("+b".into())
            }]
        );
        assert_eq!(d.comments.unwrap()[0].body, "Merci");
        let checks: Vec<_> = d
            .statuses
            .iter()
            .map(|c| (c.context.as_str(), c.status))
            .collect();
        assert_eq!(
            checks,
            [
                ("test", Status::Success),
                ("lint", Status::Running),
                ("codecov", Status::Failure)
            ]
        );
        assert_eq!(d.checks, Some(Status::Failure), "un échec l'emporte");
        assert!(d.patch.starts_with("diff --git a/x"));
    }

    #[test]
    fn bilan_de_la_ci() {
        let c = |s| CheckStatus {
            context: String::new(),
            status: s,
            description: String::new(),
            url: None,
        };
        assert_eq!(combined(&[]), None);
        assert_eq!(
            combined(&[c(Status::Success), c(Status::Skipped)]),
            Some(Status::Success)
        );
        assert_eq!(
            combined(&[c(Status::Success), c(Status::Pending)]),
            Some(Status::Running)
        );
    }
}
