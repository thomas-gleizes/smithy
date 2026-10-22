//! Connexion par code (« device flow » OAuth, GitLab 17.9+) : l'application affiche un code,
//! l'utilisateur l'autorise sur `<hôte>/oauth/device` depuis n'importe quel navigateur, et smithy
//! reçoit un jeton. Ni secret ni redirection : pratique sur une machine sans bureau, et sans les
//! permissions à cocher une à une d'un jeton personnel granulaire (qui ne se modifie pas).
//!
//! Les routes sont sur l'hôte **web** (`/oauth/…`), pas sous `/api/v4`. Mesuré (2026-10-22) :
//! - il faut une application **publique** (« Confidential » décoché), sinon 401 `invalid_client` ;
//! - le code vit 300 s, l'intervalle est de 5 s ; l'attente répond **HTTP 400**
//!   `authorization_pending` (et non 200 comme GitHub) ;
//! - le jeton vit 7200 s et un `refresh_token` est fourni : `refresh` en obtient un nouveau sans
//!   que l'utilisateur ait à rien refaire. Le jeton ne passe qu'en `Authorization: Bearer`.

use std::time::Duration;

use reqwest::header;
use serde::Deserialize;

use crate::base_url;

/// Droit demandé : la lecture de l'API, rien d'autre.
pub const SCOPE: &str = "read_api";

/// Identifiant public de l'application OAuth de smithy sur gitlab.com (il n'est pas secret).
/// `SMITHY_GITLAB_CLIENT_ID` le remplace, notamment pour une instance auto-hébergée.
pub const DEFAULT_CLIENT_ID: &str =
    "fe57a4bebc8a292273e9a92e8a2a6f3a9d268a384b7856c4f709d9f53f2d1eb8";

const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Rallonge imposée par `slow_down`.
const SLOW_DOWN_STEP: u64 = 5;

/// Ce qu'il faut montrer à l'utilisateur, et ce que `poll_token` consomme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// Secondes à attendre entre deux interrogations.
    pub interval: u64,
}

/// Un jeton d'accès et de quoi le renouveler.
#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Durée de vie en secondes (7200 sur gitlab.com).
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Jamais de secret dans un journal ou un message d'erreur.
        f.debug_struct("Token")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// Hôte web : c'est l'adresse du serveur elle-même.
pub fn web_base(url: &str) -> String {
    base_url(url)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.without_url().to_string())
}

fn net_error(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "délai dépassé".to_owned()
    } else if e.is_connect() {
        "connexion impossible".to_owned()
    } else {
        e.without_url().to_string().chars().take(60).collect()
    }
}

