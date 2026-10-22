//! Backend GitLab (gitlab.com ou instance auto-hébergée). Tout passe par l'API REST v4 avec un
//! jeton `Authorization: Bearer` : un PAT et un jeton OAuth y passent tous les deux, mais un jeton
//! OAuth répond 401 en `PRIVATE-TOKEN` (mesuré). Ni cookie ni route web : la route web `.diff`
//! redirige vers la page de connexion même avec un jeton. Comportement mesuré avec
//! `dev/probe-gitlab.py` (voir CLAUDE.md, « GitLab »).
//!
//! Quota : 100 requêtes par minute pour un compte gratuit. Aucune requête n'est faite « par run » :
//! la liste des runs est un seul appel (`/jobs`, où chaque job porte son pipeline et son commit).

mod ci;
mod code;
pub mod oauth;
mod pulls;
mod repos;
mod trace;

use std::time::Duration;

use reqwest::{StatusCode, header};
use serde::de::DeserializeOwned;
use smithy_core::{
    Branch, Capabilities, CommitDetail, CommitPage, Entry, Error, FileContent, JobLog, Pipeline,
    Provider, PullDetail, PullFilter, PullPage, Repo, RepoInfo, RepoSource, Result,
};

/// gitlab.com ; une instance auto-hébergée se configure avec `https://<hôte>`.
pub const DEFAULT_URL: &str = "https://gitlab.com";
/// Plafond de `per_page` (mesuré : 101 donne 100).
const MAX_PER_PAGE: usize = 100;

/// Ce qu'il faut pour interroger l'API ; clonable pour lancer des requêtes en parallèle.
#[derive(Clone)]
struct Http {
    /// `https://gitlab.com/api/v4`
    api: String,
    token: String,
    client: reqwest::Client,
}

pub struct GitLab {
    http: Http,
}

/// L'adresse du serveur sans `/` final ni `/api/v4` (on accepte l'une ou l'autre en configuration).
fn base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix("/api/v4").unwrap_or(url);
    if url.is_empty() {
        DEFAULT_URL.to_owned()
    } else {
        url.to_owned()
    }
}

impl GitLab {
    /// `url` : l'adresse du serveur (vide = gitlab.com).
    pub fn new(url: impl AsRef<str>, token: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("configuration du client HTTP statique et valide");
        Self {
            http: Http {
                api: format!("{}/api/v4", base_url(url.as_ref())),
                token: token.into(),
                client,
            },
        }
    }
}

impl Http {
    fn request(&self, path: &str, query: &[(&str, String)]) -> reqwest::RequestBuilder {
        self.client
            .get(format!("{}{path}", self.api))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .query(query)
    }

    /// GET JSON.
    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        Ok(self.get_page(path, query).await?.0)
    }

    /// GET JSON et numéro de la page suivante (`x-next-page` vide : c'était la dernière).
    async fn get_page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(T, Option<u32>)> {
        let resp = check(self.request(path, query).send().await.map_err(net)?, path).await?;
        let next = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let body = resp.bytes().await.map_err(net)?;
        Ok((decode(&body)?, next))
    }

    /// Valeur d'un en-tête de la réponse à un GET (par exemple `x-total`).
    async fn get_header(
        &self,
        path: &str,
        query: &[(&str, String)],
        name: &str,
    ) -> Result<Option<String>> {
        let resp = check(self.request(path, query).send().await.map_err(net)?, path).await?;
        Ok(resp
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned))
    }

    /// GET d'un contenu brut (fichier, diff, trace), lu sur au plus `max_bytes` octets.
    /// Renvoie les octets, la taille annoncée et si le plafond a coupé la lecture.
    async fn get_raw(
        &self,
        path: &str,
        query: &[(&str, String)],
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<(Vec<u8>, Option<u64>, bool)> {
        let mut resp = check(
            self.request(path, query)
                .timeout(timeout)
                .send()
                .await
                .map_err(net)?,
            path,
        )
        .await?;
        let announced = resp.content_length();
        let mut buf: Vec<u8> = Vec::new();
        let mut truncated = false;
        while let Some(chunk) = resp.chunk().await.map_err(net)? {
            buf.extend_from_slice(&chunk);
            if buf.len() > max_bytes {
                buf.truncate(max_bytes);
                truncated = true;
                break;
            }
        }
        Ok((buf, announced, truncated))
    }
}

