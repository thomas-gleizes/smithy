//! Runs et logs d'un job.
//!
//! Une entrée de la liste de smithy est un **job**, comme pour Forgejo et GitHub. GitLab les donne
//! tous d'un coup : `/projects/:id/jobs` renvoie les plus récents, chacun avec son pipeline
//! (`id`, `ref`, `sha`) et son commit (`title`). La liste coûte une seule requête (le quota est de
//! 100 par minute : un appel par pipeline serait impossible toutes les 5 s).
//!
//! La trace d'un job (`/jobs/:id/trace`) est un seul texte, découpé en étapes par `trace.rs`.

use std::time::Duration;

use serde::Deserialize;
use smithy_core::{Error, JobLog, Pipeline, Repo, Result, Status};

use crate::{Http, MAX_PER_PAGE, project, trace};

/// Une trace plus grosse est coupée (et le dit).
const LOG_LIMIT_BYTES: usize = 16 << 20;
const LOG_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ApiJob {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    stage: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    allow_failure: bool,
    #[serde(default, rename = "ref")]
    git_ref: String,
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    commit: Option<ApiCommit>,
    #[serde(default)]
    pipeline: Option<ApiPipeline>,
}

#[derive(Deserialize)]
struct ApiCommit {
    #[serde(default)]
    title: String,
}

#[derive(Deserialize)]
struct ApiPipeline {
    id: u64,
    /// Numéro dans le projet (`#2`), bien plus court que l'`id` global (`2976549425`).
    #[serde(default)]
    iid: Option<u64>,
    #[serde(default)]
    sha: String,
    #[serde(default, rename = "ref")]
    git_ref: String,
}

/// `status` d'un job : `created`, `pending`, `preparing`, `waiting_for_resource`, `scheduled`,
/// `running`, `success`, `failed`, `canceling`, `canceled`, `skipped`, `manual`. Un job `manual`
/// attend un clic qui peut ne jamais venir : il compte comme non exécuté, pas comme en cours (il
/// ferait suivre le run indéfiniment).
pub(crate) fn map_status(status: &str) -> Status {
    match status {
        "success" => Status::Success,
        "failed" => Status::Failure,
        "running" => Status::Running,
        "canceled" | "canceling" => Status::Cancelled,
        "skipped" | "manual" => Status::Skipped,
        "created" | "pending" | "preparing" | "waiting_for_resource" | "scheduled" => {
            Status::Pending
        }
        _ => Status::Unknown,
    }
}

impl Http {
    pub(crate) async fn fetch_pipelines(&self, repo: &Repo) -> Result<Vec<Pipeline>> {
        let mut jobs: Vec<ApiJob> = self
            .get(
                &format!("/projects/{}/jobs", project(repo)),
                &[("per_page", MAX_PER_PAGE.to_string())],
            )
            .await?;
        // L'API trie par identifiant de job : un job relancé après coup passerait au-dessus des
        // jobs de pipelines plus récents et couperait son pipeline en deux. Le tri est stable :
        // l'ordre des jobs d'un même pipeline ne bouge pas.
        jobs.sort_by_key(|j| std::cmp::Reverse(j.pipeline.as_ref().map_or(j.id, |p| p.id)));
        Ok(jobs
            .into_iter()
            .map(|j| {
                let (number, sha, branch) = match &j.pipeline {
                    Some(p) => (
                        p.iid.unwrap_or(p.id),
                        p.sha.clone(),
                        if p.git_ref.is_empty() {
                            j.git_ref.clone()
                        } else {
                            p.git_ref.clone()
                        },
                    ),
                    None => (j.id, String::new(), j.git_ref.clone()),
                };
                // Un échec toléré (`allow_failure`) ne fait pas échouer le pipeline : on le dit.
                let tolerated = j.status == "failed" && j.allow_failure;
                Pipeline {
                    id: j.id,
                    number,
                    job: if tolerated {
                        format!("{} (échec toléré)", j.name)
                    } else {
                        j.name
                    },
                    workflow: j.stage,
                    title: j.commit.map(|c| c.title).unwrap_or_default(),
                    branch,
                    commit: sha,
                    status: map_status(&j.status),
                    url: j.web_url,
                }
            })
            .collect())
    }

