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

const MAX_REDIRECTS: usize = 4;

/// Valeur du cookie `_csrf` posé par une réponse (ignoré s'il est vide ou supprimé).
fn csrf_cookie(headers: &header::HeaderMap) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter(|c| !c.to_ascii_lowercase().contains("max-age=0"))
        .find_map(|c| c.strip_prefix("_csrf="))
        .map(|c| c.split(';').next().unwrap_or_default().to_owned())
        .filter(|c| !c.is_empty())
}

/// Chemin (et requête) d'un en-tête `Location`, absolu ou relatif.
fn local_path(location: &str) -> Option<String> {
    if location.starts_with('/') {
        return Some(location.to_owned());
    }
    let url = reqwest::Url::parse(location).ok()?;
    Some(match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    })
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

    /// Familles `Legacy` et `Transitional` : le jeton CSRF vient d'une page du dépôt (cookie
    /// `_csrf` + `csrfToken` dans le HTML). Dès la 13 la page du run redirige (307) vers
    /// `…/jobs/0/attempt/1` : on suit, comme un navigateur. Cette page sert aussi de test de session.
    async fn web_context(&self, flavor: Flavor, repo: &Repo, number: u64) -> Result<WebCtx> {
        if !flavor.needs_csrf() {
            return Ok(WebCtx { csrf: None });
        }
        let mut path = format!("/{}/{}/actions/runs/{number}", repo.owner, repo.name);
        let mut cookie: Option<String> = None;
        for _ in 0..MAX_REDIRECTS {
            let mut cookies = self.cookie_header(None);
            if let Some(c) = &cookie {
                cookies.push_str(&format!("; _csrf={c}"));
            }
            let resp = self
                .web
                .get(format!("{}{path}", self.base_url))
                .header(header::COOKIE, cookies)
                .send()
                .await
                .map_err(|e| Error::Network(e.without_url().to_string()))?;
            let status = resp.status();
            if let Some(c) = csrf_cookie(resp.headers()) {
                cookie = Some(c);
            }
            if status.is_redirection() {
                // On ne garde que le chemin : l'hôte d'une URL absolue peut différer de celui configuré.
                let next = resp
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(local_path);
                match next {
                    Some(p) if !p.starts_with("/user/login") => {
                        path = p;
                        continue;
                    }
                    _ => return Err(Error::SessionExpired),
                }
            }
            if Self::session_lost(status) {
                return Err(Error::SessionExpired);
            }
            if !status.is_success() {
                return Err(Error::Network(format!("HTTP {status} sur {path}")));
            }
            let page = resp
                .text()
                .await
                .map_err(|e| Error::Network(e.without_url().to_string()))?;
            let token = page
                .split("csrfToken: '")
                .nth(1)
                .and_then(|rest| rest.split('\'').next())
                .filter(|t| !t.is_empty());
            return match (cookie, token) {
                (Some(c), Some(t)) => Ok(WebCtx {
                    csrf: Some((c, t.to_owned())),
                }),
                _ => Err(Error::Decode(
                    "jeton CSRF introuvable (page modifiée dans cette version de Forgejo ?)".into(),
                )),
            };
        }
        Err(Error::Decode(
            "trop de redirections sur la page du run".into(),
        ))
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

        // 1. Position du job dans le run (le chemin veut l'index, pas l'id). Dès la 13, une
        //    requête sans `attempt` répond 500 : on part de la tentative 1, qui existe toujours.
        let first_attempt = flavor.uses_attempts().then_some(1);
        let first = self
            .job_view(&ctx, repo, number, 0, first_attempt, &[])
            .await?;
        let index = first
            .state
            .run
            .jobs
            .iter()
            .position(|j| j.name == pipeline.job)
            .ok_or_else(|| Error::NotFound(format!("job « {} »", pipeline.job)))?;

        // 2. Liste des étapes. Avec tentatives : on lit `allAttempts` sur la tentative 1 de ce
        //    job, puis on passe à la dernière si le job a été relancé.
        let view = if index == 0 {
            first
        } else {
            self.job_view(&ctx, repo, number, index, first_attempt, &[])
                .await?
        };
        let (attempt, view) = match first_attempt {
            None => (None, view),
            Some(_) => {
                let latest = view
                    .state
                    .current_job
                    .all_attempts
                    .iter()
                    .map(|a| a.number)
                    .max()
                    .unwrap_or(1);
                if latest == 1 {
                    (Some(1), view)
                } else {
                    let v = self
                        .job_view(&ctx, repo, number, index, Some(latest), &[])
                        .await?;
                    (Some(latest), v)
                }
            }
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
    const PAGE: &str = "/smithy/demo/actions/runs/7";
    const JOBS: [&str; 2] = ["build", "test"];

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

    fn steps_json() -> Value {
        json!([
            {"summary": "Set up job", "status": "success"},
            {"summary": "cargo test", "status": "failure"}
        ])
    }

    /// Lignes dans le désordre, avec ANSI et barre de progression.
    fn logs_json() -> Value {
        json!([
            {"step": 1, "cursor": 3, "lines": [
                {"index": 2, "message": "10%\r100%"},
                {"index": 1, "message": "\u{1b}[31mrouge\u{1b}[0m"},
                {"index": 3, "message": "fin"}]},
            {"step": 0, "cursor": 1, "lines": [{"index": 1, "message": "prêt"}]}
        ])
    }

    /// Annonce la version du serveur (détermine l'adaptateur).
    async fn mount_version(server: &MockServer, version: &str) {
        Mock::given(method("GET"))
            .and(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "version": version })))
            .mount(server)
            .await;
    }

    /// Réponse web sans protection CSRF (cookies de session seuls).
    async fn mock(server: &MockServer, p: &str, contains: &str, body: Value) {
        Mock::given(method("POST"))
            .and(path(p))
            .and(header("Cookie", "lang=fr; session=abc"))
            .and(body_string_contains(contains))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    /// Réponse web avec les deux moitiés du jeton CSRF.
    async fn mock_csrf(server: &MockServer, p: &str, contains: &str, body: Value) {
        Mock::given(method("POST"))
            .and(path(p))
            .and(header("Cookie", "lang=fr; session=abc; _csrf=ck123"))
            .and(header("X-Csrf-Token", "tok456"))
            .and(body_string_contains(contains))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    async fn posts(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| r.url.path().to_owned())
            .collect()
    }

    /// Mock d'une page du dépôt qui fournit le jeton CSRF.
    fn csrf_page() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .append_header("Set-Cookie", "_csrf=ck123; Path=/; Max-Age=86400")
            .set_body_string("<script>window.config = { csrfToken: 'tok456', x: 1 }</script>")
    }

    // --- Forgejo 14 et 15 : `Modern` (pas de CSRF, tentatives)

    #[tokio::test]
    async fn modern_second_job_avec_relance() {
        // Même comportement en 14 et en 15.
        for version in ["14.0.5+gitea-1.22.0", "15.0.10+gitea-1.22.0"] {
            let server = MockServer::start().await;
            mount_version(&server, version).await;
            // 1. liste des jobs : depuis la tentative 1 (sans `attempt` la 14 répond 500)
            mock(
                &server,
                &format!("{RUN}/0/attempt/1"),
                "\"logCursors\":[]",
                view(&JOBS, &[1], json!([]), json!([])),
            )
            .await;
            // 2. tentatives du job « test » (index 1) : la 2 est la dernière
            mock(
                &server,
                &format!("{RUN}/1/attempt/1"),
                "\"logCursors\":[]",
                view(&JOBS, &[1, 2], json!([]), json!([])),
            )
            .await;
            // 3. étapes de la tentative 2, puis 4. leur sortie
            mock(
                &server,
                &format!("{RUN}/1/attempt/2"),
                "\"logCursors\":[]",
                view(&JOBS, &[1, 2], steps_json(), json!([])),
            )
            .await;
            mock(
                &server,
                &format!("{RUN}/1/attempt/2"),
                "\"step\":1",
                view(&JOBS, &[1, 2], steps_json(), logs_json()),
            )
            .await;

            let log = forgejo(&server)
                .job_log(&repo(), &pipeline("test"))
                .await
                .unwrap();
            assert_eq!(log.notice, None, "{version} est vérifiée");
            assert_eq!(log.steps.len(), 2);
            assert_eq!(log.steps[0].status, Status::Success);
            assert_eq!(log.steps[0].lines, ["prêt"]);
            assert_eq!(log.steps[1].status, Status::Failure);
            assert_eq!(log.steps[1].lines, ["rouge", "100%", "fin"]);
            // Jamais d'appel sans `attempt`, et aucune page lue pour un jeton CSRF.
            assert!(
                posts(&server).await.iter().all(|p| p.contains("attempt")),
                "{version}"
            );
            assert!(
                server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .all(|r| r.url.path() != PAGE)
            );
        }
    }

    #[tokio::test]
    async fn modern_une_seule_tentative_economise_les_appels() {
        let server = MockServer::start().await;
        mount_version(&server, "15.0.10+gitea-1.22.0").await;
        // Job d'index 0, une tentative : liste des jobs = étapes, plus la sortie → 2 appels.
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "\"logCursors\":[]",
            view(&JOBS, &[1], steps_json(), json!([])),
        )
        .await;
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "\"step\":1",
            view(&JOBS, &[1], steps_json(), logs_json()),
        )
        .await;
        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("build"))
            .await
            .unwrap();
        assert_eq!(log.steps[1].lines, ["rouge", "100%", "fin"]);
        assert_eq!(posts(&server).await.len(), 2);
    }

    #[tokio::test]
    async fn job_sans_etape_donne_un_log_vide() {
        // Job annulé avant démarrage : steps vide, aucune demande de sortie.
        let server = MockServer::start().await;
        mount_version(&server, "15.0.10+gitea-1.22.0").await;
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
        assert_eq!(posts(&server).await.len(), 1);
    }

    // --- Forgejo 13 : `Transitional` (CSRF et tentatives)

    #[tokio::test]
    async fn forgejo_13_csrf_via_la_redirection_et_tentatives() {
        let server = MockServer::start().await;
        mount_version(&server, "13.0.5+gitea-1.22.0").await;
        // Dès la 13 la page du run redirige, avec une URL absolue vers un autre hôte :
        // seul le chemin compte. Le cookie `_csrf` est déjà posé par la redirection.
        Mock::given(method("GET"))
            .and(path(PAGE))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header(
                        "Location",
                        "http://forge.interne:3000/smithy/demo/actions/runs/7/jobs/0/attempt/1",
                    )
                    .append_header("Set-Cookie", "_csrf=ck123; Path=/; Max-Age=86400"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{RUN}/0/attempt/1")))
            .and(header("Cookie", "lang=fr; session=abc; _csrf=ck123"))
            .respond_with(csrf_page())
            .mount(&server)
            .await;
        mock_csrf(
            &server,
            &format!("{RUN}/0/attempt/1"),
            "\"logCursors\":[]",
            view(&JOBS, &[1], json!([]), json!([])),
        )
        .await;
        mock_csrf(
            &server,
            &format!("{RUN}/1/attempt/1"),
            "\"logCursors\":[]",
            view(&JOBS, &[1], steps_json(), json!([])),
        )
        .await;
        mock_csrf(
            &server,
            &format!("{RUN}/1/attempt/1"),
            "\"step\":1",
            view(&JOBS, &[1], steps_json(), logs_json()),
        )
        .await;

        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("test"))
            .await
            .unwrap();
        assert_eq!(log.notice, None);
        assert_eq!(log.steps[1].lines, ["rouge", "100%", "fin"]);
    }

    #[tokio::test]
    async fn redirection_vers_le_login_donne_session_expiree() {
        let server = MockServer::start().await;
        mount_version(&server, "13.0.5+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .and(path(PAGE))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/user/login"))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("test")).await;
        assert!(matches!(r, Err(Error::SessionExpired)), "{r:?}");
    }

    #[tokio::test]
    async fn boucle_de_redirections() {
        let server = MockServer::start().await;
        mount_version(&server, "13.0.5+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", PAGE))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("test")).await;
        assert!(
            matches!(r, Err(Error::Decode(ref m)) if m.contains("redirections")),
            "{r:?}"
        );
    }

    // --- Forgejo 10 à 12 : `Legacy` (CSRF, pas de tentative)

    #[tokio::test]
    async fn forgejo_10_jeton_csrf_et_pas_de_tentative() {
        let server = MockServer::start().await;
        mount_version(&server, "10.0.3+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .and(path(PAGE))
            .and(header("Cookie", "lang=fr; session=abc"))
            .respond_with(csrf_page())
            .mount(&server)
            .await;
        mock_csrf(
            &server,
            &format!("{RUN}/0"),
            "\"logCursors\":[]",
            view(&JOBS, &[], json!([]), json!([])),
        )
        .await;
        mock_csrf(
            &server,
            &format!("{RUN}/1"),
            "\"logCursors\":[]",
            view(&JOBS, &[], steps_json(), json!([])),
        )
        .await;
        mock_csrf(
            &server,
            &format!("{RUN}/1"),
            "\"step\":1",
            view(&JOBS, &[], steps_json(), logs_json()),
        )
        .await;

        let log = forgejo(&server)
            .job_log(&repo(), &pipeline("test"))
            .await
            .unwrap();
        assert_eq!(log.notice, None, "version vérifiée : aucune réserve");
        assert_eq!(log.steps[1].lines, ["rouge", "100%", "fin"]);
        assert!(
            posts(&server).await.iter().all(|p| !p.contains("attempt")),
            "pas d'attempt en 10 à 12"
        );
    }

    #[tokio::test]
    async fn forgejo_10_session_perdue_a_la_lecture_de_la_page() {
        let server = MockServer::start().await;
        mount_version(&server, "11.0.16+gitea-1.22.0").await;
        Mock::given(method("GET"))
            .and(path(PAGE))
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
            .and(path(PAGE))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>rien</html>"))
            .mount(&server)
            .await;
        let r = forgejo(&server).job_log(&repo(), &pipeline("test")).await;
        assert!(
            matches!(r, Err(Error::Decode(ref m)) if m.contains("CSRF")),
            "{r:?}"
        );
    }

    // --- Cas communs

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
        mount_version(&server, "15.0.10+gitea-1.22.0").await;
        mock(
            &server,
            &format!("{RUN}/0/attempt/1"),
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
        mount_version(&server, "16.0.0+gitea-1.22.0").await;
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
        assert!(
            log.notice.as_deref().unwrap().contains("plus récente"),
            "{:?}",
            log.notice
        );
    }

    #[tokio::test]
    async fn version_illisible_ou_muette_donne_un_support_partiel() {
        // /version absent (404) : on essaie quand même, avec une réserve.
        let server = MockServer::start().await;
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
        assert!(log.notice.is_some());
    }

    #[test]
    fn chemin_d_une_redirection() {
        assert_eq!(local_path("/a/b?x=1").as_deref(), Some("/a/b?x=1"));
        assert_eq!(local_path("http://h:3000/a/b").as_deref(), Some("/a/b"));
        assert_eq!(
            local_path("https://h/a/b?x=1#f").as_deref(),
            Some("/a/b?x=1")
        );
        assert_eq!(local_path("pas une url"), None);
    }

    #[test]
    fn cookie_csrf_ignore_si_vide_ou_supprime() {
        let headers = |vals: &[&str]| {
            let mut h = header::HeaderMap::new();
            for v in vals {
                h.append(
                    header::SET_COOKIE,
                    header::HeaderValue::from_str(v).unwrap(),
                );
            }
            h
        };
        assert_eq!(
            csrf_cookie(&headers(&["_csrf=abc; Path=/"])).as_deref(),
            Some("abc")
        );
        assert_eq!(csrf_cookie(&headers(&["_csrf=; Path=/; Max-Age=0"])), None);
        assert_eq!(
            csrf_cookie(&headers(&["_csrf=x; Max-Age=0", "lang=fr"])),
            None
        );
        assert_eq!(csrf_cookie(&headers(&["lang=fr"])), None);
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
