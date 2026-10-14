//! Backend GitHub (et GitHub Enterprise : `https://<hôte>/api/v3`). Tout passe par l'API REST
//! documentée avec un token : ni cookie ni route web, contrairement à Forgejo. Comportement
//! mesuré avec `dev/probe-github.py` (voir CLAUDE.md, « GitHub »).
//!
//! Quota : 5000 requêtes par heure. Les requêtes conditionnelles (`If-None-Match` → 304) ne
//! comptent pas : chaque réponse JSON est gardée avec son `ETag`, et redemander une liste qui
//! n'a pas changé (le rafraîchissement des runs toutes les 5 s) ne coûte rien.

mod actions;
mod code;
mod pulls;
mod repos;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::{StatusCode, header};
use serde::de::DeserializeOwned;
use smithy_core::{
    Branch, Capabilities, CommitDetail, CommitPage, Entry, Error, FileContent, JobLog, Pipeline,
    Provider, PullDetail, PullFilter, PullPage, Repo, RepoInfo, RepoSource, Result,
};

/// L'API publique ; une instance GitHub Enterprise se configure avec `https://<hôte>/api/v3`.
pub const DEFAULT_API: &str = "https://api.github.com";
/// Plafond de `per_page` (101 donne 100, mesuré).
const MAX_PER_PAGE: usize = 100;
/// Au-delà, on ne garde plus de réponses : le cache repart de zéro.
const MAX_CACHED: usize = 512;
const API_VERSION: &str = "2022-11-28";

/// Une réponse JSON gardée avec son `ETag`.
type Cache = Arc<Mutex<HashMap<String, (String, Vec<u8>)>>>;

/// Ce qu'il faut pour interroger l'API ; clonable pour lancer des requêtes en parallèle.
#[derive(Clone)]
struct Http {
    api: String,
    token: String,
    client: reqwest::Client,
    cache: Cache,
}

pub struct GitHub {
    http: Http,
}

impl GitHub {
    pub fn new(api_url: impl Into<String>, token: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
            // Les logs redirigent vers une URL signée d'un autre hôte : reqwest y retire
            // l'en-tête `Authorization` (avec, le stockage répond 401, mesuré).
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .expect("configuration du client HTTP statique et valide");
        let api = api_url.into();
        let api = if api.trim().is_empty() {
            DEFAULT_API.to_owned()
        } else {
            api.trim_end_matches('/').to_owned()
        };
        Self {
            http: Http {
                api,
                token: token.into(),
                client,
                cache: Arc::default(),
            },
        }
    }
}

impl Http {
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api, path)
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        self.client
            .get(url)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header("X-GitHub-Api-Version", API_VERSION)
    }

    /// GET JSON avec le cache d'`ETag` : une réponse 304 rend la copie gardée, sans coût.
    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let mut url = reqwest::Url::parse(&self.url(path))
            .map_err(|e| Error::Network(format!("adresse invalide : {e}")))?;
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        let key = url.to_string();
        let cached = self.cache.lock().ok().and_then(|c| c.get(&key).cloned());
        let mut req = self
            .request(url.as_str())
            .header(header::ACCEPT, "application/vnd.github+json");
        if let Some((etag, _)) = &cached {
            req = req.header(header::IF_NONE_MATCH, etag);
        }
        let resp = req.send().await.map_err(net)?;
        if resp.status() == StatusCode::NOT_MODIFIED
            && let Some((_, body)) = cached
        {
            return decode(&body);
        }
        let resp = check(resp, path).await?;
        let etag = resp
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = resp.bytes().await.map_err(net)?.to_vec();
        let value = decode(&body)?;
        if let (Some(etag), Ok(mut cache)) = (etag, self.cache.lock()) {
            if cache.len() >= MAX_CACHED {
                cache.clear();
            }
            cache.insert(key, (etag, body));
        }
        Ok(value)
    }

    /// GET d'un contenu brut (fichier, diff, log), lu sur au plus `max_bytes` octets.
    /// Renvoie les octets, la taille annoncée et si le plafond a coupé la lecture.
    async fn get_raw(
        &self,
        path: &str,
        query: &[(&str, String)],
        accept: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<(Vec<u8>, Option<u64>, bool)> {
        let mut resp = check(
            self.request(&self.url(path))
                .header(header::ACCEPT, accept)
                .query(query)
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

/// Traduit les codes d'erreur. Un 403 (ou 429) avec un quota épuisé n'est pas un refus d'accès :
/// on dit quand il revient.
async fn check(resp: reqwest::Response, path: &str) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let exhausted = header("x-ratelimit-remaining").as_deref() == Some("0");
    if exhausted || status == StatusCode::TOO_MANY_REQUESTS {
        let reset = header("x-ratelimit-reset")
            .and_then(|r| r.parse::<i64>().ok())
            .map(|t| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64);
                format!(" (reprise dans {} min)", ((t - now).max(0) + 59) / 60)
            })
            .unwrap_or_default();
        return Err(Error::Network(format!(
            "quota de l'API GitHub épuisé{reset}"
        )));
    }
    match status {
        StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
        StatusCode::FORBIDDEN => Err(Error::Unauthorized),
        StatusCode::NOT_FOUND => Err(Error::NotFound(path.to_owned())),
        s => {
            // Le message de GitHub est court et utile (« Git Repository is empty. »…).
            let msg = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("message")?.as_str().map(str::to_owned))
                .unwrap_or_default();
            Err(Error::Network(
                format!("HTTP {} sur {path} {msg}", s.as_u16())
                    .trim_end()
                    .to_owned(),
            ))
        }
    }
}