/// Les réponses d'OAuth : un jeton, un code, ou une erreur (avec un statut HTTP 4xx).
#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    interval: Option<u64>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    device_code: Option<String>,
    #[serde(default)]
    user_code: Option<String>,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn post(web: &str, path: &str, form: &[(&str, &str)]) -> Result<Reply, String> {
    let resp = client()?
        .post(format!("{}{path}", web.trim_end_matches('/')))
        .header(header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(net_error)?;
    let status = resp.status();
    let body = resp.text().await.map_err(net_error)?;
    serde_json::from_str::<Reply>(&body).map_err(|_| format!("réponse inattendue (HTTP {status})"))
}

fn explain(error: &str, description: Option<&str>) -> String {
    match error {
        "invalid_client" => {
            "identifiant client inconnu de GitLab, ou application « Confidential » (à décocher)"
                .to_owned()
        }
        "invalid_grant" => "autorisation expirée ou déjà utilisée : reconnectez-vous".to_owned(),
        "expired_token" => "le code a expiré : recommencez".to_owned(),
        "access_denied" => "autorisation refusée".to_owned(),
        other => match description {
            Some(d) if !d.is_empty() => format!("{other} : {d}"),
            _ => other.to_owned(),
        },
    }
}

fn token_of(r: Reply) -> Option<Token> {
    let access_token = r.access_token.filter(|t| !t.is_empty())?;
    Some(Token {
        access_token,
        refresh_token: r.refresh_token.filter(|t| !t.is_empty()),
        expires_in: r.expires_in,
    })
}

/// Étape 1 : demande un code à faire autoriser.
pub async fn request_code(web: &str, client_id: &str) -> Result<DeviceCode, String> {
    let r = post(
        web,
        "/oauth/authorize_device",
        &[("client_id", client_id), ("scope", SCOPE)],
    )
    .await?;
    if let Some(e) = r.error {
        return Err(explain(&e, r.error_description.as_deref()));
    }
    match (r.device_code, r.user_code, r.verification_uri) {
        (Some(device_code), Some(user_code), Some(verification_uri)) => Ok(DeviceCode {
            device_code,
            user_code,
            verification_uri,
            expires_in: r.expires_in.unwrap_or(300),
            interval: r.interval.unwrap_or(5).max(1),
        }),
        _ => Err("réponse incomplète de GitLab".to_owned()),
    }
}

/// Étape 2 : attend l'autorisation. Une première pause d'un intervalle, puis une interrogation
/// par intervalle (`slow_down` l'allonge de 5 s, ou de la valeur renvoyée), jusqu'à l'expiration.
pub async fn poll_token(web: &str, client_id: &str, code: &DeviceCode) -> Result<Token, String> {
    poll_with(web, client_id, code, Duration::from_secs(1)).await
}

/// `second` est la durée d'une « seconde » du protocole (réduite dans les tests).
async fn poll_with(
    web: &str,
    client_id: &str,
    code: &DeviceCode,
    second: Duration,
) -> Result<Token, String> {
    let mut interval = code.interval.max(1);
    let mut waited = 0u64;
    loop {
        tokio::time::sleep(second * interval as u32).await;
        waited += interval;
        let r = post(
            web,
            "/oauth/token",
            &[
                ("client_id", client_id),
                ("device_code", &code.device_code),
                ("grant_type", GRANT_TYPE),
            ],
        )
        .await?;
        let (error, description, new_interval) =
            (r.error.clone(), r.error_description.clone(), r.interval);
        if let Some(token) = token_of(r) {
            return Ok(token);
        }
        match error.as_deref() {
            Some("authorization_pending") | None => {}
            Some("slow_down") => {
                interval = new_interval
                    .unwrap_or(interval + SLOW_DOWN_STEP)
                    .max(interval);
            }
            Some(e) => return Err(explain(e, description.as_deref())),
        }
        if waited >= code.expires_in {
            return Err(explain("expired_token", None));
        }
    }
}

/// Obtient un nouveau jeton avec le `refresh_token` : à faire avant que l'ancien n'expire ou dès
/// qu'il est refusé. GitLab peut en renvoyer un nouveau (à garder à la place de l'ancien).
pub async fn refresh(web: &str, client_id: &str, refresh_token: &str) -> Result<Token, String> {
    let r = post(
        web,
        "/oauth/token",
        &[
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )
    .await?;
    if let Some(e) = &r.error {
        return Err(explain(e, r.error_description.as_deref()));
    }
    token_of(r).ok_or_else(|| "réponse sans jeton".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, method, path},
    };

    #[test]
    fn hote_web_depuis_l_adresse() {
        assert_eq!(web_base(""), "https://gitlab.com");
        assert_eq!(web_base("https://git.example/"), "https://git.example");
        assert_eq!(
            web_base("https://git.example/api/v4"),
            "https://git.example"
        );
    }

    fn code() -> DeviceCode {
        DeviceCode {
            device_code: "dc".into(),
            user_code: "UC".into(),
            verification_uri: "https://gitlab.com/oauth/device".into(),
            expires_in: 60,
            interval: 1,
        }
    }

    #[tokio::test]
    async fn demande_de_code() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/authorize_device"))
            .and(body_string_contains("client_id=abc"))
            .and(body_string_contains("scope=read_api"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_code": "dc", "user_code": "IOZZQB9E",
                "verification_uri": "https://gitlab.com/oauth/device",
                "verification_uri_complete": "https://gitlab.com/oauth/device?user_code=IOZZQB9E",
                "expires_in": 300, "interval": 5
            })))
            .mount(&server)
            .await;
        let c = request_code(&server.uri(), "abc").await.unwrap();
        assert_eq!(
            (c.user_code.as_str(), c.expires_in, c.interval),
            ("IOZZQB9E", 300, 5)
        );

        let refuse = MockServer::start().await;
        Mock::given(path("/oauth/authorize_device"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "error": "invalid_client", "error_description": "Client authentication failed"
            })))
            .mount(&refuse)
            .await;
        let err = request_code(&refuse.uri(), "abc").await.unwrap_err();
        assert!(err.contains("Confidential"), "{err}");
    }

    #[tokio::test]
    async fn attente_en_400_puis_jeton_avec_son_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("device_code=dc"))
            .respond_with(ResponseTemplate::new(400).set_body_json(json!({
                "error": "authorization_pending"
            })))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "tok", "token_type": "Bearer", "expires_in": 7200,
                "refresh_token": "ref", "scope": "read_api", "created_at": 1792672650
            })))
            .mount(&server)
            .await;
        let t = poll_with(&server.uri(), "abc", &code(), Duration::from_millis(1))
            .await
            .unwrap();
        assert_eq!(t.access_token, "tok");
        assert_eq!(t.refresh_token.as_deref(), Some("ref"));
        assert_eq!(t.expires_in, Some(7200));
        assert!(
            !format!("{t:?}").contains("tok"),
            "le jeton ne s'affiche jamais"
        );
    }

    #[tokio::test]
    async fn refus_expiration_et_renouvellement() {
        let denied = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "access_denied"})),
            )
            .mount(&denied)
            .await;
        let err = poll_with(&denied.uri(), "abc", &code(), Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(err.contains("refusée"), "{err}");

        let slow = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "authorization_pending"})),
            )
            .mount(&slow)
            .await;
        let mut short = code();
        short.expires_in = 3;
        let err = poll_with(&slow.uri(), "abc", &short, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(err.contains("expiré"), "{err}");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=ref"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "neuf", "refresh_token": "ref2", "expires_in": 7200
            })))
            .mount(&server)
            .await;
        let t = refresh(&server.uri(), "abc", "ref").await.unwrap();
        assert_eq!(
            (t.access_token.as_str(), t.refresh_token.as_deref()),
            ("neuf", Some("ref2"))
        );
        let bad = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})),
            )
            .mount(&bad)
            .await;
        assert!(
            refresh(&bad.uri(), "abc", "x")
                .await
                .unwrap_err()
                .contains("reconnectez")
        );
    }
}
