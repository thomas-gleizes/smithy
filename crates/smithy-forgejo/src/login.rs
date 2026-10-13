//! Connexion par identifiant/mot de passe. Les logs de CI ne sont servis que par les
//! routes web de Forgejo, qui n'acceptent pas le token d'API mais un cookie de
//! session (validé sur Forgejo 15). Un seul mot de passe donne les deux accès :
//! le cookie (formulaire de login) et un token en lecture seule (API officielle).
//! Le mot de passe n'est jamais conservé.

use std::collections::BTreeMap;

use reqwest::{StatusCode, header};
use serde::Deserialize;
use smithy_core::{Error, Result};

/// Droits du token créé : lecture seule, suffisants pour repos et pipelines.
const TOKEN_SCOPES: [&str; 2] = ["read:repository", "read:user"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    /// Identifiant canonique (l'utilisateur a pu saisir son e-mail).
    pub username: String,
    /// `None` si l'appelant a demandé de ne pas en créer.
    pub token: Option<String>,
    pub cookies: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Deserialize)]
struct ApiToken {
    sha1: String,
}

/// `token_name` : nom du token en lecture seule à créer ; `None` pour ne renouveler que le
/// cookie (reconnexion, quand un token existe déjà : sinon ils s'empilent sur le compte).
pub async fn login(
    base_url: &str,
    username: &str,
    password: &str,
    token_name: Option<&str>,
) -> Result<Login> {
    let base = base_url.trim_end_matches('/');
    // Pas de redirection suivie : un 303 vers `/` est précisément le signe du succès.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("smithy/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("configuration du client HTTP statique et valide");
    let net = |e: reqwest::Error| Error::Network(e.without_url().to_string());

    // 1. Cookie de session, via le formulaire web. Forgejo 10 à 12 y mettent un jeton
    //    CSRF (champ `_csrf` + cookie du même nom) ; la 15 n'en a pas. On fait comme
    //    un navigateur : lire le formulaire, renvoyer ce qu'il contient.
    let form_page = client
        .get(format!("{base}/user/login"))
        .send()
        .await
        .map_err(net)?;
    let csrf_cookie = parse_cookies(form_page.headers()).remove("_csrf");
    let form_html = form_page.text().await.map_err(net)?;
    let csrf_field = form_html
        .split("name=\"_csrf\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .filter(|t| !t.is_empty());

    let mut fields = vec![
        ("user_name", username),
        ("password", password),
        ("remember", "on"),
    ];
    if let Some(t) = csrf_field {
        fields.push(("_csrf", t));
    }
    let mut req = client.post(format!("{base}/user/login")).form(&fields);
    if let Some(c) = &csrf_cookie {
        req = req.header(header::COOKIE, format!("_csrf={c}"));
    }
    let resp = req.send().await.map_err(net)?;
    let location = resp
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    // Un échec réaffiche le formulaire (200 en 15) ou y renvoie (303 en 10 à 12) :
    // seule une redirection ailleurs que vers le login est un succès.
    if !resp.status().is_redirection() || location.contains("/user/login") {
        return match resp.status() {
            s if s.is_redirection() || s == StatusCode::OK => Err(Error::BadCredentials),
            s => Err(Error::Network(format!("HTTP {s} sur /user/login"))),
        };
    }
    if location.contains("two_factor") || location.contains("twofa") {
        return Err(Error::Unsupported("authentification à deux facteurs"));
    }
    // Les noms varient selon la version (`session`, `i_like_gitea`, `gitea_incredible`…) :
    // on garde tout, sauf la langue et le jeton CSRF, qui ne sont pas des identifiants.
    let mut cookies = parse_cookies(resp.headers());
    cookies.remove("_csrf");
    cookies.remove("lang");
    if cookies.is_empty() {
        return Err(Error::Decode(
            "login accepté mais aucun cookie de session reçu".into(),
        ));
    }

    // 2. Identifiant canonique (l'API veut le login, pas l'e-mail).
    let user: ApiUser = api_json(
        client
            .get(format!("{base}/api/v1/user"))
            .basic_auth(username, Some(password)),
        "/user",
    )
    .await?;

    // 3. Token en lecture seule.
    let token = match token_name {
        Some(name) => {
            let t: ApiToken = api_json(
                client
                    .post(format!("{base}/api/v1/users/{}/tokens", user.login))
                    .basic_auth(username, Some(password))
                    .json(&serde_json::json!({"name": name, "scopes": TOKEN_SCOPES})),
                "création du token",
            )
            .await?;
            Some(t.sha1)
        }
        None => None,
    };

    Ok(Login {
        username: user.login,
        token,
        cookies,
    })
}

async fn api_json<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
    what: &str,
) -> Result<T> {
    let resp = req
        .send()
        .await
        .map_err(|e| Error::Network(e.without_url().to_string()))?;
    match resp.status() {
        s if s.is_success() => resp.json().await.map_err(|e| Error::Decode(e.to_string())),
        StatusCode::UNAUTHORIZED => Err(Error::BadCredentials),
        StatusCode::FORBIDDEN => Err(Error::Network(format!("{what} : refusé par le serveur"))),
        s => Err(Error::Network(format!("HTTP {s} ({what})"))),
    }
}