fn net(e: reqwest::Error) -> Error {
    Error::Network(e.without_url().to_string())
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|e| Error::Decode(e.to_string()))
}

/// Dans combien de minutes revient le quota : `Retry-After` (secondes) sinon `RateLimit-Reset`
/// (heure Unix de la reprise), tels que GitLab les envoie.
fn resume_in_minutes(resp: &reqwest::Response) -> Option<u64> {
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    };
    if let Some(secs) = header("retry-after") {
        return Some(secs.div_ceil(60));
    }
    let reset = header("ratelimit-reset")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Some(reset.saturating_sub(now).div_ceil(60))
}

/// Traduit les codes d'erreur. Un 429 n'est pas un refus d'accès : on dit quand le quota revient
/// (son corps est du texte brut, pas du JSON).
async fn check(resp: reqwest::Response, path: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let reset = resume_in_minutes(&resp)
            .map(|m| format!(" (reprise dans {} min)", m.max(1)))
            .unwrap_or_default();
        return Err(Error::Network(format!(
            "quota de l'API GitLab épuisé{reset}"
        )));
    }
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Unauthorized),
        StatusCode::NOT_FOUND => Err(Error::NotFound(path.to_owned())),
        s => {
            // `message` (chaîne ou objet) ou `error` : court et utile.
            let msg = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    let m = v.get("message").or_else(|| v.get("error"))?;
                    Some(
                        m.as_str()
                            .map_or_else(|| m.to_string(), str::to_owned)
                            .chars()
                            .take(120)
                            .collect::<String>(),
                    )
                })
                .unwrap_or_default();
            Err(Error::Network(
                format!("HTTP {} sur {path} {msg}", s.as_u16())
                    .trim_end()
                    .to_owned(),
            ))
        }
    }
}

/// Encode un composant d'URL, `/` compris : un chemin de projet (`groupe/sous-groupe/projet`) ou de
/// fichier tient en un seul segment (`a%2Fb`).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// L'identifiant d'un projet dans une URL de l'API : son chemin complet encodé.
fn project(repo: &Repo) -> String {
    encode(&repo.full_name())
}

/// Sonde de joignabilité, sans jeton : `/version` répond 401 sans authentification, ce qui prouve
/// que le serveur est là.
pub async fn ping(url: &str) -> std::result::Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let resp = client
        .get(format!("{}/api/v4/version", base_url(url)))
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "délai dépassé".to_owned()
            } else if e.is_connect() {
                "connexion impossible".to_owned()
            } else {
                e.without_url().to_string().chars().take(40).collect()
            }
        })?;
    let status = resp.status();
    if status.is_server_error() {
        return Err(format!("erreur HTTP {}", status.as_u16()));
    }
    if status == StatusCode::NOT_FOUND {
        return Err("pas une API GitLab (404)".to_owned());
    }
    // 401/403/429 : l'API répond.
    Ok(None)
}

