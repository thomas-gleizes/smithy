//! Backend Forgejo. API REST validée sur Forgejo 15 : `/user/repos` et
//! `/repos/{o}/{r}/actions/tasks`. Les logs, eux, passent par les routes web (voir `logs`),
//! qui demandent un cookie de session (voir `login`).

mod login;
mod logs;

pub use login::{Login, login};

use reqwest::StatusCode;
use serde::{Deserialize, de::DeserializeOwned};
use smithy_core::{Capabilities, Error, JobLog, Pipeline, Provider, Repo, Result, Status};
use std::collections::BTreeMap;

const PAGE_SIZE: usize = 50;
const MAX_PAGES: usize = 20;

pub struct Forgejo {
    base_url: String,
    token: String,
    client: reqwest::Client,
    /// Pour les routes web : on ne suit pas les redirections, c'est le signe d'une session perdue.
    web: reqwest::Client,
    /// Cookies de session (obtenus par `login`) ; vides tant que l'utilisateur ne s'est pas connecté.
    cookies: BTreeMap<String, String>,
}

impl Forgejo {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        let builder = || {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        };
        let expect = "configuration du client HTTP statique et valide";
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token: token.into(),
            client: builder().build().expect(expect),
            web: builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect(expect),
            cookies: BTreeMap::new(),
        }
    }

    pub fn with_cookies(mut self, cookies: BTreeMap<String, String>) -> Self {
        self.cookies = cookies;
        self
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.base_url, path)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let resp = self
            .client
            .get(self.api(path))
            .header("Authorization", format!("token {}", self.token))
            .query(query)
            .send()
            .await
            .map_err(|e| Error::Network(e.without_url().to_string()))?;

        match resp.status() {
            s if s.is_success() => resp.json().await.map_err(|e| Error::Decode(e.to_string())),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Unauthorized),
            StatusCode::NOT_FOUND => Err(Error::NotFound(path.to_owned())),
            s => Err(Error::Network(format!("HTTP {s} sur {path}"))),
        }
    }
}

#[derive(Deserialize)]
struct ApiRepo {
    name: String,
    owner: ApiOwner,
}

#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}

#[derive(Deserialize)]
struct ApiTasks {
    #[serde(default)]
    workflow_runs: Vec<ApiRun>,
}

#[derive(Deserialize)]
struct ApiRun {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    run_number: u64,
    #[serde(default)]
    workflow_id: String,
    #[serde(default)]
    display_title: String,
    #[serde(default)]
    head_branch: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    url: String,
}

fn map_status(s: &str) -> Status {
    match s {
        "waiting" | "blocked" => Status::Pending,
        "running" => Status::Running,
        "success" => Status::Success,
        "failure" => Status::Failure,
        "cancelled" => Status::Cancelled,
        "skipped" => Status::Skipped,
        _ => Status::Unknown,
    }
}

