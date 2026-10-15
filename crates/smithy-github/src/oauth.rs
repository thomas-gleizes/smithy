//! Connexion par code (« device flow » OAuth) : l'application affiche un code, l'utilisateur
//! l'autorise sur github.com depuis n'importe quel navigateur, et smithy reçoit un token. Ni
//! secret ni redirection : pratique sur une machine sans bureau.
//!
//! Les routes sont sur l'hôte **web** (`github.com`), pas sur celui de l'API : on le déduit de
//! l'URL de l'API (`api.github.com` → `github.com`, `https://hôte/api/v3` → `https://hôte`).
//! Aucune n'a besoin d'un token ; seul l'identifiant public (`client_id`) d'une OAuth App avec
//! le flux « device » activé est requis.

use std::time::Duration;

use reqwest::header;
use serde::Deserialize;

use crate::DEFAULT_API;

/// Droits demandés. `repo` est le plus petit droit qui donne les dépôts privés avec une OAuth
/// App, mais il inclut l'écriture : smithy, lui, ne fait que lire. Un seul endroit à changer
/// (`public_repo` pour ne voir que le public).
pub const SCOPE: &str = "repo";

/// Identifiant public de l'OAuth App de smithy (vide tant qu'elle n'est pas créée ; il n'est pas
/// secret). `SMITHY_GITHUB_CLIENT_ID` le remplace, notamment pour GitHub Enterprise.
pub const DEFAULT_CLIENT_ID: &str = "";

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

/// Hôte web correspondant à l'URL de l'API.
pub fn web_base(api_url: &str) -> String {
    let api = api_url.trim().trim_end_matches('/');
    if api.is_empty() || api == DEFAULT_API {
        return "https://github.com".to_owned();
    }
    api.strip_suffix("/api/v3").unwrap_or(api).to_owned()
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

/// Les erreurs d'OAuth arrivent en HTTP 200 avec un champ `error`.
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
        // Sans cet en-tête la réponse est un formulaire, pas du JSON.
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
        "device_flow_disabled" => {
            "le flux « device » n'est pas activé sur l'OAuth App (Settings → Developer settings)"
                .to_owned()
        }
        "incorrect_client_credentials" => "identifiant client inconnu de GitHub".to_owned(),
        "expired_token" => "le code a expiré : recommencez".to_owned(),
        "access_denied" => "autorisation refusée".to_owned(),
        other => match description {
            Some(d) if !d.is_empty() => format!("{other} : {d}"),
            _ => other.to_owned(),
        },
    }
}

/// Étape 1 : demande un code à faire autoriser.
pub async fn request_code(web: &str, client_id: &str) -> Result<DeviceCode, String> {
    let r = post(
        web,
        "/login/device/code",
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
            expires_in: r.expires_in.unwrap_or(900),
            interval: r.interval.unwrap_or(5).max(1),
        }),
        _ => Err("réponse incomplète de GitHub".to_owned()),
    }
}

/// Étape 2 : attend l'autorisation. Une première pause d'un intervalle, puis une interrogation
/// par intervalle (`slow_down` l'allonge de 5 s, ou de la valeur renvoyée), jusqu'à l'expiration.
pub async fn poll_token(web: &str, client_id: &str, code: &DeviceCode) -> Result<String, String> {
    poll_with(web, client_id, code, Duration::from_secs(1)).await
}