impl Provider for GitLab {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            logs: true,
            browse: true,
        }
    }

    async fn list_repos(&self, source: &RepoSource) -> Result<Vec<RepoInfo>> {
        self.http.fetch_repos(source).await
    }

    async fn repo_info(&self, repo: &Repo) -> Result<RepoInfo> {
        self.http.fetch_repo_info(repo).await
    }

    async fn list_pipelines(&self, repo: &Repo) -> Result<Vec<Pipeline>> {
        self.http.fetch_pipelines(repo).await
    }

    async fn job_log(&self, repo: &Repo, pipeline: &Pipeline) -> Result<JobLog> {
        self.http.fetch_job_log(repo, pipeline).await
    }

    async fn list_branches(&self, repo: &Repo) -> Result<Vec<Branch>> {
        self.http.fetch_branches(repo).await
    }

    async fn list_commits(&self, repo: &Repo, reference: &str, page: u32) -> Result<CommitPage> {
        self.http.fetch_commits(repo, reference, page).await
    }

    async fn commit_detail(
        &self,
        repo: &Repo,
        sha: &str,
        max_bytes: usize,
    ) -> Result<CommitDetail> {
        self.http.fetch_commit_detail(repo, sha, max_bytes).await
    }

    async fn read_dir(&self, repo: &Repo, reference: &str, path: &str) -> Result<Vec<Entry>> {
        self.http.fetch_dir(repo, reference, path).await
    }

    async fn read_file(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<FileContent> {
        self.http.fetch_file(repo, reference, path, max_bytes).await
    }

    async fn list_pulls(&self, repo: &Repo, filter: PullFilter, page: u32) -> Result<PullPage> {
        self.http.fetch_pulls(repo, filter, page).await
    }

    async fn pull_detail(&self, repo: &Repo, number: u64, max_bytes: usize) -> Result<PullDetail> {
        self.http.fetch_pull_detail(repo, number, max_bytes).await
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use wiremock::MockServer;

    pub fn gitlab(server: &MockServer) -> GitLab {
        GitLab::new(server.uri(), "secret")
    }

    /// Un projet dans un sous-groupe, comme GitLab en a : le propriétaire contient un `/`.
    pub fn repo() -> Repo {
        Repo {
            owner: "groupe/sous".into(),
            name: "app".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn adresse_et_identifiant_de_projet() {
        assert_eq!(GitLab::new("", "t").http.api, "https://gitlab.com/api/v4");
        assert_eq!(
            GitLab::new("https://git.example/", "t").http.api,
            "https://git.example/api/v4"
        );
        assert_eq!(
            GitLab::new("https://git.example/api/v4/", "t").http.api,
            "https://git.example/api/v4",
            "le suffixe saisi est accepté"
        );
        assert_eq!(project(&repo()), "groupe%2Fsous%2Fapp");
        assert_eq!(encode("a b/#1?.txt"), "a%20b%2F%231%3F.txt");
        assert_eq!(MAX_PER_PAGE, 100);
    }

    #[tokio::test]
    async fn jeton_en_bearer_et_projet_par_chemin_encode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v4/projects/groupe%2Fsous%2Fapp"))
            .and(header("Authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "path_with_namespace": "groupe/sous/app", "name": "app",
                "visibility": "private", "default_branch": "main"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let info = gitlab(&server).repo_info(&repo()).await.unwrap();
        assert_eq!(info.repo, repo());
        assert!(info.private);
    }

    #[tokio::test]
    async fn erreurs_traduites_et_quota_en_texte_brut() {
        let server = MockServer::start().await;
        for (name, status) in [("refus", 403), ("jeton", 401), ("rien", 404)] {
            Mock::given(path(format!("/api/v4/projects/o%2F{name}")))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
        }
        Mock::given(path("/api/v4/projects/o%2Fquota"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "90")
                    .set_body_string("Rate limit exceeded; see https://docs.gitlab.com"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/api/v4/projects/o%2Fpanne"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"message": "oups"})))
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let info = |name: &str| Repo {
            owner: "o".into(),
            name: name.into(),
        };
        let quota = g.repo_info(&info("quota")).await.unwrap_err();
        assert!(
            quota.to_string().contains("quota") && quota.to_string().contains("2 min"),
            "{quota}"
        );
        assert!(matches!(
            g.repo_info(&info("refus")).await,
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            g.repo_info(&info("jeton")).await,
            Err(Error::Unauthorized)
        ));
        assert!(matches!(
            g.repo_info(&info("rien")).await,
            Err(Error::NotFound(_))
        ));
        let panne = g.repo_info(&info("panne")).await.unwrap_err();
        assert!(panne.to_string().contains("500") && panne.to_string().contains("oups"));
    }

    #[tokio::test]
    async fn sonde_sans_jeton_un_401_prouve_que_le_serveur_repond() {
        let server = MockServer::start().await;
        Mock::given(path("/api/v4/version"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert_eq!(ping(&server.uri()).await, Ok(None));
        let down = MockServer::start().await;
        Mock::given(path("/api/v4/version"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&down)
            .await;
        assert!(ping(&down.uri()).await.unwrap_err().contains("503"));
        let other = MockServer::start().await;
        assert!(ping(&other.uri()).await.unwrap_err().contains("404"));
    }
}
