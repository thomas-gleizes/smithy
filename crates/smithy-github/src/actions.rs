//! GitHub Actions : runs, jobs et logs d'un job.
//!
//! Une entrée de la liste de smithy est un **job**, comme pour Forgejo. GitHub ne donne les jobs
//! que run par run (`/actions/runs/{id}/jobs`) : la liste coûte 1 + N requêtes, lancées en
//! parallèle, presque toutes servies en 304 (gratuites) par le cache d'`ETag`.
//!
//! Le log d'un job (`/actions/jobs/{id}/logs`) est un seul texte : 302 vers une URL signée, puis
//! chaque ligne préfixée d'un horodatage, avec des marqueurs `##[group]`… Il n'y a aucun découpage
//! par étape, et les horodatages ne suffisent pas à en faire un (bornes des étapes à la seconde,
//! sortie écrite en retard : mesuré). Les marqueurs, eux, suivent l'ordre des étapes :
//! - avant le premier `##[group]Run ` : « Set up job » ;
//! - chaque `##[group]Run ` : l'étape principale suivante qui s'est exécutée ;
//! - chaque `Post job cleanup.` : l'étape `Post …` suivante qui s'est exécutée ;
//! - `Evaluate and set job outputs` ou `Cleaning up orphan processes` : « Complete job ».
//!
//! Si les comptes ne correspondent pas (une étape qui écrit elle-même `##[group]Run …`), tout
//! le log va dans une seule section plutôt que dans de mauvaises étapes.

use std::time::Duration;

use serde::Deserialize;
use smithy_core::{Error, JobLog, Pipeline, Repo, Result, Status, StepLog, clean_log_line};

use crate::{Http, MAX_PER_PAGE};

/// Runs listés (le plus récent d'abord) ; leurs jobs sont demandés un par un.
const RUNS: usize = 20;
/// Un log plus gros est coupé (et le dit).
const LOG_LIMIT_BYTES: usize = 16 << 20;
const LOG_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ApiRuns {
    #[serde(default)]
    workflow_runs: Vec<ApiRun>,
}

#[derive(Deserialize, Clone)]
struct ApiRun {
    id: u64,
    #[serde(default)]
    run_number: u64,
    #[serde(default)]
    name: Option<String>,
    /// `.github/workflows/ci.yml`
    #[serde(default)]
    path: String,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    head_branch: Option<String>,
    #[serde(default)]
    head_sha: String,
}

#[derive(Deserialize)]
struct ApiJobs {
    #[serde(default)]
    jobs: Vec<ApiJob>,
}

#[derive(Deserialize)]
struct ApiJob {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    steps: Option<Vec<ApiStep>>,
}

#[derive(Deserialize, Clone)]
struct ApiStep {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    number: u64,
}

/// `status` : `queued`, `in_progress`, `completed`, `waiting`, `requested`, `pending` ;
/// `conclusion` une fois terminé : `success`, `failure`, `cancelled`, `skipped`, `neutral`,
/// `timed_out`, `action_required`, `startup_failure`, `stale`.
fn map_status(status: &str, conclusion: Option<&str>) -> Status {
    match (status, conclusion) {
        ("completed", Some("success")) => Status::Success,
        ("completed", Some("failure" | "timed_out" | "startup_failure")) => Status::Failure,
        ("completed", Some("cancelled")) => Status::Cancelled,
        ("completed", Some("skipped" | "neutral")) => Status::Skipped,
        ("completed", _) => Status::Unknown,
        ("in_progress", _) => Status::Running,
        ("queued" | "waiting" | "requested" | "pending", _) => Status::Pending,
        _ => Status::Unknown,
    }
}

/// `ci.yml` plutôt que `.github/workflows/ci.yml` ; le nom du workflow à défaut de chemin.
fn workflow_file(run: &ApiRun) -> String {
    run.path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| run.name.clone())
        .unwrap_or_default()
}

