//! Logs d'un job, via les routes web de Forgejo (pas d'API REST pour ça, validé sur
//! Forgejo 15) : `POST /{o}/{r}/actions/runs/{run}/jobs/{index}/attempt/{n}`. Elles
//! n'acceptent que le cookie de session et ne sont pas documentées : le format peut
//! changer d'une version à l'autre, d'où des champs tous optionnels.

use reqwest::{StatusCode, header};
use serde::Deserialize;
use smithy_core::{Error, JobLog, Pipeline, Repo, Result, StepLog, clean_log_line};

use crate::{Flavor, Forgejo, Support, map_status};

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

/// Ce que partagent les requêtes web d'un même affichage de logs.
struct WebCtx {
    /// `(valeur du cookie _csrf, jeton de la page)`, pour les versions qui l'exigent.
    csrf: Option<(String, String)>,
}

impl Forgejo {
    fn cookie_header(&self, ctx: Option<&WebCtx>) -> String {
        let mut parts: Vec<String> = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        if let Some((cookie, _)) = ctx.and_then(|c| c.csrf.as_ref()) {
            parts.push(format!("_csrf={cookie}"));
        }
        parts.join("; ")
    }

    fn session_lost(status: StatusCode) -> bool {
        // Sans session valide, Forgejo répond 404 (ou renvoie vers le login).
        status.is_redirection()
            || matches!(
                status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            )
    }

    /// Adaptateur `Legacy` : le jeton CSRF vient de n'importe quelle page du dépôt (cookie
    /// `_csrf` + `csrfToken` dans le HTML). Cette page sert aussi de test de session.
    async fn web_context(&self, flavor: Flavor, repo: &Repo, number: u64) -> Result<WebCtx> {
        if !flavor.needs_csrf() {
            return Ok(WebCtx { csrf: None });
        }
        let path = format!("/{}/{}/actions/runs/{number}", repo.owner, repo.name);
        let resp = self
            .web
            .get(format!("{}{path}", self.base_url))
            .header(header::COOKIE, self.cookie_header(None))
            .send()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;
        let status = resp.status();
        if Self::session_lost(status) {
            return Err(Error::SessionExpired);
        }
        if !status.is_success() {
            return Err(Error::Network(format!("HTTP {status} sur {path}")));
        }
        let cookie = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(|c| c.strip_prefix("_csrf="))
            .map(|c| c.split(';').next().unwrap_or_default().to_owned());
        let page = resp
            .text()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;
        let token = page
            .split("csrfToken: '")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .filter(|t| !t.is_empty());
        match (cookie, token) {
            (Some(c), Some(t)) if !c.is_empty() => Ok(WebCtx {
                csrf: Some((c, t.to_owned())),
            }),
            _ => Err(Error::Decode(
                "jeton CSRF introuvable (page modifiée dans cette version de Forgejo ?)".into(),
            )),
        }
    }

