//! Explorateur de code : branches, dossiers, fichiers. Contrairement aux logs, tout passe par
//! l'API REST documentée (`/repos/{o}/{r}/branches`, `/contents`, `/raw`) avec le token, sans
//! cookie de session. Comportement mesuré avec `dev/dev.sh probe-browse`.

use reqwest::StatusCode;
use serde::Deserialize;
use smithy_core::{Branch, Entry, EntryKind, Error, FileContent, Repo, Result};

use crate::{Forgejo, PAGE_SIZE};

const MAX_BRANCH_PAGES: usize = 20;
/// Taille examinée pour décider qu'un fichier est binaire (octet nul), comme Git.
const BINARY_SNIFF: usize = 8192;

#[derive(Deserialize)]
struct ApiRepoMeta {
    #[serde(default)]
    default_branch: String,
}

#[derive(Deserialize)]
struct ApiBranch {
    name: String,
}

#[derive(Deserialize)]
struct ApiEntry {
    name: String,
    #[serde(default)]
    path: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    size: u64,
}

/// Encode chaque segment d'un chemin de dépôt (espaces, `#`, `?`, accents…) et garde les `/`.
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

fn kind(t: &str) -> EntryKind {
    match t {
        "dir" => EntryKind::Dir,
        "symlink" => EntryKind::Symlink,
        "submodule" => EntryKind::Submodule,
        // « file », mais aussi tout type inconnu d'une version future : ouvrable.
        _ => EntryKind::File,
    }
}

impl Forgejo {
    pub(crate) async fn fetch_branches(&self, repo: &Repo) -> Result<Vec<Branch>> {
        let base = format!("/repos/{}/{}", repo.owner, repo.name);
        let meta: ApiRepoMeta = self.get(&base, &[]).await?;
        let mut branches = Vec::new();
        for page in 1..=MAX_BRANCH_PAGES {
            // Un dépôt sans commit répond `null` (et non `[]`) : mesuré sur 10 à 15.
            let batch: Vec<ApiBranch> = self
                .get::<Option<Vec<ApiBranch>>>(
                    &format!("{base}/branches"),
                    &[("limit", PAGE_SIZE.to_string()), ("page", page.to_string())],
                )
                .await?
                .unwrap_or_default();
            let n = batch.len();
            branches.extend(batch.into_iter().map(|b| Branch {
                default: b.name == meta.default_branch,
                name: b.name,
            }));
            if n < PAGE_SIZE {
                break;
            }
        }
        // La branche par défaut d'abord, puis l'ordre alphabétique.
        branches.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| a.name.cmp(&b.name)));
        Ok(branches)
    }

    pub(crate) async fn fetch_dir(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
    ) -> Result<Vec<Entry>> {
        let mut api = format!("/repos/{}/{}/contents", repo.owner, repo.name);
        if !path.is_empty() {
            api.push('/');
            api.push_str(&encode_path(path));
        }
        // Même précaution que pour les branches : `null` est traité comme un dossier vide.
        let entries = self
            .get::<Option<Vec<ApiEntry>>>(&api, &[("ref", reference.to_owned())])
            .await?
            .unwrap_or_default();
        Ok(entries
            .into_iter()
            .map(|e| Entry {
                kind: kind(&e.kind),
                size: if e.kind == "dir" { 0 } else { e.size },
                path: if e.path.is_empty() {
                    format!("{path}/{}", e.name)
                } else {
                    e.path
                },
                name: e.name,
            })
            .collect())
    }

    /// Lit `raw` (et non `contents`, qui renvoie tout le fichier en base64 sans plafond) en
    /// s'arrêtant au plafond d'octets.
    pub(crate) async fn fetch_file(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<FileContent> {
        let api = format!(
            "/repos/{}/{}/raw/{}",
            repo.owner,
            repo.name,
            encode_path(path)
        );
        let net = |e: reqwest::Error| Error::Network(e.without_url().to_string());
        let mut resp = self
            .client
            .get(self.api(&api))
            .header("Authorization", format!("token {}", self.token))
            .query(&[("ref", reference)])
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Unauthorized),
            StatusCode::NOT_FOUND => return Err(Error::NotFound(path.to_owned())),
            s => return Err(Error::Network(format!("HTTP {s} sur {path}"))),
        }
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
        let size = announced.unwrap_or(buf.len() as u64);
        let text = if buf[..buf.len().min(BINARY_SNIFF)].contains(&0) {
            None
        } else {
            Some(decode(&buf, truncated))
        };
        Ok(FileContent {
            size,
            text,
            truncated,
        })
    }
}

