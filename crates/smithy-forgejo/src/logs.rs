//! Logs d'un job, via les routes web de Forgejo (pas d'API REST pour ça, validé sur
//! Forgejo 15) : `POST /{o}/{r}/actions/runs/{run}/jobs/{index}/attempt/{n}`. Elles
//! n'acceptent que le cookie de session et ne sont pas documentées : le format peut
//! changer d'une version à l'autre, d'où des champs tous optionnels.

use reqwest::{StatusCode, header};
use serde::Deserialize;
use smithy_core::{Error, JobLog, Pipeline, Repo, Result, StepLog, clean_log_line};

use crate::{Forgejo, map_status};

/// Au-delà, on garde la fin de l'étape (la plus utile : c'est là que ça casse).
const MAX_LINES_PER_STEP: usize = 50_000;

#[derive(Deserialize, Default)]
struct JobView {
    #[serde(default)]
    state: ViewState,
    #[serde(default)]
    logs: ViewLogs,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ViewState {
    #[serde(default)]
    run: ViewRun,
    #[serde(default)]
    current_job: ViewJob,
}

#[derive(Deserialize, Default)]
struct ViewRun {
    #[serde(default)]
    jobs: Vec<ViewRunJob>,
}

#[derive(Deserialize)]
struct ViewRunJob {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ViewJob {
    #[serde(default)]
    steps: Vec<ViewStep>,
    #[serde(default)]
    all_attempts: Vec<ViewAttempt>,
}

#[derive(Deserialize)]
struct ViewAttempt {
    #[serde(default)]
    number: u64,
}

#[derive(Deserialize)]
struct ViewStep {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ViewLogs {
    #[serde(default)]
    steps_log: Vec<ViewStepLog>,
}

#[derive(Deserialize)]
struct ViewStepLog {
    #[serde(default)]
    step: usize,
    #[serde(default)]
    lines: Vec<ViewLine>,
}

#[derive(Deserialize)]
struct ViewLine {
    #[serde(default)]
    index: u64,
    #[serde(default)]
    message: String,
}

impl Forgejo {
    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// `steps` : étapes dont on veut la sortie. Une étape hors bornes fait répondre
    /// Forgejo par une page d'erreur HTML, d'où la lecture préalable de la liste.
    async fn job_view(
        &self,
        repo: &Repo,
        number: u64,
        index: usize,
        attempt: Option<u64>,
        steps: &[usize],
    ) -> Result<JobView> {
        if self.cookies.is_empty() {
            return Err(Error::SessionExpired);
        }
        let mut path = format!(
            "/{}/{}/actions/runs/{number}/jobs/{index}",
            repo.owner, repo.name
        );
        if let Some(a) = attempt {
            path.push_str(&format!("/attempt/{a}"));
        }
        let cursors: Vec<_> = steps
            .iter()
            .map(|s| serde_json::json!({"step": s, "cursor": null, "expanded": true}))
            .collect();
        let resp = self
            .web
            .post(format!("{}{path}", self.base_url))
            .header(header::COOKIE, self.cookie_header())
            .json(&serde_json::json!({ "logCursors": cursors }))
            .send()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;

        let status = resp.status();
        if status.is_redirection()
            || matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            )
        {
            // Sans session valide, Forgejo répond 404 (ou renvoie vers le login).
            return Err(Error::SessionExpired);
        }
        if !status.is_success() {
            return Err(Error::Network(format!("HTTP {status} sur {path}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;
        serde_json::from_str(&text).map_err(|_| {
            Error::Decode(format!(
                "réponse inattendue sur {path} (route web modifiée dans cette version de Forgejo ?)"
            ))
        })
    }

    pub(crate) async fn fetch_job_log(&self, repo: &Repo, pipeline: &Pipeline) -> Result<JobLog> {
        let number = pipeline.number;
        if number == 0 {
            return Err(Error::NotFound("numéro de run inconnu".into()));
        }

        // 1. Position du job dans le run (le chemin veut l'index, pas l'id).
        let first = self.job_view(repo, number, 0, None, &[]).await?;
        let index = first
            .state
            .run
            .jobs
            .iter()
            .position(|j| j.name == pipeline.job)
            .ok_or_else(|| Error::NotFound(format!("job « {} »", pipeline.job)))?;

        // 2. Dernière tentative (relances).
        let probe = if index == 0 {
            first
        } else {
            self.job_view(repo, number, index, None, &[]).await?
        };
        let attempt = probe
            .state
            .current_job
            .all_attempts
            .iter()
            .map(|a| a.number)
            .max()
            .unwrap_or(1);

        // 3. Liste des étapes, puis 4. leur sortie.
        let view = self
            .job_view(repo, number, index, Some(attempt), &[])
            .await?;
        let steps = view.state.current_job.steps;
        let mut logs = if steps.is_empty() {
            ViewLogs::default()
        } else {
            let wanted: Vec<usize> = (0..steps.len()).collect();
            self.job_view(repo, number, index, Some(attempt), &wanted)
                .await?
                .logs
        };

        Ok(JobLog {
            steps: steps
                .into_iter()
                .enumerate()
                .map(|(i, s)| StepLog {
                    name: s.summary,
                    status: map_status(&s.status),
                    lines: step_lines(&mut logs, i),
                })
                .collect(),
        })
    }
}

fn step_lines(logs: &mut ViewLogs, step: usize) -> Vec<String> {
    let Some(entry) = logs.steps_log.iter_mut().find(|l| l.step == step) else {
        return Vec::new();
    };
    entry.lines.sort_by_key(|l| l.index);
    let total = entry.lines.len();
    let skipped = total.saturating_sub(MAX_LINES_PER_STEP);
    let mut out = Vec::with_capacity(total - skipped + 1);
    if skipped > 0 {
        out.push(format!("… {skipped} lignes précédentes omises"));
    }
    out.extend(
        entry
            .lines
            .drain(skipped..)
            .map(|l| clean_log_line(&l.message)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use smithy_core::{Provider, Status};
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RUN: &str = "/smithy/demo/actions/runs/7/jobs";

    fn repo() -> Repo {
        Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        }
    }

    fn pipeline(job: &str) -> Pipeline {
        Pipeline {
            id: 99, // l'id de tâche ne doit jamais servir dans le chemin
            number: 7,
            job: job.into(),
            workflow: "ci.yml".into(),
            title: "t".into(),
            branch: "main".into(),
            commit: "abc".into(),
            status: Status::Failure,
            url: None,
        }
    }

    fn forgejo(server: &MockServer) -> Forgejo {
        Forgejo::new(server.uri(), "tok").with_cookies(
            [
                ("session".to_owned(), "abc".to_owned()),
                ("lang".to_owned(), "fr".to_owned()),
            ]
            .into(),
        )
    }

    fn view(jobs: &[&str], attempts: &[u64], steps: Value, steps_log: Value) -> Value {
        let jobs: Vec<_> = jobs.iter().map(|n| json!({"id": 1, "name": n})).collect();
        let attempts: Vec<_> = attempts.iter().map(|n| json!({"number": n})).collect();
        json!({
            "state": {"run": {"jobs": jobs}, "currentJob": {"steps": steps, "allAttempts": attempts}},
            "logs": {"stepsLog": steps_log}
        })
    }

    async fn mock(server: &MockServer, p: &str, contains: &str, body: Value) {
        Mock::given(method("POST"))
            .and(path(p))
            .and(header("Cookie", "lang=fr; session=abc"))
            .and(body_string_contains(contains))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn logs_d_un_second_job_avec_relance() {
        let server = MockServer::start().await;
        let jobs = ["build", "test"];
        // 1. liste des jobs (index 0, sans tentative)
        mock(
            &server,
            &format!("{RUN}/0"),
            "\"logCursors\":[]",
            view(&jobs, &[1], json!([]), json!([])),
        )
        .await;
        // 2. tentatives du job « test » (index 1) : la 2 est la dernière
        mock(
            &server,
            &format!("{RUN}/1"),
            "\"logCursors\":[]",
            view(&jobs, &[1, 2], json!([]), json!([])),
        )
        .await;
        let steps = json!([
            {"summary": "Set up job", "status": "success"},
            {"summary": "cargo test", "status": "failure"}
        ]);
        // 3. étapes de la tentative 2
        mock(
            &server,
            &format!("{RUN}/1/attempt/2"),
            "\"logCursors\":[]",
            view(&jobs, &[1, 2], steps.clone(), json!([])),
        )
        .await;
        // 4. sortie (lignes dans le désordre, avec ANSI et barre de progression)
        let log = json!([
            {"step": 1, "cursor": 3, "lines": [
                {"index": 2, "message": "10%\r100%"},
                {"index": 1, "message": "\u{1b}[31mrouge\u{1b}[0m"},
                {"index": 3, "message": "fin"}]},
            {"step": 0, "cursor": 1, "lines": [{"index": 1, "message": "prêt"}]}
        ]);
        mock(
            &server,
            &format!("{RUN}/1/attempt/2"),
            "\"step\":1",
            view(&jobs, &[1, 2], steps, log),
        )
        .await;

        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("test"))
            .await
            .unwrap();
        assert_eq!(log.steps.len(), 2);
        assert_eq!(log.steps[0].name, "Set up job");
        assert_eq!(log.steps[0].status, Status::Success);
        assert_eq!(log.steps[0].lines, ["prêt"]);
        assert_eq!(log.steps[1].status, Status::Failure);
        assert_eq!(log.steps[1].lines, ["rouge", "100%", "fin"]);
    }

    #[tokio::test]
    async fn sans_cookie_aucune_requete() {
        let server = MockServer::start().await;
        let f = Forgejo::new(server.uri(), "tok");
        let r = f.job_log(&repo(), &pipeline("build")).await;
        assert!(matches!(r, Err(Error::SessionExpired)), "{r:?}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn session_refusee_ou_redirigee() {
        for resp in [
            ResponseTemplate::new(404).set_body_string("Not found."),
            ResponseTemplate::new(303).insert_header("Location", "/user/login"),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(resp)
                .mount(&server)
                .await;
            let r = forgejo(&server).job_log(&repo(), &pipeline("build")).await;
            assert!(matches!(r, Err(Error::SessionExpired)), "{r:?}");
        }
    }

    #[tokio::test]
    async fn page_html_inattendue() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<!DOCTYPE html>"))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("build")).await;
        assert!(matches!(r, Err(Error::Decode(_))), "{r:?}");
    }

    #[tokio::test]
    async fn job_inconnu_et_numero_inconnu() {
        let server = MockServer::start().await;
        mock(
            &server,
            &format!("{RUN}/0"),
            "",
            view(&["build"], &[1], json!([]), json!([])),
        )
        .await;
        let f = forgejo(&server);
        let r = f.job_log(&repo(), &pipeline("absent")).await;
        assert!(
            matches!(r, Err(Error::NotFound(ref m)) if m.contains("absent")),
            "{r:?}"
        );

        let mut p = pipeline("build");
        p.number = 0;
        assert!(matches!(
            f.job_log(&repo(), &p).await,
            Err(Error::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn job_sans_etape_donne_un_log_vide() {
        // Job annulé avant démarrage : steps vide, aucune demande de sortie.
        let server = MockServer::start().await;
        mock(
            &server,
            &format!("{RUN}/0"),
            "",
            view(&["build"], &[1], json!([]), json!([])),
        )
        .await;
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "",
            view(&["build"], &[1], json!([]), json!([])),
        )
        .await;
        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("build"))
            .await
            .unwrap();
        assert!(log.steps.is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[test]
    fn etape_trop_longue_gardee_par_la_fin() {
        let n = MAX_LINES_PER_STEP + 3;
        let lines = (1..=n as u64)
            .map(|i| ViewLine {
                index: i,
                message: format!("l{i}"),
            })
            .collect();
        let mut logs = ViewLogs {
            steps_log: vec![ViewStepLog { step: 0, lines }],
        };
        let out = step_lines(&mut logs, 0);
        assert_eq!(out.len(), MAX_LINES_PER_STEP + 1);
        assert_eq!(out[0], "… 3 lignes précédentes omises");
        assert_eq!(out[1], "l4");
        assert_eq!(out.last().unwrap(), &format!("l{n}"));
        assert!(step_lines(&mut logs, 5).is_empty());
    }
}