/// `second` est la durée d'une « seconde » du protocole (réduite dans les tests).
async fn poll_with(
    web: &str,
    client_id: &str,
    code: &DeviceCode,
    second: Duration,
) -> Result<String, String> {
    let mut interval = code.interval.max(1);
    let mut waited = 0u64;
    loop {
        tokio::time::sleep(second * interval as u32).await;
        waited += interval;
        let r = post(
            web,
            "/login/oauth/access_token",
            &[
                ("client_id", client_id),
                ("device_code", &code.device_code),
                ("grant_type", GRANT_TYPE),
            ],
        )
        .await?;
        if let Some(token) = r.access_token.filter(|t| !t.is_empty()) {
            return Ok(token);
        }
        match r.error.as_deref() {
            Some("authorization_pending") | None => {}
            Some("slow_down") => {
                interval = r
                    .interval
                    .unwrap_or(interval + SLOW_DOWN_STEP)
                    .max(interval);
            }
            Some(e) => return Err(explain(e, r.error_description.as_deref())),
        }
        if waited >= code.expires_in {
            return Err(explain("expired_token", None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_string_contains, header as h, method, path},
    };

    #[test]
    fn hote_web_depuis_l_api() {
        assert_eq!(web_base(""), "https://github.com");
        assert_eq!(web_base("https://api.github.com"), "https://github.com");
        assert_eq!(web_base("https://api.github.com/"), "https://github.com");
        assert_eq!(
            web_base("https://ghe.exemple.fr/api/v3"),
            "https://ghe.exemple.fr"
        );
        assert_eq!(
            web_base("https://ghe.exemple.fr/api/v3/"),
            "https://ghe.exemple.fr"
        );
    }

    const TICK: Duration = Duration::from_millis(5);

    fn code(interval: u64, expires_in: u64) -> DeviceCode {
        DeviceCode {
            device_code: "dc".into(),
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_in,
            interval,
        }
    }

    fn reply(json: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json)
    }

    #[tokio::test]
    async fn demande_un_code() {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .and(h("accept", "application/json"))
            .and(body_string_contains("client_id=cid"))
            .and(body_string_contains("scope=repo"))
            .respond_with(reply(serde_json::json!({
                "device_code": "dc", "user_code": "ABCD-1234",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900, "interval": 5
            })))
            .mount(&s)
            .await;
        let c = request_code(&s.uri(), "cid").await.unwrap();
        assert_eq!(c, code(5, 900));
    }

    #[tokio::test]
    async fn flux_desactive_est_explique() {
        let s = MockServer::start().await;
        Mock::given(path("/login/device/code"))
            .respond_with(reply(serde_json::json!({"error": "device_flow_disabled"})))
            .mount(&s)
            .await;
        let e = request_code(&s.uri(), "cid").await.unwrap_err();
        assert!(e.contains("device"), "{e}");
    }

    #[tokio::test]
    async fn attend_ralentit_puis_recoit_le_token() {
        let s = MockServer::start().await;
        let seq = |json| {
            Mock::given(path("/login/oauth/access_token"))
                .and(body_string_contains("device_code=dc"))
                .and(body_string_contains(
                    "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                ))
                .respond_with(reply(json))
        };
        seq(serde_json::json!({"error": "authorization_pending"}))
            .up_to_n_times(1)
            .mount(&s)
            .await;
        seq(serde_json::json!({"error": "slow_down", "interval": 10}))
            .up_to_n_times(1)
            .mount(&s)
            .await;
        seq(serde_json::json!({"access_token": "gho_x", "token_type": "bearer"}))
            .mount(&s)
            .await;
        let start = std::time::Instant::now();
        let t = poll_with(&s.uri(), "cid", &code(1, 900), TICK)
            .await
            .unwrap();
        assert_eq!(t, "gho_x");
        // 1 s (pending) + 1 s (slow_down reçu) + 10 s (nouvel intervalle).
        assert!(start.elapsed() >= TICK * 12, "{:?}", start.elapsed());
    }

    #[tokio::test]
    async fn refus_expiration_et_delai() {
        for (error, attendu) in [
            ("access_denied", "refusée"),
            ("expired_token", "expiré"),
            ("device_flow_disabled", "device"),
        ] {
            let s = MockServer::start().await;
            Mock::given(path("/login/oauth/access_token"))
                .respond_with(reply(serde_json::json!({ "error": error })))
                .mount(&s)
                .await;
            let e = poll_with(&s.uri(), "cid", &code(1, 900), TICK)
                .await
                .unwrap_err();
            assert!(e.contains(attendu), "{error}: {e}");
        }
        // Toujours en attente jusqu'à l'expiration locale.
        let s = MockServer::start().await;
        Mock::given(path("/login/oauth/access_token"))
            .respond_with(reply(serde_json::json!({"error": "authorization_pending"})))
            .mount(&s)
            .await;
        let e = poll_with(&s.uri(), "cid", &code(5, 12), TICK)
            .await
            .unwrap_err();
        assert!(e.contains("expiré"), "{e}");
    }
}