/// UTF-8, avec remplacement des octets invalides. Une troncature en plein milieu d'un
/// caractère ne laisse pas de caractère de remplacement parasite en fin de fichier.
fn decode(buf: &[u8], truncated: bool) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_owned(),
        Err(e) if truncated && e.error_len().is_none() => {
            String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(buf).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn repo() -> Repo {
        Repo {
            owner: "smithy".into(),
            name: "tree".into(),
        }
    }

    fn forgejo(server: &MockServer) -> Forgejo {
        Forgejo::new(server.uri(), "secret")
    }

    #[test]
    fn encodage_des_chemins() {
        assert_eq!(encode_path("src/main.rs"), "src/main.rs");
        assert_eq!(encode_path("a b/c#1 ?.txt"), "a%20b/c%231%20%3F.txt");
        assert_eq!(
            encode_path("été/résumé.md"),
            "%C3%A9t%C3%A9/r%C3%A9sum%C3%A9.md"
        );
        assert_eq!(encode_path("100%/a+b&c=d"), "100%25/a%2Bb%26c%3Dd");
        assert_eq!(encode_path(""), "");
    }

    #[test]
    fn types_d_entree() {
        assert_eq!(kind("dir"), EntryKind::Dir);
        assert_eq!(kind("file"), EntryKind::File);
        assert_eq!(kind("symlink"), EntryKind::Symlink);
        assert_eq!(kind("submodule"), EntryKind::Submodule);
        assert_eq!(kind("autre-chose"), EntryKind::File);
    }

    #[test]
    fn decodage_des_troncatures() {
        assert_eq!(decode("héllo".as_bytes(), false), "héllo");
        // « é » coupé en deux : l'octet orphelin disparaît au lieu de devenir « � »
        assert_eq!(decode(&"héllo".as_bytes()[..2], true), "h");
        // Octets invalides au milieu : remplacés
        assert_eq!(decode(b"a\xffb", false), "a\u{fffd}b");
        assert_eq!(decode(b"a\xffb", true), "a\u{fffd}b");
    }

    #[tokio::test]
    async fn branches_paginees_la_par_defaut_en_premier() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree"))
            .and(header("Authorization", "token secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let page = |names: Vec<String>| {
            ResponseTemplate::new(200).set_body_json(
                names
                    .into_iter()
                    .map(|n| json!({"name": n, "commit": {"id": "abc"}}))
                    .collect::<Vec<_>>(),
            )
        };
        let first: Vec<String> = (0..PAGE_SIZE).map(|i| format!("b{i:02}")).collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/branches"))
            .and(query_param("page", "1"))
            .and(query_param("limit", "50"))
            .respond_with(page(first))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/branches"))
            .and(query_param("page", "2"))
            .respond_with(page(vec!["main".into(), "feature/x".into()]))
            .mount(&server)
            .await;

        let b = forgejo(&server).list_branches(&repo()).await.unwrap();
        assert_eq!(b.len(), PAGE_SIZE + 2);
        assert_eq!(
            b[0],
            Branch {
                name: "main".into(),
                default: true
            }
        );
        assert!(b[1..].iter().all(|x| !x.default));
        assert_eq!(b[1].name, "b00", "puis l'ordre alphabétique");
        assert!(b.iter().any(|x| x.name == "feature/x"));
    }

    #[tokio::test]
    async fn depot_vide_repond_null_et_non_une_liste() {
        // Capturé sur Forgejo 10 à 15 : GET /branches d'un dépôt sans commit → 200 `null`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"default_branch": "main", "empty": true})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_string("null"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/contents"))
            .respond_with(ResponseTemplate::new(200).set_body_string("null"))
            .mount(&server)
            .await;
        let f = forgejo(&server);
        assert!(f.list_branches(&repo()).await.unwrap().is_empty());
        assert!(f.read_dir(&repo(), "main", "").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn depot_vide_sans_branche() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"default_branch": "main", "empty": true})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        assert!(
            forgejo(&server)
                .list_branches(&repo())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn listing_d_un_dossier_avec_chemin_encode_et_branche_a_slash() {
        let server = MockServer::start().await;
        // Réponse réduite d'une capture réelle (Forgejo 15) : champs inutiles conservés.
        let body = json!([
            {"name": "intro.md", "path": "docs/guide/intro.md", "type": "file", "size": 8,
             "sha": "abc", "url": "http://x", "_links": {}},
            {"name": "sub", "path": "docs/guide/sub", "type": "dir", "size": 0},
            {"name": "lien", "path": "docs/guide/lien", "type": "symlink", "size": 5, "target": "intro.md"},
            {"name": "mod", "path": "docs/guide/mod", "type": "submodule", "size": 0}
        ]);
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/contents/docs/guide%20x"))
            .and(query_param("ref", "feature/x"))
            .and(header("Authorization", "token secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let e = forgejo(&server)
            .read_dir(&repo(), "feature/x", "docs/guide x")
            .await
            .unwrap();
        let kinds: Vec<_> = e.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::File,
                EntryKind::Dir,
                EntryKind::Symlink,
                EntryKind::Submodule
            ]
        );
        assert_eq!(e[0].path, "docs/guide/intro.md");
        assert_eq!(e[0].size, 8);
        assert_eq!(e[1].size, 0);
    }

    #[tokio::test]
    async fn racine_sans_chemin() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/contents"))
            .and(query_param("ref", "main"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([{"name": "README.md", "type": "file", "size": 3}])),
            )
            .mount(&server)
            .await;
        let e = forgejo(&server)
            .read_dir(&repo(), "main", "")
            .await
            .unwrap();
        assert_eq!(
            e[0].path, "/README.md",
            "chemin reconstruit si l'API n'en donne pas"
        );
    }

    #[tokio::test]
    async fn fichier_texte_et_chemin_special() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/api/v1/repos/smithy/tree/raw/%C3%A9t%C3%A9/a%20%231%20%3F.txt",
            ))
            .and(query_param("ref", "main"))
            .respond_with(ResponseTemplate::new(200).set_body_string("bonjour\n"))
            .mount(&server)
            .await;
        let f = forgejo(&server)
            .read_file(&repo(), "main", "été/a #1 ?.txt", 1024)
            .await
            .unwrap();
        assert_eq!(f.text.as_deref(), Some("bonjour\n"));
        assert_eq!((f.size, f.truncated), (8, false));
    }

    #[tokio::test]
    async fn fichier_trop_gros_tronque_sans_tout_lire() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/raw/big.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 5000]))
            .mount(&server)
            .await;
        let f = forgejo(&server)
            .read_file(&repo(), "main", "big.txt", 1000)
            .await
            .unwrap();
        assert!(f.truncated);
        assert_eq!(f.text.as_deref().map(str::len), Some(1000));
        assert_eq!(
            f.size, 5000,
            "la taille annoncée par le serveur est conservée"
        );
        // Exactement au plafond : pas de troncature
        let g = forgejo(&server)
            .read_file(&repo(), "main", "big.txt", 5000)
            .await
            .unwrap();
        assert!(!g.truncated);
        assert_eq!(g.text.as_deref().map(str::len), Some(5000));
    }

    #[tokio::test]
    async fn fichier_binaire_et_fichier_vide() {
        let server = MockServer::start().await;
        let mut bin = vec![b'a'; 100];
        bin[50] = 0;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/raw/data.bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bin))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/raw/empty.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(Vec::new()))
            .mount(&server)
            .await;
        let f = forgejo(&server);
        let b = f
            .read_file(&repo(), "main", "data.bin", 1024)
            .await
            .unwrap();
        assert_eq!((b.text, b.size, b.truncated), (None, 100, false));
        let e = f
            .read_file(&repo(), "main", "empty.txt", 1024)
            .await
            .unwrap();
        assert_eq!((e.text.as_deref(), e.size), (Some(""), 0));
    }

    #[tokio::test]
    async fn erreurs_http_traduites() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/raw/absent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/contents"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let f = forgejo(&server);
        assert!(matches!(
            f.read_file(&repo(), "main", "absent", 10).await,
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            f.read_dir(&repo(), "main", "").await,
            Err(Error::Unauthorized)
        ));
    }
}