    pub(crate) async fn fetch_job_log(&self, repo: &Repo, pipeline: &Pipeline) -> Result<JobLog> {
        let path = format!("/projects/{}/jobs/{}", project(repo), pipeline.id);
        // Le statut d'aujourd'hui : la liste que l'on a peut dater de quelques secondes.
        let job: ApiJob = self.get(&path, &[]).await?;
        let status = map_status(&job.status);
        let raw = self
            .get_raw(&format!("{path}/trace"), &[], LOG_LIMIT_BYTES, LOG_TIMEOUT)
            .await;
        let (text, truncated) = match raw {
            Ok((bytes, _, truncated)) => (String::from_utf8_lossy(&bytes).into_owned(), truncated),
            // Un job pas encore démarré n'a pas de trace.
            Err(Error::NotFound(_)) if matches!(status, Status::Pending | Status::Skipped) => {
                return Ok(JobLog {
                    steps: Vec::new(),
                    notice: Some("ce job n'a pas encore de trace".to_owned()),
                });
            }
            Err(e) => return Err(e),
        };
        let mut log = trace::split(&text, status);
        if truncated {
            log.notice = Some(format!("log coupé à {} Mio", LOG_LIMIT_BYTES >> 20));
        }
        Ok(log)
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::{Pipeline, Provider, Status};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn job(id: u64, name: &str, stage: &str, status: &str, allow: bool) -> serde_json::Value {
        // Réduit d'une réponse réelle (`/jobs`).
        json!({
            "id": id, "name": name, "stage": stage, "status": status, "allow_failure": allow,
            "ref": "feat/x", "web_url": format!("https://gitlab.com/g/app/-/jobs/{id}"),
            "commit": {"id": "67dc94ec", "title": "feat: 1"},
            "pipeline": {"id": 2976549425u64, "iid": 2, "sha": "67dc94eceec4", "ref": "feat/x"}
        })
    }

    #[tokio::test]
    async fn la_liste_des_runs_tient_en_une_requete() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                job(3, "summary", "report", "success", false),
                job(2, "flaky", "test", "failed", true),
                job(1, "release", "deploy", "manual", false),
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let runs = gitlab(&server).list_pipelines(&repo()).await.unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(
            runs[0].number, 2,
            "numéro du pipeline dans le projet : le même pour tous ses jobs"
        );
        assert_eq!(
            runs[0].workflow, "report",
            "l'étape (stage) tient lieu de workflow"
        );
        assert_eq!(
            (runs[0].title.as_str(), runs[0].branch.as_str()),
            ("feat: 1", "feat/x")
        );
        assert_eq!(runs[0].commit, "67dc94eceec4");
        assert_eq!(runs[1].status, Status::Failure);
        assert_eq!(runs[1].job, "flaky (échec toléré)");
        assert_eq!(runs[2].status, Status::Skipped, "manuel : pas en cours");
        assert!(runs[0].url.as_deref().unwrap().ends_with("/jobs/3"));
    }

    #[tokio::test]
    async fn un_job_relance_reste_avec_son_pipeline() {
        let server = MockServer::start().await;
        let on = |mut j: serde_json::Value, pipeline: u64| {
            j["pipeline"]["id"] = json!(pipeline);
            j["pipeline"]["iid"] = json!(pipeline - 100);
            j
        };
        // Le job 30 (pipeline 105) a été relancé après ceux du pipeline 106 : il arrive en tête
        Mock::given(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                on(job(30, "relance", "test", "success", false), 105),
                on(job(21, "b", "test", "success", false), 106),
                on(job(20, "a", "build", "success", false), 106),
                on(job(10, "c", "build", "success", false), 105),
            ])))
            .mount(&server)
            .await;
        let runs = gitlab(&server).list_pipelines(&repo()).await.unwrap();
        let order: Vec<_> = runs.iter().map(|r| (r.number, r.job.as_str())).collect();
        assert_eq!(order, [(6, "b"), (6, "a"), (5, "relance"), (5, "c")]);
    }

    fn pipeline(id: u64) -> Pipeline {
        Pipeline {
            id,
            number: 1,
            job: "j".into(),
            workflow: "test".into(),
            title: String::new(),
            branch: String::new(),
            commit: String::new(),
            status: Status::Failure,
            url: None,
        }
    }

    #[tokio::test]
    async fn trace_decoupee_en_etapes_avec_le_statut_du_jour() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs/7"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(job(7, "failing", "test", "failed", false)),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs/7/trace"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../tests/fixtures/trace_failing.log")),
            )
            .mount(&server)
            .await;
        let log = gitlab(&server)
            .job_log(&repo(), &pipeline(7))
            .await
            .unwrap();
        assert!(log.notice.is_none());
        let fail = log
            .steps
            .iter()
            .find(|s| s.status == Status::Failure)
            .unwrap();
        assert!(fail.name.contains("step_script"));
    }

    #[tokio::test]
    async fn job_sans_trace_encore() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs/8"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(job(8, "attente", "test", "pending", false)),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/v4/projects/groupe%2Fsous%2Fapp/jobs/8/trace"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let log = gitlab(&server)
            .job_log(&repo(), &pipeline(8))
            .await
            .unwrap();
        assert!(log.steps.is_empty());
        assert!(log.notice.unwrap().contains("pas encore"));
    }
}