/// Encode chaque segment d'un chemin de fichier pour l'URL (les `/` restent).
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Sonde de joignabilité, sans token : l'API répond-elle ?
pub async fn ping(api_url: &str) -> std::result::Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let api = if api_url.trim().is_empty() {
        DEFAULT_API
    } else {
        api_url.trim_end_matches('/')
    };
    let resp = client
        .get(format!("{api}/meta"))
        .header(header::ACCEPT, "application/vnd.github+json")
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
        return Err("pas une API GitHub (404)".to_owned());
    }
    // 401/403/429 : l'API répond (quota sans token, instance privée).
    Ok(None)
}

impl Provider for GitHub {
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

    pub fn github(server: &MockServer) -> GitHub {
        GitHub::new(server.uri(), "secret")
    }

    pub fn repo() -> Repo {
        Repo {
            owner: "octo".into(),
            name: "app".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn une_liste_inchangee_revient_du_cache_par_etag() {
        let server = MockServer::start().await;
        // Deuxième appel : l'ETag est renvoyé, le serveur répond 304 sans corps
        Mock::given(method("GET"))
            .and(path("/repos/octo/app"))
            .and(header("If-None-Match", "\"v1\""))
            .respond_with(ResponseTemplate::new(304))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/octo/app"))
            .and(header("Authorization", "Bearer secret"))
            .and(header("X-GitHub-Api-Version", API_VERSION))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("ETag", "\"v1\"")
                    .set_body_json(json!({"name": "app", "owner": {"login": "octo"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let g = github(&server);
        let a = g.repo_info(&repo()).await.unwrap();
        let b = g.repo_info(&repo()).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(b.repo.full_name(), "octo/app");
    }

    #[tokio::test]
    async fn erreurs_traduites_et_quota_epuise() {
        let server = MockServer::start().await;
        Mock::given(path("/repos/octo/quota"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", "0"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/refus"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/jeton"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/rien"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let g = github(&server);
        let info = |name: &str| Repo {
            owner: "octo".into(),
            name: name.into(),
        };
        let quota = g.repo_info(&info("quota")).await.unwrap_err();
        assert!(quota.to_string().contains("quota"), "{quota}");
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
    }

    #[tokio::test]
    async fn sonde_sans_token() {
        let server = MockServer::start().await;
        Mock::given(path("/meta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        assert_eq!(ping(&server.uri()).await, Ok(None));
        let down = MockServer::start().await;
        Mock::given(path("/meta"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&down)
            .await;
        assert!(ping(&down.uri()).await.unwrap_err().contains("503"));
        let _ = header_exists("x");
    }

    #[test]
    fn url_par_defaut_et_chemins_encodes() {
        assert_eq!(GitHub::new("", "t").http.api, DEFAULT_API);
        assert_eq!(
            GitHub::new("https://ghe.example/api/v3/", "t").http.api,
            "https://ghe.example/api/v3"
        );
        assert_eq!(encode_path("a b/#1?.txt"), "a%20b/%231%3F.txt");
        assert_eq!(MAX_PER_PAGE, 100);
    }
}