impl Http {
    pub(crate) async fn fetch_pipelines(&self, repo: &Repo) -> Result<Vec<Pipeline>> {
        let base = format!("/repos/{}/{}/actions", repo.owner, repo.name);
        let runs: ApiRuns = self
            .get(&format!("{base}/runs"), &[("per_page", RUNS.to_string())])
            .await?;
        // Les jobs de chaque run, en parallèle.
        let mut set = tokio::task::JoinSet::new();
        for (i, run) in runs.workflow_runs.iter().enumerate() {
            let http = self.clone();
            let path = format!("{base}/runs/{}/jobs", run.id);
            set.spawn(async move {
                let jobs = http
                    .get::<ApiJobs>(&path, &[("per_page", MAX_PER_PAGE.to_string())])
                    .await;
                (i, jobs)
            });
        }
        let mut per_run: Vec<Option<Vec<ApiJob>>> = Vec::new();
        per_run.resize_with(runs.workflow_runs.len(), || None);
        while let Some(done) = set.join_next().await {
            let (i, jobs) = done.map_err(|e| Error::Network(e.to_string()))?;
            per_run[i] = Some(jobs?.jobs);
        }
        let mut out = Vec::new();
        for (run, jobs) in runs.workflow_runs.iter().zip(per_run) {
            // Un run sans job (refusé au démarrage, encore à planifier) n'a pas de log à
            // ouvrir : il n'apparaît pas.
            for job in jobs.unwrap_or_default() {
                out.push(Pipeline {
                    id: job.id,
                    number: run.run_number,
                    job: job.name,
                    workflow: workflow_file(run),
                    title: run.display_title.clone(),
                    branch: run.head_branch.clone().unwrap_or_default(),
                    commit: run.head_sha.clone(),
                    status: map_status(&job.status, job.conclusion.as_deref()),
                    url: job.html_url,
                });
            }
        }
        Ok(out)
    }

    pub(crate) async fn fetch_job_log(&self, repo: &Repo, pipeline: &Pipeline) -> Result<JobLog> {
        let path = format!(
            "/repos/{}/{}/actions/jobs/{}",
            repo.owner, repo.name, pipeline.id
        );
        let job: ApiJob = self.get(&path, &[]).await?;
        let steps = job.steps.unwrap_or_default();
        let running = job.status != "completed";
        let raw = self
            .get_raw(
                &format!("{path}/logs"),
                &[],
                "application/vnd.github+json",
                LOG_LIMIT_BYTES,
                LOG_TIMEOUT,
            )
            .await;
        let (text, truncated) = match raw {
            Ok((bytes, _, truncated)) => (String::from_utf8_lossy(&bytes).into_owned(), truncated),
            // Un job en cours (ou pas encore démarré) peut ne pas avoir de log à servir.
            Err(Error::NotFound(_)) if running => {
                let mut log = split_steps(&steps, "");
                log.notice = Some(
                    "GitHub ne sert le log qu'une fois le job terminé : les étapes se mettent à \
                     jour en attendant"
                        .to_owned(),
                );
                return Ok(log);
            }
            Err(e) => return Err(e),
        };
        let mut log = split_steps(&steps, &text);
        if truncated {
            log.notice = Some(format!("log coupé à {} Mio", LOG_LIMIT_BYTES >> 20));
        }
        Ok(log)
    }
}

/// Retire le BOM et l'horodatage de tête (`2026-10-14T15:14:26.1234567Z `).
fn strip_timestamp(line: &str) -> &str {
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    let b = line.as_bytes();
    let looks_like = b.len() > 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[..4].iter().all(u8::is_ascii_digit);
    if !looks_like {
        return line;
    }
    match line.find("Z ") {
        Some(i) if i < 40 => &line[i + 2..],
        _ => line,
    }
}

/// Les marqueurs de GitHub (`##[group]`, `##[error]`…) rendus lisibles ; `None` pour une ligne à
/// ne pas afficher (`##[endgroup]`).
fn display_line(line: &str) -> Option<String> {
    let Some(rest) = line.strip_prefix("##[") else {
        return Some(clean_log_line(line));
    };
    let Some((tag, text)) = rest.split_once(']') else {
        return Some(clean_log_line(line));
    };
    let shown = match tag {
        "endgroup" => return None,
        "group" | "debug" | "command" => text.to_owned(),
        "error" => format!("Error: {text}"),
        "warning" => format!("Warning: {text}"),
        "notice" => format!("Notice: {text}"),
        _ => return Some(clean_log_line(line)),
    };
    Some(clean_log_line(&shown))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Setup,
    Main,
    Post,
    Complete,
}