/// Numéro du run, lu dans l'URL (`…/actions/runs/3`) si l'API ne le donne pas.
fn run_number(run_number: u64, url: &str) -> u64 {
    if run_number != 0 {
        return run_number;
    }
    url.trim_end_matches('/')
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

impl From<ApiRun> for Pipeline {
    fn from(r: ApiRun) -> Self {
        Pipeline {
            id: r.id,
            number: run_number(r.run_number, &r.url),
            job: r.name,
            workflow: r.workflow_id,
            title: r.display_title,
            branch: r.head_branch,
            commit: r.head_sha,
            status: map_status(&r.status),
            url: (!r.url.is_empty()).then_some(r.url),
        }
    }
}

impl Provider for Forgejo {
    fn capabilities(&self) -> Capabilities {
        Capabilities { logs: true }
    }

    async fn list_repos(&self) -> Result<Vec<Repo>> {
        let mut repos = Vec::new();
        for page in 1..=MAX_PAGES {
            let batch: Vec<ApiRepo> = self
                .get(
                    "/user/repos",
                    &[("limit", PAGE_SIZE.to_string()), ("page", page.to_string())],
                )
                .await?;
            let n = batch.len();
            repos.extend(batch.into_iter().map(|r| Repo {
                owner: r.owner.login,
                name: r.name,
            }));
            if n < PAGE_SIZE {
                break;
            }
        }
        Ok(repos)
    }

    async fn list_pipelines(&self, repo: &Repo) -> Result<Vec<Pipeline>> {
        let path = format!("/repos/{}/{}/actions/tasks", repo.owner, repo.name);
        let tasks: ApiTasks = self.get(&path, &[("limit", PAGE_SIZE.to_string())]).await?;
        Ok(tasks
            .workflow_runs
            .into_iter()
            .map(Pipeline::from)
            .collect())
    }

    async fn job_log(&self, repo: &Repo, pipeline: &Pipeline) -> Result<JobLog> {
        self.fetch_job_log(repo, pipeline).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo() -> Repo {
        Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        }
    }

    #[test]
    fn api_url_normalise_le_slash_final() {
        let f = Forgejo::new("https://git.example.com/", "t");
        assert_eq!(f.api("/user"), "https://git.example.com/api/v1/user");
    }

    #[test]
    fn statuts_connus() {
        assert_eq!(map_status("waiting"), Status::Pending);
        assert_eq!(map_status("running"), Status::Running);
        assert_eq!(map_status("success"), Status::Success);
        assert_eq!(map_status("failure"), Status::Failure);
        assert_eq!(map_status("nimportequoi"), Status::Unknown);
    }

    #[tokio::test]
    async fn liste_les_pipelines_depuis_une_reponse_reelle() {
        let server = MockServer::start().await;
        // Réponse capturée sur Forgejo 15 (champs inutiles conservés pour tester la tolérance).
        let body = serde_json::json!({
            "total_count": 2,
            "workflow_runs": [
                {"id": 2, "name": "build", "head_branch": "main", "head_sha": "abc",
                 "run_number": 3, "event": "push", "display_title": "ci: ok", "status": "success",
                 "workflow_id": "ok.yml", "url": "http://x/runs/3",
                 "created_at": "2026-10-13T09:49:22Z"},
                {"id": 1, "head_branch": "main", "head_sha": "abc", "status": "failure",
                 "workflow_id": "fail.yml"}
            ]
        });
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/demo/actions/tasks"))
            .and(header("Authorization", "token secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let f = Forgejo::new(server.uri(), "secret");
        let p = f.list_pipelines(&repo()).await.unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].status, Status::Success);
        assert_eq!(p[0].workflow, "ok.yml");
        assert_eq!(p[0].url.as_deref(), Some("http://x/runs/3"));
        assert_eq!((p[0].number, p[0].job.as_str()), (3, "build"));
        // Sans `run_number`, le numéro est lu dans l'URL ; sinon 0 (inconnu).
        assert_eq!(run_number(0, "http://x/o/r/actions/runs/12"), 12);
        assert_eq!(run_number(0, "http://x/o/r/actions/runs/12/"), 12);
        assert_eq!(run_number(0, ""), 0);
        assert_eq!(p[1].number, 0);
        assert_eq!(p[1].status, Status::Failure);
        assert_eq!(p[1].url, None);
    }

    #[tokio::test]
    async fn pagine_les_depots() {
        let server = MockServer::start().await;
        let page = |n: usize, prefix: &str| {
            let items: Vec<_> = (0..n)
                .map(|i| serde_json::json!({"name": format!("{prefix}{i}"), "owner": {"login": "o"}}))
                .collect();
            ResponseTemplate::new(200).set_body_json(items)
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/user/repos"))
            .and(query_param("page", "1"))
            .respond_with(page(PAGE_SIZE, "a"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user/repos"))
            .and(query_param("page", "2"))
            .respond_with(page(3, "b"))
            .mount(&server)
            .await;

        let repos = Forgejo::new(server.uri(), "t").list_repos().await.unwrap();
        assert_eq!(repos.len(), PAGE_SIZE + 3);
    }

    #[tokio::test]
    async fn erreurs_http_traduites() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/demo/actions/tasks"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let f = Forgejo::new(server.uri(), "bad");
        assert!(matches!(
            f.list_pipelines(&repo()).await,
            Err(Error::Unauthorized)
        ));

        let other = Repo {
            owner: "x".into(),
            name: "y".into(),
        };
        assert!(matches!(
            f.list_pipelines(&other).await,
            Err(Error::NotFound(_))
        ));
    }
}