/// Cookies posés par une réponse. Le dernier `Set-Cookie` d'un nom l'emporte
/// (Forgejo en émet deux pour `session`) ; une suppression (`Max-Age=0`) l'efface.
fn parse_cookies(headers: &header::HeaderMap) -> BTreeMap<String, String> {
    let mut jar = BTreeMap::new();
    for raw in headers.get_all(header::SET_COOKIE) {
        let Ok(raw) = raw.to_str() else { continue };
        let mut parts = raw.split(';');
        let Some((name, value)) = parts.next().and_then(|kv| kv.trim().split_once('=')) else {
            continue;
        };
        let deleted = value.is_empty() || parts.any(|a| a.trim().eq_ignore_ascii_case("max-age=0"));
        if deleted {
            jar.remove(name);
        } else {
            jar.insert(name.to_owned(), value.to_owned());
        }
    }
    jar
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{basic_auth, body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn redirect(to: &str) -> ResponseTemplate {
        ResponseTemplate::new(303)
            .insert_header("Location", to)
            .append_header("Set-Cookie", "session=premier; Path=/; HttpOnly")
            .append_header("Set-Cookie", "session=second; Path=/; HttpOnly")
            .append_header("Set-Cookie", "persistent=long; Path=/; Max-Age=2678400")
            .append_header("Set-Cookie", "lang=fr-FR; Path=/")
    }

    async fn api_ok(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .and(basic_auth("a@b.c", "pw"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"login": "thomas"})),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn login_complet() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .and(body_string_contains("user_name=a%40b.c"))
            .and(body_string_contains("remember=on"))
            .respond_with(redirect("/"))
            .mount(&server)
            .await;
        api_ok(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/v1/users/thomas/tokens"))
            .and(basic_auth("a@b.c", "pw"))
            .and(body_string_contains("smithy-test"))
            .and(body_string_contains("read:repository"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"sha1": "tok123", "id": 1})),
            )
            .mount(&server)
            .await;

        let l = login(
            &format!("{}/", server.uri()),
            "a@b.c",
            "pw",
            Some("smithy-test"),
        )
        .await
        .unwrap();
        assert_eq!(l.username, "thomas");
        assert_eq!(l.token.as_deref(), Some("tok123"));
        assert_eq!(l.cookies["session"], "second");
        assert_eq!(l.cookies["persistent"], "long");
    }

    #[tokio::test]
    async fn reconnexion_sans_nouveau_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .respond_with(redirect("/"))
            .mount(&server)
            .await;
        api_ok(&server).await;
        let l = login(&server.uri(), "a@b.c", "pw", None).await.unwrap();
        assert_eq!(l.token, None);
        assert_eq!(l.cookies["session"], "second");
        let seen = server.received_requests().await.unwrap();
        assert!(
            seen.iter().all(|r| !r.url.path().ends_with("/tokens")),
            "aucun token ne doit être créé"
        );
    }

    /// Forgejo 10 à 12 : jeton CSRF dans le formulaire et en cookie, échec en redirection.
    async fn mount_legacy_login(server: &MockServer, ok: bool) {
        Mock::given(method("GET"))
            .and(path("/user/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .append_header("Set-Cookie", "_csrf=ck123; Path=/; Max-Age=86400")
                    .set_body_string(r#"<form><input type="hidden" name="_csrf" value="tok456">"#),
            )
            .mount(server)
            .await;
        let answer = if ok {
            ResponseTemplate::new(303)
                .insert_header("Location", "/")
                .append_header("Set-Cookie", "i_like_gitea=sess; Path=/; HttpOnly")
                .append_header(
                    "Set-Cookie",
                    "gitea_incredible=rem; Path=/; Max-Age=2678400",
                )
                .append_header("Set-Cookie", "_csrf=; Path=/; Max-Age=0")
                .append_header("Set-Cookie", "lang=fr-FR; Path=/")
        } else {
            ResponseTemplate::new(303).insert_header("Location", "/user/login")
        };
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .and(wiremock::matchers::header("Cookie", "_csrf=ck123"))
            .and(body_string_contains("_csrf=tok456"))
            .respond_with(answer)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn login_forgejo_10_a_12() {
        let server = MockServer::start().await;
        mount_legacy_login(&server, true).await;
        api_ok(&server).await;
        let l = login(&server.uri(), "a@b.c", "pw", None).await.unwrap();
        // Les noms de cookies de cette famille sont gardés ; langue et CSRF écartés.
        assert_eq!(
            l.cookies.keys().map(String::as_str).collect::<Vec<_>>(),
            ["gitea_incredible", "i_like_gitea"]
        );
    }

    #[tokio::test]
    async fn echec_en_redirection_vers_le_login() {
        let server = MockServer::start().await;
        mount_legacy_login(&server, false).await;
        let r = login(&server.uri(), "u", "faux", Some("t")).await;
        assert!(matches!(r, Err(Error::BadCredentials)), "{r:?}");
        let seen = server.received_requests().await.unwrap();
        assert!(seen.iter().all(|r| !r.url.path().starts_with("/api/")));
    }

    #[tokio::test]
    async fn mauvais_mot_de_passe_ne_cree_rien() {
        let server = MockServer::start().await;
        // Forgejo réaffiche le formulaire en 200 quand l'authentification échoue.
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<form>"))
            .mount(&server)
            .await;
        let r = login(&server.uri(), "u", "faux", Some("t")).await;
        assert!(matches!(r, Err(Error::BadCredentials)), "{r:?}");
        // Aucune requête API (et donc aucun token orphelin) n'a été envoyée.
        let seen = server.received_requests().await.unwrap();
        assert!(
            seen.iter().all(|r| !r.url.path().starts_with("/api/")),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn deux_facteurs_signales() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .respond_with(ResponseTemplate::new(303).insert_header("Location", "/user/two_factor"))
            .mount(&server)
            .await;
        let r = login(&server.uri(), "u", "pw", Some("t")).await;
        assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
    }

    #[tokio::test]
    async fn creation_du_token_refusee() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/user/login"))
            .respond_with(redirect("/"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/user"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"login": "thomas"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/users/thomas/tokens"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let r = login(&server.uri(), "thomas", "pw", Some("t")).await;
        assert!(
            matches!(r, Err(Error::Network(ref m)) if m.contains("refusé")),
            "{r:?}"
        );
    }

    #[test]
    fn cookies_supprimes_effaces() {
        let mut h = header::HeaderMap::new();
        for v in [
            "a=1; Path=/",
            "b=2; Path=/",
            "b=; Path=/; Max-Age=0",
            "c=3; Path=/; Max-Age=0",
            "\u{e9}invalide",
        ] {
            if let Ok(v) = header::HeaderValue::from_str(v) {
                h.append(header::SET_COOKIE, v);
            }
        }
        let jar = parse_cookies(&h);
        assert_eq!(jar.len(), 1);
        assert_eq!(jar["a"], "1");
    }
}
