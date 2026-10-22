//! Merge requests (les « pull requests » de smithy), en consultation seule.
//!
//! Mesuré : la liste donne déjà `draft`, `has_conflicts`, `detailed_merge_status`, les étiquettes
//! et `user_notes_count`, mais **ni ajouts ni suppressions** (`stats` reste `None`, comme GitHub).
//! Il n'y a pas d'état « fermée ou fusionnée » : `Closed` interroge `closed` et `merged` côte à
//! côte. La CI d'une MR est son `head_pipeline` ; la conversation vient des **discussions**, dont
//! les notes `system: true` (« a ajouté une étiquette »…) sont du bruit. Les relectures sont les
//! approbations (une revue par approbateur) ; les commentaires posés sur une ligne du diff sont
//! regroupés par auteur en une revue « commentée ».

use std::time::Duration;

use serde::Deserialize;
use smithy_core::{
    CheckStatus, Comment, Error, PullDetail, PullFilter, PullPage, PullRequest, PullState,
    PullStats, Repo, Result, Review, ReviewComment, ReviewState, Status,
};

use crate::ci::map_status;
use crate::code::{ApiCommit, decode_text, patch_stats, strip_preamble};
use crate::{Http, MAX_PER_PAGE, project};

/// Taille d'une page (comme Forgejo : l'application charge au fil de l'eau).
const PULLS_PAGE: usize = 50;
/// Au-delà, le détail dit que la liste des commits est incomplète.
const COMMITS_LIMIT: usize = 50;
const DIFF_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ApiUser {
    #[serde(default)]
    username: String,
}

#[derive(Deserialize)]
struct ApiMr {
    iid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<ApiUser>,
    /// `opened`, `closed`, `merged` (ou `locked`).
    #[serde(default)]
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    source_branch: String,
    #[serde(default)]
    target_branch: String,
    #[serde(default)]
    sha: String,
    #[serde(default)]
    labels: Vec<serde_json::Value>,
    #[serde(default)]
    has_conflicts: bool,
    #[serde(default)]
    detailed_merge_status: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    web_url: Option<String>,
    /// Seulement dans le détail.
    #[serde(default)]
    head_pipeline: Option<ApiHeadPipeline>,
}

#[derive(Deserialize)]
struct ApiHeadPipeline {
    id: u64,
    #[serde(default)]
    status: String,
}

impl From<ApiMr> for PullRequest {
    fn from(m: ApiMr) -> Self {
        let state = match m.state.as_str() {
            "opened" => PullState::Open,
            "merged" => PullState::Merged,
            _ => PullState::Closed,
        };
        let open = state == PullState::Open;
        let mergeable = if !open {
            None
        } else if m.has_conflicts {
            Some(false)
        } else if m.detailed_merge_status == "mergeable" {
            Some(true)
        } else {
            // `unchecked`, `checking`, `draft_status`… : pas de verdict.
            None
        };
        PullRequest {
            number: m.iid,
            title: m.title,
            body: m.description.unwrap_or_default(),
            author: m.author.map(|a| a.username).unwrap_or_default(),
            state,
            draft: m.draft,
            head: m.source_branch,
            base: m.target_branch,
            head_sha: m.sha,
            labels: m
                .labels
                .iter()
                .filter_map(|l| l.as_str().map(str::to_owned))
                .collect(),
            stats: None,
            mergeable,
            created: m.created_at.filter(|d| !d.is_empty()),
            updated: m.updated_at.filter(|d| !d.is_empty()),
            closed: m.merged_at.or(m.closed_at).filter(|d| !d.is_empty()),
            url: m.web_url,
        }
    }
}

#[derive(Deserialize)]
struct ApiApprovals {
    #[serde(default)]
    approved_by: Vec<ApiApprover>,
}

#[derive(Deserialize)]
struct ApiApprover {
    #[serde(default)]
    user: Option<ApiUser>,
}

