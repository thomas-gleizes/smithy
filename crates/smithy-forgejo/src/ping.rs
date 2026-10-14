//! Sonde de joignabilité : le serveur répond-il ? Aucune authentification, c'est la
//! connexion réseau qu'on teste (VPN coupé, machine éteinte, mauvaise URL), pas la session.

use std::time::Duration;

/// `Ok(version)` si le serveur répond à `/api/v1/version` (la version peut manquer si la
/// réponse n'est pas celle d'un Forgejo), `Err(raison courte)` sinon.
pub async fn ping(base_url: &str) -> Result<Option<String>, String> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .map_err(|e| e.without_url().to_string())?;
    let url = format!("{}/api/v1/version", base_url.trim_end_matches('/'));
    let resp = client.get(url).send().await.map_err(describe)?;
    let status = resp.status();
    if status.is_server_error() {
        return Err(format!("erreur HTTP {}", status.as_u16()));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("pas un Forgejo (404)".to_owned());
    }
    // 401/403 : le serveur répond (certains exigent une authentification partout).
    let version = resp
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v.get("version")?.as_str().map(str::to_owned));
    Ok(version)
}

fn describe(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "délai dépassé".to_owned()
    } else if e.is_connect() {
        "connexion impossible".to_owned()
    } else {
        let msg = e.without_url().to_string();
        msg.chars().take(40).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    async fn serving(status: u16, body: &str) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(path("/api/v1/version"))
            .respond_with(ResponseTemplate::new(status).set_body_string(body))
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn serveur_joignable_avec_sa_version() {
        let s = serving(200, r#"{"version":"10.0.3+gitea-1.22.0"}"#).await;
        assert_eq!(ping(&s.uri()).await, Ok(Some("10.0.3+gitea-1.22.0".into())));
        // Une barre finale dans l'URL ne gêne pas
        assert!(ping(&format!("{}/", s.uri())).await.is_ok());
    }

    #[tokio::test]
    async fn authentification_exigee_compte_comme_joignable() {
        let s = serving(401, "").await;
        assert_eq!(ping(&s.uri()).await, Ok(None));
    }

    #[tokio::test]
    async fn erreurs_http() {
        let s = serving(502, "").await;
        assert_eq!(ping(&s.uri()).await, Err("erreur HTTP 502".into()));
        let s = serving(404, "").await;
        assert!(ping(&s.uri()).await.unwrap_err().contains("Forgejo"));
    }

    #[tokio::test]
    async fn serveur_inaccessible() {
        // Un port fermé sur la machine : la connexion est refusée tout de suite.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(
            ping(&format!("http://127.0.0.1:{port}")).await,
            Err("connexion impossible".into())
        );
    }
}