/// Découpe le log d'un job en étapes (voir l'en-tête du module).
fn split_steps(steps: &[ApiStep], text: &str) -> JobLog {
    let mut steps: Vec<ApiStep> = steps.to_vec();
    steps.sort_by_key(|s| s.number);
    let last = steps.len().saturating_sub(1);
    let kind = |i: usize, s: &ApiStep| {
        if i == 0 && s.name == "Set up job" {
            Kind::Setup
        } else if i == last && s.name == "Complete job" {
            Kind::Complete
        } else if s.name.starts_with("Post ") {
            Kind::Post
        } else {
            Kind::Main
        }
    };
    let ran = |s: &ApiStep| {
        s.conclusion.as_deref() != Some("skipped")
            && !matches!(s.status.as_str(), "queued" | "pending" | "waiting")
    };
    let queue = |k: Kind| -> Vec<usize> {
        steps
            .iter()
            .enumerate()
            .filter(|(i, s)| kind(*i, s) == k && ran(s))
            .map(|(i, _)| i)
            .collect()
    };
    let (mains, posts) = (queue(Kind::Main), queue(Kind::Post));
    let setup = steps
        .iter()
        .enumerate()
        .position(|(i, s)| kind(i, s) == Kind::Setup);
    let complete = steps
        .iter()
        .enumerate()
        .position(|(i, s)| kind(i, s) == Kind::Complete);

    let lines: Vec<&str> = text.lines().map(strip_timestamp).collect();
    let groups = lines
        .iter()
        .filter(|l| l.starts_with("##[group]Run "))
        .count();
    let mut out: Vec<StepLog> = steps
        .iter()
        .map(|s| StepLog {
            name: s.name.clone(),
            status: map_status(&s.status, s.conclusion.as_deref()),
            lines: Vec::new(),
        })
        .collect();

    if groups != mains.len() || (setup.is_none() && !lines.is_empty()) {
        // Pas de découpage sûr : tout le log dans une seule section, à la suite des étapes.
        if !lines.is_empty() {
            out.push(StepLog {
                name: "Journal complet".to_owned(),
                status: Status::Unknown,
                lines: lines.iter().filter_map(|l| display_line(l)).collect(),
            });
        }
        return JobLog {
            steps: out,
            notice: (!lines.is_empty())
                .then(|| "étapes non reconnues dans le log : il est affiché en entier".to_owned()),
        };
    }

    let (mut mains, mut posts) = (mains.into_iter(), posts.into_iter());
    let mut current = setup;
    for line in lines {
        if line.starts_with("##[group]Run ") {
            current = mains.next().or(current);
        } else if line == "Post job cleanup." {
            current = posts.next().or(current);
        } else if (line.starts_with("Evaluate and set job outputs")
            || line.starts_with("Cleaning up orphan processes"))
            && complete.is_some()
            && current != complete
        {
            current = complete;
        }
        if let (Some(i), Some(shown)) = (current, display_line(line)) {
            out[i].lines.push(shown);
        }
    }
    JobLog {
        steps: out,
        notice: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    fn step(number: u64, name: &str, conclusion: &str) -> ApiStep {
        ApiStep {
            name: name.into(),
            status: "completed".into(),
            conclusion: Some(conclusion.into()),
            number,
        }
    }

    /// Les étapes d'un job réel en échec (lazyshell, run 59) : numéros à trous, étapes ignorées.
    fn failed_steps() -> Vec<ApiStep> {
        vec![
            step(1, "Set up job", "success"),
            step(2, "Run actions/checkout@v4", "success"),
            step(3, "Build", "success"),
            step(4, "Test", "failure"),
            step(5, "Perf budget", "skipped"),
            step(13, "Post Run actions/setup-go@v5", "skipped"),
            step(14, "Post Run actions/checkout@v4", "success"),
            step(15, "Complete job", "success"),
        ]
    }

    /// Reconstitué d'après la forme mesurée : BOM, horodatages, marqueurs.
    fn failed_log() -> String {
        let lines = [
            "Current runner version: '2.336.0'",
            "##[group]Runner Image",
            "Image: macos-15",
            "##[endgroup]",
            "Complete job name: test",
            "##[group]Run actions/checkout@v4",
            "with:",
            "##[endgroup]",
            "Syncing repository",
            "##[group]Run go build ./...",
            "\u{1b}[36;1mgo build ./...\u{1b}[0m",
            "##[endgroup]",
            "##[group]Run go test -race ./...",
            "--- FAIL: TestX (0.01s)",
            "##[error]Process completed with exit code 1.",
            "Post job cleanup.",
            "[command]/usr/bin/git version",
            "Cleaning up orphan processes",
        ];
        let mut out = String::from("\u{feff}");
        for (i, l) in lines.iter().enumerate() {
            out.push_str(&format!("2026-10-14T15:14:{:02}.1234567Z {l}\n", 10 + i));
        }
        out
    }

    fn names_and_lines(log: &JobLog) -> Vec<(String, Vec<String>)> {
        log.steps
            .iter()
            .map(|s| (s.name.clone(), s.lines.clone()))
            .collect()
    }

    #[test]
    fn decoupage_par_marqueurs_dans_l_ordre_des_etapes() {
        let log = split_steps(&failed_steps(), &failed_log());
        assert_eq!(log.notice, None);
        let got = names_and_lines(&log);
        assert_eq!(
            got[0],
            (
                "Set up job".into(),
                vec![
                    "Current runner version: '2.336.0'".into(),
                    "Runner Image".into(),
                    "Image: macos-15".into(),
                    "Complete job name: test".into(),
                ]
            )
        );
        assert_eq!(
            got[1].1,
            ["Run actions/checkout@v4", "with:", "Syncing repository"]
        );
        assert_eq!(
            got[2].1,
            ["Run go build ./...", "go build ./..."],
            "ANSI retiré"
        );
        assert_eq!(
            got[3].1,
            [
                "Run go test -race ./...",
                "--- FAIL: TestX (0.01s)",
                "Error: Process completed with exit code 1."
            ],
            "l'erreur reste dans l'étape qui a échoué"
        );
        assert!(got[4].1.is_empty() && got[5].1.is_empty(), "ignorées");
        assert_eq!(
            got[6].1,
            ["Post job cleanup.", "[command]/usr/bin/git version"]
        );
        assert_eq!(got[7].1, ["Cleaning up orphan processes"]);
        let statuses: Vec<Status> = log.steps.iter().map(|s| s.status).collect();
        assert_eq!(statuses[3], Status::Failure);
        assert_eq!(statuses[4], Status::Skipped);
    }

    #[test]
    fn comptes_differents_tout_dans_une_section() {
        // Une étape qui écrit elle-même « ##[group]Run … » : une de trop
        let log = failed_log().replace("Syncing repository", "##[group]Run piège");
        let out = split_steps(&failed_steps(), &log);
        assert!(out.notice.is_some());
        let last = out.steps.last().unwrap();
        assert_eq!(last.name, "Journal complet");
        assert_eq!(last.lines.len(), 15, "##[endgroup] retirés");
        assert!(out.steps[..8].iter().all(|s| s.lines.is_empty()));
    }

    #[test]
    fn horodatage_bom_et_statuts() {
        assert_eq!(
            strip_timestamp("\u{feff}2026-10-14T15:14:26.1234567Z a b"),
            "a b"
        );
        assert_eq!(strip_timestamp("2026-10-14T15:14:26Z x"), "x");
        assert_eq!(strip_timestamp("pas d'heure Z ici"), "pas d'heure Z ici");
        assert_eq!(map_status("completed", Some("timed_out")), Status::Failure);
        assert_eq!(map_status("completed", Some("neutral")), Status::Skipped);
        assert_eq!(map_status("in_progress", None), Status::Running);
        assert_eq!(map_status("queued", None), Status::Pending);
    }

    fn run_json(id: u64, number: u64) -> serde_json::Value {
        json!({
            "id": id, "run_number": number, "run_attempt": 1, "name": "CI",
            "path": ".github/workflows/ci.yml", "display_title": format!("feat: n°{number}"),
            "head_branch": "main", "head_sha": "a068523abc", "status": "completed",
            "conclusion": "success", "event": "push"
        })
    }

    fn job_json(id: u64, name: &str, status: &str, conclusion: Option<&str>) -> serde_json::Value {
        json!({
            "id": id, "run_id": 1, "name": name, "status": status, "conclusion": conclusion,
            "html_url": format!("https://github.com/octo/app/actions/runs/1/job/{id}"),
            "steps": [
                {"number": 1, "name": "Set up job", "status": "completed", "conclusion": "success"},
                {"number": 2, "name": "Build", "status": status, "conclusion": conclusion},
                {"number": 3, "name": "Complete job", "status": "completed", "conclusion": "success"}
            ]
        })
    }

    #[tokio::test]
    async fn un_job_par_ligne_et_ses_logs_sans_le_token_sur_la_redirection() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/runs"))
            .and(query_param("per_page", "20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 2, "workflow_runs": [run_json(200, 8), run_json(100, 7)]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/runs/200/jobs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 2,
                "jobs": [job_json(21, "test", "in_progress", None), job_json(20, "build", "completed", Some("failure"))]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/runs/100/jobs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"total_count": 0, "jobs": []})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/jobs/20"))
            .respond_with(ResponseTemplate::new(200).set_body_json(job_json(
                20,
                "build",
                "completed",
                Some("failure"),
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/jobs/20/logs"))
            .and(header("Authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("Location", format!("{}/blob/log.txt?sig=x", storage.uri())),
            )
            .mount(&server)
            .await;
        // Le stockage refuse un en-tête d'authentification (comme Azure, mesuré)
        Mock::given(method("GET"))
            .and(path("/blob/log.txt"))
            .respond_with(|req: &Request| {
                if req.headers.contains_key("authorization") {
                    ResponseTemplate::new(401)
                } else {
                    ResponseTemplate::new(200).set_body_string(
                        "\u{feff}2026-10-14T10:00:00.1Z Runner\n2026-10-14T10:00:01.1Z ##[group]Run make\n2026-10-14T10:00:02.1Z ##[error]boum\n2026-10-14T10:00:03.1Z Cleaning up orphan processes\n",
                    )
                }
            })
            .mount(&storage)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/jobs/21"))
            .respond_with(ResponseTemplate::new(200).set_body_json(job_json(
                21,
                "test",
                "in_progress",
                None,
            )))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/actions/jobs/21/logs"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let g = github(&server);
        let list = g.list_pipelines(&repo()).await.unwrap();
        // Le run 7 n'a pas de job : absent
        let got: Vec<_> = list
            .iter()
            .map(|p| (p.id, p.number, p.job.as_str(), p.status))
            .collect();
        assert_eq!(
            got,
            [
                (21, 8, "test", Status::Running),
                (20, 8, "build", Status::Failure)
            ]
        );
        let p = &list[1];
        assert_eq!(p.workflow, "ci.yml");
        assert_eq!((p.branch.as_str(), p.title.as_str()), ("main", "feat: n°8"));
        assert!(p.url.as_deref().unwrap().ends_with("/job/20"));

        let log = g.job_log(&repo(), p).await.unwrap();
        let steps: Vec<_> = log
            .steps
            .iter()
            .map(|s| (s.name.as_str(), s.lines.clone()))
            .collect();
        assert_eq!(
            steps,
            [
                ("Set up job", vec!["Runner".to_owned()]),
                (
                    "Build",
                    vec!["Run make".to_owned(), "Error: boum".to_owned()]
                ),
                (
                    "Complete job",
                    vec!["Cleaning up orphan processes".to_owned()]
                )
            ]
        );
        // Job en cours sans log servi : les étapes, et l'explication
        let running = g.job_log(&repo(), &list[0]).await.unwrap();
        assert_eq!(running.steps.len(), 3);
        assert_eq!(running.steps[1].status, Status::Running);
        assert!(running.notice.unwrap().contains("terminé"));
    }
}