#[derive(Deserialize)]
struct ApiDiscussion {
    #[serde(default)]
    notes: Vec<ApiNote>,
}

#[derive(Deserialize)]
struct ApiNote {
    #[serde(default)]
    body: String,
    #[serde(default)]
    author: Option<ApiUser>,
    #[serde(default)]
    created_at: Option<String>,
    /// Note produite par GitLab (« a ajouté l'étiquette… »).
    #[serde(default)]
    system: bool,
    /// Présente pour un commentaire posé sur une ligne du diff.
    #[serde(default)]
    position: Option<ApiPosition>,
}

#[derive(Deserialize)]
struct ApiPosition {
    #[serde(default)]
    new_path: Option<String>,
    #[serde(default)]
    old_path: Option<String>,
    #[serde(default)]
    new_line: Option<u64>,
    #[serde(default)]
    old_line: Option<u64>,
}

#[derive(Deserialize)]
struct ApiPipelineJob {
    #[serde(default)]
    name: String,
    #[serde(default)]
    stage: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    allow_failure: bool,
    #[serde(default)]
    web_url: Option<String>,
}

/// Les revues et la conversation d'après les discussions : les commentaires de ligne par auteur,
/// le reste comme conversation.
fn conversation(discussions: Vec<ApiDiscussion>) -> (Vec<Review>, Vec<Comment>) {
    let mut line_notes: Vec<Review> = Vec::new();
    let mut comments = Vec::new();
    for note in discussions.into_iter().flat_map(|d| d.notes) {
        if note.system || note.body.trim().is_empty() {
            continue;
        }
        let author = note.author.map(|a| a.username).unwrap_or_default();
        match note.position {
            Some(p) => {
                let path = p.new_path.or(p.old_path).unwrap_or_default();
                let line = p
                    .new_line
                    .map(|n| format!("ligne {n}"))
                    .or_else(|| p.old_line.map(|n| format!("ancienne ligne {n}")));
                let comment = ReviewComment {
                    path,
                    body: note.body,
                    context: line,
                };
                match line_notes.iter_mut().find(|r| r.author == author) {
                    Some(r) => r.comments.push(comment),
                    None => line_notes.push(Review {
                        author,
                        state: ReviewState::Commented,
                        body: String::new(),
                        date: note.created_at.filter(|d| !d.is_empty()),
                        dismissed: false,
                        stale: false,
                        comments: vec![comment],
                    }),
                }
            }
            None => comments.push(Comment {
                author,
                body: note.body,
                date: note.created_at.filter(|d| !d.is_empty()),
            }),
        }
    }
    (line_notes, comments)
}

fn approvals(a: ApiApprovals) -> Vec<Review> {
    a.approved_by
        .into_iter()
        .filter_map(|x| x.user)
        .map(|u| Review {
            author: u.username,
            state: ReviewState::Approved,
            body: String::new(),
            date: None,
            dismissed: false,
            stale: false,
            comments: Vec::new(),
        })
        .collect()
}

/// Le statut combiné d'après les jobs : un échec (qui compte) l'emporte, puis ce qui tourne.
fn combined(jobs: &[CheckStatus], pipeline: Status) -> Status {
    if jobs
        .iter()
        .any(|j| j.status == Status::Failure && !j.description.contains("toléré"))
    {
        Status::Failure
    } else if jobs.iter().any(|j| j.status == Status::Running) {
        Status::Running
    } else {
        pipeline
    }
}