    /// `steps` : étapes dont on veut la sortie. Une étape hors bornes fait répondre
    /// Forgejo par une page d'erreur HTML, d'où la lecture préalable de la liste.
    async fn job_view(
        &self,
        ctx: &WebCtx,
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
        let mut req = self
            .web
            .post(format!("{}{path}", self.base_url))
            .header(header::COOKIE, self.cookie_header(Some(ctx)))
            .json(&serde_json::json!({ "logCursors": cursors }));
        if let Some((_, token)) = &ctx.csrf {
            req = req.header("X-Csrf-Token", token);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;

        let status = resp.status();
        if Self::session_lost(status) {
            return Err(Error::SessionExpired);
        }
        if status == StatusCode::BAD_REQUEST {
            return Err(Error::Decode(format!(
                "requête refusée sur {path} (jeton CSRF ?)"
            )));
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
        // Sans session, inutile d'interroger quoi que ce soit.
        if self.cookies.is_empty() {
            return Err(Error::SessionExpired);
        }
        let decision = self.decision().await?;
        let flavor = decision.flavor;
        let notice = match &decision.logs {
            Support::Unsupported(why) => return Err(Error::UnsupportedVersion(why.clone())),
            Support::Partial(why) => Some(why.clone()),
            Support::Full => None,
        };
        let number = pipeline.number;
        if number == 0 {
            return Err(Error::NotFound("numéro de run inconnu".into()));
        }
        let ctx = self.web_context(flavor, repo, number).await?;

        // 1. Position du job dans le run (le chemin veut l'index, pas l'id).
        let first = self.job_view(&ctx, repo, number, 0, None, &[]).await?;
        let index = first
            .state
            .run
            .jobs
            .iter()
            .position(|j| j.name == pipeline.job)
            .ok_or_else(|| Error::NotFound(format!("job « {} »", pipeline.job)))?;

        // 2. Liste des étapes. `Modern` : dernière tentative (relances) ; `Legacy` : pas de tentative.
        let (attempt, view) = if flavor.uses_attempts() {
            let probe = if index == 0 {
                first
            } else {
                self.job_view(&ctx, repo, number, index, None, &[]).await?
            };
            let latest = probe
                .state
                .current_job
                .all_attempts
                .iter()
                .map(|a| a.number)
                .max()
                .unwrap_or(1);
            let view = self
                .job_view(&ctx, repo, number, index, Some(latest), &[])
                .await?;
            (Some(latest), view)
        } else if index == 0 {
            (None, first)
        } else {
            (
                None,
                self.job_view(&ctx, repo, number, index, None, &[]).await?,
            )
        };

        // 3. Leur sortie.
        let steps = view.state.current_job.steps;
        let mut logs = if steps.is_empty() {
            ViewLogs::default()
        } else {
            let wanted: Vec<usize> = (0..steps.len()).collect();
            self.job_view(&ctx, repo, number, index, attempt, &wanted)
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
            notice,
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

    /// Annonce la version du serveur (détermine l'adaptateur).
    async fn mount_version(server: &MockServer, version: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": version })))
            .mount(server)
            .await;
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
        mount_version(&server, "15.0.0+gitea-1.22.0").await;
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
        mount_version(&server, "15.0.0+gitea-1.22.0").await;
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
        let posts = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .count();
        assert_eq!(
            posts, 2,
            "liste des jobs puis liste des étapes, pas de demande de sortie"
        );
    }

    /// Forgejo 10 à 12 : page du run pour le jeton CSRF, pas de segment `attempt`.
    async fn mount_legacy_page(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/smithy/demo/actions/runs/7"))
            .and(header("Cookie", "lang=fr; session=abc"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Set-Cookie", "_csrf=ck123; Path=/; Max-Age=86400")
                    .set_body_string(
                        "<script>window.config = { csrfToken: 'tok456', x: 1 }</script>",
                    ),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn forgejo_10_jeton_csrf_et_pas_de_tentative() {
        let server = MockServer::start().await;
        mount_version(&server, "10.0.3+gitea-1.22.0").await;
        mount_legacy_page(&server).await;
        let jobs = ["build", "test"];
        let steps = json!([
            {"summary": "Set up job", "status": "success"},
            {"summary": "Tests", "status": "failure"}
        ]);
        let log = json!([{"step": 1, "cursor": 1, "lines": [{"index": 1, "message": "boum"}]}]);
        // Les deux demandes partent du même chemin, sans `attempt`, avec les deux moitiés du CSRF.
        let csrf_post = |contains: &'static str, body: Value| {
            Mock::given(method("POST"))
                .and(path(format!("{RUN}/1")))
                .and(header("Cookie", "lang=fr; session=abc; _csrf=ck123"))
                .and(header("X-Csrf-Token", "tok456"))
                .and(body_string_contains(contains))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
        };
        Mock::given(method("POST"))
            .and(path(format!("{RUN}/0")))
            .and(header("X-Csrf-Token", "tok456"))
            .respond_with(ResponseTemplate::new(200).set_body_json(view(
                &jobs,
                &[],
                json!([]),
                json!([]),
            )))
            .mount(&server)
            .await;
        csrf_post(
            "\"logCursors\":[]",
            view(&jobs, &[], steps.clone(), json!([])),
        )
        .mount(&server)
        .await;
        csrf_post("\"step\":1", view(&jobs, &[], steps, log))
            .mount(&server)
            .await;

        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("test"))
            .await
            .unwrap();
        assert_eq!(log.notice, None, "version vérifiée : aucune réserve");
        assert_eq!(log.steps[1].lines, ["boum"]);
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|r| !r.url.path().contains("attempt")),
            "pas de segment attempt en 10 à 12"
        );
    }

    #[tokio::test]
    async fn forgejo_10_session_perdue_a_la_lecture_de_la_page() {
        let server = MockServer::start().await;
        mount_version(&server, "11.0.16+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .and(path("/smithy/demo/actions/runs/7"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("test")).await;
        assert!(matches!(r, Err(Error::SessionExpired)), "{r:?}");
    }

    #[tokio::test]
    async fn forgejo_10_page_sans_jeton() {
        let server = MockServer::start().await;
        mount_version(&server, "12.0.4+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .and(path("/smithy/demo/actions/runs/7"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>rien</html>"))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("test")).await;
        assert!(
            matches!(r, Err(Error::Decode(ref m)) if m.contains("CSRF")),
            "{r:?}"
        );
    }

    #[tokio::test]
    async fn version_non_supportee_sans_requete_web() {
        for v in ["9.0.3+gitea-1.22.0", "7.0.12+gitea-1.21.11", "1.22.0"] {
            let server = MockServer::start().await;
            mount_version(&server, v).await;
            let r = forgejo(&server).job_log(&repo(), &pipeline("build")).await;
            assert!(matches!(r, Err(Error::UnsupportedVersion(_))), "{v}: {r:?}");
            let seen = server.received_requests().await.unwrap();
            assert!(
                seen.iter().all(|r| r.method.as_str() == "GET"),
                "{v}: aucune requête web attendue"
            );
        }
    }

    #[tokio::test]
    async fn version_non_verifiee_prevenue_dans_le_log() {
        let server = MockServer::start().await;
        mount_version(&server, "14.0.0+gitea-1.22.0").await;
        let jobs = ["build"];
        mock(
            &server,
            &format!("{RUN}/0"),
            "",
            view(&jobs, &[1], json!([]), json!([])),
        )
        .await;
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "",
            view(&jobs, &[1], json!([]), json!([])),
        )
        .await;
        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("build"))
            .await
            .unwrap();
        assert!(
            log.notice.as_deref().unwrap().contains("non vérifiée"),
            "{:?}",
            log.notice
        );
    }

    #[tokio::test]
    async fn version_illisible_ou_muette_donne_un_support_partiel() {
        // /version absent (404) : on essaie quand même, avec une réserve.
        let server = MockServer::start().await;
        let jobs = ["build"];
        mock(
            &server,
            &format!("{RUN}/0"),
            "",
            view(&jobs, &[1], json!([]), json!([])),
        )
        .await;
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "",
            view(&jobs, &[1], json!([]), json!([])),
        )
        .await;
        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("build"))
            .await
            .unwrap();
        assert!(log.notice.is_some());
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