impl Http {
    pub(crate) async fn fetch_pulls(
        &self,
        repo: &Repo,
        filter: PullFilter,
        page: u32,
    ) -> Result<PullPage> {
        let path = format!("/projects/{}/merge_requests", project(repo));
        let one = |state: &'static str| {
            let path = path.clone();
            async move {
                self.get_page::<Vec<ApiMr>>(
                    &path,
                    &[
                        ("state", state.to_owned()),
                        ("per_page", PULLS_PAGE.to_string()),
                        ("page", page.max(1).to_string()),
                        ("order_by", "created_at".to_owned()),
                        ("sort", "desc".to_owned()),
                    ],
                )
                .await
            }
        };
        let (mrs, has_more) = match filter {
            PullFilter::Open => {
                let (l, n) = one("opened").await?;
                (l, n.is_some())
            }
            PullFilter::All => {
                let (l, n) = one("all").await?;
                (l, n.is_some())
            }
            // « Fermées, fusionnées ou non » : GitLab sépare les deux états.
            PullFilter::Closed => {
                let (closed, merged) = tokio::join!(one("closed"), one("merged"));
                let ((mut a, na), (b, nb)) = (closed?, merged?);
                a.extend(b);
                a.sort_by(|x, y| y.created_at.cmp(&x.created_at));
                (a, na.is_some() || nb.is_some())
            }
        };
        Ok(PullPage {
            pulls: mrs.into_iter().map(PullRequest::from).collect(),
            has_more,
        })
    }

    pub(crate) async fn fetch_pull_detail(
        &self,
        repo: &Repo,
        number: u64,
        max_bytes: usize,
    ) -> Result<PullDetail> {
        let api = format!("/projects/{}/merge_requests/{number}", project(repo));
        let mr: ApiMr = self.get(&api, &[]).await?;
        let head_pipeline = mr
            .head_pipeline
            .as_ref()
            .map(|p| (p.id, map_status(&p.status)));
        let pipeline_jobs = async {
            match head_pipeline {
                Some((id, _)) => {
                    self.get::<Vec<ApiPipelineJob>>(
                        &format!("/projects/{}/pipelines/{id}/jobs", project(repo)),
                        &[("per_page", MAX_PER_PAGE.to_string())],
                    )
                    .await
                }
                None => Ok(Vec::new()),
            }
        };
        let (commits_api, approvals_api) = (format!("{api}/commits"), format!("{api}/approvals"));
        let (discussions_api, diff_api) =
            (format!("{api}/discussions"), format!("{api}/raw_diffs"));
        let per_page = [("per_page", COMMITS_LIMIT.to_string())];
        let all = [("per_page", MAX_PER_PAGE.to_string())];
        let (commits, approved, discussions, jobs, diff) = tokio::join!(
            self.get_page::<Vec<ApiCommit>>(&commits_api, &per_page),
            self.get::<ApiApprovals>(&approvals_api, &[]),
            self.get::<Vec<ApiDiscussion>>(&discussions_api, &all),
            pipeline_jobs,
            self.get_raw(&diff_api, &[], max_bytes, DIFF_TIMEOUT),
        );
        let (commits, more) = commits?;
        // L'API donne le plus récent d'abord ; les autres backends, le plus ancien d'abord.
        let commits: Vec<_> = commits.iter().rev().map(ApiCommit::to_commit).collect();
        // Les approbations sont facultatives (certaines offres n'en ont pas) : leur absence ne
        // cache pas la MR. Sans droit sur les discussions, la conversation est « inconnue ».
        let mut reviews = approved.map(approvals).unwrap_or_default();
        let comments = match discussions {
            Ok(d) => {
                let (line_notes, comments) = conversation(d);
                reviews.extend(line_notes);
                Some(comments)
            }
            Err(Error::Unauthorized) => None,
            Err(e) => return Err(e),
        };
        let statuses: Vec<CheckStatus> = jobs?
            .into_iter()
            .map(|j| CheckStatus {
                context: j.name,
                status: map_status(&j.status),
                description: if j.status == "failed" && j.allow_failure {
                    format!("{} (échec toléré)", j.stage)
                } else {
                    j.stage
                },
                url: j.web_url,
            })
            .collect();
        let checks = head_pipeline.map(|(_, status)| combined(&statuses, status));
        let (buf, _, patch_truncated) = diff?;
        let patch = strip_preamble(&decode_text(&buf, patch_truncated)).to_owned();
        // La liste n'a pas de compteurs : on les tire du correctif (et de la conversation).
        let mut pull = PullRequest::from(mr);
        let (changed_files, additions, deletions) = patch_stats(&patch);
        pull.stats = Some(PullStats {
            additions,
            deletions,
            changed_files,
            comments: comments.as_ref().map_or(0, |c| c.len() as u64),
        });
        Ok(PullDetail {
            pull,
            commits_truncated: more.is_some(),
            commits,
            reviews,
            comments,
            checks,
            statuses,
            patch,
            patch_truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::{Provider, PullFilter, PullState, ReviewState, Status};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const BASE: &str = "/api/v4/projects/groupe%2Fsous%2Fapp/merge_requests";

    fn mr_json(iid: u64, state: &str, created: &str) -> serde_json::Value {
        // Réduit d'une réponse réelle (`/merge_requests`).
        json!({
            "iid": iid, "title": format!("MR {iid}"), "description": "corps",
            "author": {"username": "thomas"}, "state": state, "draft": false,
            "source_branch": "feat/x", "target_branch": "main", "sha": "67dc94ec",
            "labels": ["bug"], "has_conflicts": false, "detailed_merge_status": "mergeable",
            "created_at": created, "updated_at": created,
            "merged_at": if state == "merged" { json!("2026-10-23T10:00:00Z") } else { json!(null) },
            "closed_at": null, "web_url": format!("https://gitlab.com/g/app/-/merge_requests/{iid}")
        })
    }

    #[tokio::test]
    async fn liste_ouverte_et_fermees_fusionnees_ensemble() {
        let server = MockServer::start().await;
        Mock::given(path(BASE))
            .and(query_param("state", "opened"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([mr_json(5, "opened", "2026-10-22T10:00:00Z")])),
            )
            .mount(&server)
            .await;
        Mock::given(path(BASE))
            .and(query_param("state", "closed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([mr_json(
                3,
                "closed",
                "2026-10-20T10:00:00Z"
            )])))
            .mount(&server)
            .await;
        Mock::given(path(BASE))
            .and(query_param("state", "merged"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([mr_json(
                4,
                "merged",
                "2026-10-21T10:00:00Z"
            )])))
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let open = g.list_pulls(&repo(), PullFilter::Open, 1).await.unwrap();
        assert!(open.has_more);
        let p = &open.pulls[0];
        assert_eq!(
            (p.number, p.state, p.mergeable),
            (5, PullState::Open, Some(true))
        );
        assert_eq!(
            (p.head.as_str(), p.base.as_str(), p.author.as_str()),
            ("feat/x", "main", "thomas")
        );
        assert_eq!(p.labels, ["bug"]);
        assert!(p.stats.is_none());
        let closed = g.list_pulls(&repo(), PullFilter::Closed, 1).await.unwrap();
        let numbers: Vec<_> = closed.pulls.iter().map(|p| (p.number, p.state)).collect();
        assert_eq!(
            numbers,
            [(4, PullState::Merged), (3, PullState::Closed)],
            "plus récente d'abord"
        );
        assert!(!closed.has_more);
        assert_eq!(
            closed.pulls[0].closed.as_deref(),
            Some("2026-10-23T10:00:00Z")
        );
        assert_eq!(
            closed.pulls[0].mergeable, None,
            "pas de verdict pour une MR fermée"
        );
    }

    #[tokio::test]
    async fn detail_conversation_sans_bruit_ci_et_commits_a_l_endroit() {
        let server = MockServer::start().await;
        let mut mr = mr_json(1, "opened", "2026-10-22T10:00:00Z");
        mr["head_pipeline"] = json!({"id": 99, "status": "failed"});
        Mock::given(method("GET"))
            .and(path(format!("{BASE}/1")))
            .respond_with(ResponseTemplate::new(200).set_body_json(mr))
            .mount(&server)
            .await;
        let c = |id: &str, t: &str| json!({"id": id, "title": t, "message": t, "author_name": "T", "parent_ids": []});
        Mock::given(path(format!("{BASE}/1/commits")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([c("bbbb", "second"), c("aaaa", "premier")])),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/1/approvals")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "approved_by": [{"user": {"username": "relecteur"}}]
            })))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/1/discussions")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "1", "notes": [{"body": "added 1 commit", "system": true, "author": {"username": "thomas"}}]},
                {"id": "2", "notes": [
                    {"body": "Bonjour", "system": false, "author": {"username": "thomas"}, "created_at": "2026-10-22T11:00:00Z"},
                    {"body": "Réponse", "system": false, "author": {"username": "relecteur"}}
                ]},
                {"id": "3", "notes": [{"body": "ici", "system": false, "author": {"username": "relecteur"},
                    "position": {"new_path": "hello.js", "old_path": "hello.js", "new_line": 3, "old_line": null}}]}
            ])))
            .mount(&server)
            .await;
        Mock::given(path(
            "/api/v4/projects/groupe%2Fsous%2Fapp/pipelines/99/jobs",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"name": "ok", "stage": "test", "status": "success", "allow_failure": false},
            {"name": "flaky", "stage": "test", "status": "failed", "allow_failure": true},
            {"name": "ko", "stage": "test", "status": "failed", "allow_failure": false}
        ])))
        .mount(&server)
        .await;
        Mock::given(path(format!("{BASE}/1/raw_diffs")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("diff --git a/hello.js b/hello.js\nnew file mode 100644\n"),
            )
            .mount(&server)
            .await;
        let d = gitlab(&server)
            .pull_detail(&repo(), 1, 1 << 20)
            .await
            .unwrap();
        let titles: Vec<_> = d.commits.iter().map(|c| c.summary()).collect();
        assert_eq!(titles, ["premier", "second"], "le plus ancien d'abord");
        assert!(!d.commits_truncated);
        let comments = d.comments.unwrap();
        assert_eq!(comments.len(), 2, "la note système est écartée");
        assert_eq!(comments[0].body, "Bonjour");
        assert_eq!(d.reviews.len(), 2);
        assert_eq!(
            (d.reviews[0].author.as_str(), d.reviews[0].state),
            ("relecteur", ReviewState::Approved)
        );
        let line = &d.reviews[1].comments[0];
        assert_eq!(
            (line.path.as_str(), line.context.as_deref()),
            ("hello.js", Some("ligne 3"))
        );
        assert_eq!(d.checks, Some(Status::Failure));
        assert_eq!(d.statuses.len(), 3);
        assert!(d.statuses[1].description.contains("toléré"));
        assert!(d.patch.starts_with("diff --git"));
        assert!(!d.patch_truncated);
        assert_eq!(d.pull.number, 1);
        let stats = d.pull.stats.unwrap();
        assert_eq!((stats.changed_files, stats.comments), (1, 2));
    }

    #[tokio::test]
    async fn conversation_illisible_et_approbations_absentes() {
        let server = MockServer::start().await;
        Mock::given(path(format!("{BASE}/2")))
            .respond_with(ResponseTemplate::new(200).set_body_json(mr_json(
                2,
                "opened",
                "2026-10-22T10:00:00Z",
            )))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/2/commits")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([])),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/2/approvals")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/2/discussions")))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/2/raw_diffs")))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        let d = gitlab(&server)
            .pull_detail(&repo(), 2, 1 << 20)
            .await
            .unwrap();
        assert!(
            d.comments.is_none(),
            "droit manquant : conversation inconnue"
        );
        assert!(d.reviews.is_empty());
        assert!(d.commits_truncated);
        assert_eq!(d.checks, None, "aucun pipeline");
    }
}
