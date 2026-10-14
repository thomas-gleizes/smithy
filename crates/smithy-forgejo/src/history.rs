//! Historique : commits d'une branche, détail d'un commit et son correctif. Comme l'explorateur,
//! tout passe par l'API REST documentée avec le token (`/commits`, `/git/commits/{sha}` et sa
//! variante `.diff`). Comportement mesuré avec `dev/dev.sh probe-history`.

use reqwest::StatusCode;
use serde::Deserialize;
use smithy_core::{ChangeKind, Commit, CommitDetail, CommitPage, Error, FileChange, Repo, Result};

use crate::{Forgejo, PAGE_SIZE, browse::decode};

/// Lecture du correctif : plus généreuse que le délai des requêtes ordinaires, comme un fichier.
const PATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Deserialize)]
pub(crate) struct ApiCommit {
    #[serde(default)]
    sha: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    commit: ApiCommitBody,
    #[serde(default)]
    parents: Vec<ApiParent>,
    #[serde(default)]
    files: Option<Vec<ApiFile>>,
    #[serde(default)]
    stats: Option<ApiStats>,
}

#[derive(Deserialize, Default)]
struct ApiCommitBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    author: ApiSignature,
}

#[derive(Deserialize, Default)]
struct ApiSignature {
    #[serde(default)]
    name: String,
    #[serde(default)]
    date: String,
}

#[derive(Deserialize)]
struct ApiParent {
    #[serde(default)]
    sha: String,
}

#[derive(Deserialize)]
struct ApiFile {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
struct ApiStats {
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
}

impl ApiCommit {
    pub(crate) fn into_commit(self) -> (Commit, ApiRest) {
        let date = Some(self.commit.author.date)
            .filter(|d| !d.is_empty())
            .or_else(|| Some(self.created).filter(|d| !d.is_empty()));
        (
            Commit {
                sha: self.sha,
                message: self.commit.message,
                author: self.commit.author.name,
                date,
                url: Some(self.html_url).filter(|u| !u.is_empty()),
            },
            ApiRest {
                parents: self.parents,
                files: self.files,
                stats: self.stats,
            },
        )
    }
}

pub(crate) struct ApiRest {
    parents: Vec<ApiParent>,
    files: Option<Vec<ApiFile>>,
    stats: Option<ApiStats>,
}

fn change_kind(status: &str) -> ChangeKind {
    match status {
        "added" => ChangeKind::Added,
        "modified" | "changed" => ChangeKind::Modified,
        "removed" | "deleted" => ChangeKind::Removed,
        "renamed" => ChangeKind::Renamed,
        _ => ChangeKind::Other,
    }
}

/// Une empreinte ne contient que des chiffres hexadécimaux : tout le reste est refusé avant
/// d'être mis dans une URL.
fn valid_sha(sha: &str) -> bool {
    (4..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Retire ce qui précède le premier `diff --git` : le correctif d'un commit racine commence par
/// un en-tête de courriel (`commit …`, auteur, date, message), les autres directement par le diff.
fn strip_preamble(patch: &str) -> &str {
    if patch.starts_with("diff --git") {
        return patch;
    }
    patch.find("\ndiff --git").map_or("", |i| &patch[i + 1..])
}

impl Forgejo {
    pub(crate) async fn fetch_commits(
        &self,
        repo: &Repo,
        reference: &str,
        page: u32,
    ) -> Result<CommitPage> {
        let api = format!("/repos/{}/{}/commits", repo.owner, repo.name);
        // `stat`, `files` et `verification` n'intéressent pas une liste : on les coupe pour
        // épargner le serveur (le plafond de `limit` est 50, comme les autres listes).
        let mut query = vec![
            ("limit", PAGE_SIZE.to_string()),
            ("page", page.max(1).to_string()),
            ("stat", "false".to_owned()),
            ("verification", "false".to_owned()),
            ("files", "false".to_owned()),
        ];
        if !reference.is_empty() {
            query.push(("sha", reference.to_owned()));
        }
        let net = |e: reqwest::Error| Error::Network(e.without_url().to_string());
        let resp = self
            .client
            .get(self.api(&api))
            .header("Authorization", format!("token {}", self.token))
            .query(&query)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Unauthorized),
            StatusCode::NOT_FOUND => return Err(Error::NotFound(reference.to_owned())),
            // Dépôt sans commit : « Git Repository is empty » (mesuré).
            StatusCode::CONFLICT => {
                return Ok(CommitPage {
                    commits: Vec::new(),
                    has_more: false,
                });
            }
            s => return Err(Error::Network(format!("HTTP {s} sur {api}"))),
        }
        let list: Option<Vec<ApiCommit>> = resp
            .json()
            .await
            .map_err(|e| Error::Decode(e.to_string()))?;
        let list = list.unwrap_or_default();
        // Une page pleine peut annoncer une suite : les en-têtes de pagination ne sont pas
        // fiables d'une version à l'autre, on s'en passe.
        let has_more = list.len() >= PAGE_SIZE;
        Ok(CommitPage {
            commits: list.into_iter().map(|c| c.into_commit().0).collect(),
            has_more,
        })
    }

    pub(crate) async fn fetch_commit_detail(
        &self,
        repo: &Repo,
        sha: &str,
        max_bytes: usize,
    ) -> Result<CommitDetail> {
        if !valid_sha(sha) {
            return Err(Error::NotFound(sha.to_owned()));
        }
        let base = format!("/repos/{}/{}/git/commits/{sha}", repo.owner, repo.name);
        let api: ApiCommit = self.get(&base, &[]).await?;
        let (commit, rest) = api.into_commit();
        let (patch, patch_truncated) = self.fetch_patch(&format!("{base}.diff"), max_bytes).await?;
        let (additions, deletions) = rest.stats.map_or((0, 0), |s| (s.additions, s.deletions));
        Ok(CommitDetail {
            commit,
            parents: rest.parents.into_iter().map(|p| p.sha).collect(),
            files: rest
                .files
                .unwrap_or_default()
                .into_iter()
                .map(|f| FileChange {
                    path: f.filename,
                    kind: change_kind(&f.status),
                })
                .collect(),
            additions,
            deletions,
            patch,
            patch_truncated,
        })
    }

    /// Le correctif en texte, lu sur au plus `max_bytes` octets.
    pub(crate) async fn fetch_patch(&self, api: &str, max_bytes: usize) -> Result<(String, bool)> {
        let net = |e: reqwest::Error| Error::Network(e.without_url().to_string());
        let mut resp = self
            .client
            .get(self.api(api))
            .header("Authorization", format!("token {}", self.token))
            .timeout(PATCH_TIMEOUT)
            .send()
            .await
            .map_err(net)?;
        match resp.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(Error::Unauthorized),
            StatusCode::NOT_FOUND => return Err(Error::NotFound(api.to_owned())),
            s => return Err(Error::Network(format!("HTTP {s} sur {api}"))),
        }
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
        let text = decode(&buf, truncated);
        Ok((strip_preamble(&text).to_owned(), truncated))
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

    fn commit_json(n: usize) -> serde_json::Value {
        // Réponse réduite d'une capture réelle (Forgejo 15) : champs inutiles conservés.
        json!({
            "url": "http://x/api", "sha": format!("{n:040x}"), "created": "2026-10-14T13:25:30Z",
            "html_url": format!("http://x/smithy/tree/commit/{n}"),
            "commit": {
                "url": "http://x", "message": format!("chore: n°{n}\n\ncorps\n"),
                "author": {"name": "smithy", "email": "s@x", "date": "2026-10-14T13:25:30Z"},
                "committer": {"name": "smithy", "email": "s@x", "date": "2026-10-14T13:25:31Z"},
                "tree": {"sha": "t"}, "verification": null
            },
            "author": {"id": 1, "login": "smithy"}, "committer": {"id": 1},
            "parents": [], "files": null, "stats": null
        })
    }

    #[tokio::test]
    async fn liste_les_commits_avec_pagination_sans_en_tetes() {
        let server = MockServer::start().await;
        let full: Vec<_> = (0..PAGE_SIZE).map(commit_json).collect();
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/commits"))
            .and(query_param("sha", "feature/x"))
            .and(query_param("page", "1"))
            .and(query_param("limit", "50"))
            .and(query_param("stat", "false"))
            .and(query_param("files", "false"))
            .and(header("Authorization", "token secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/commits"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec![commit_json(900)]))
            .mount(&server)
            .await;
        let f = forgejo(&server);
        let p1 = f.list_commits(&repo(), "feature/x", 1).await.unwrap();
        assert_eq!(p1.commits.len(), PAGE_SIZE);
        assert!(p1.has_more, "une page pleine peut annoncer une suite");
        let c = &p1.commits[3];
        assert_eq!(c.message, "chore: n°3\n\ncorps\n");
        assert_eq!(c.summary(), "chore: n°3");
        assert_eq!(c.short_sha(), "0000000");
        assert_eq!(c.author, "smithy");
        assert_eq!(c.date.as_deref(), Some("2026-10-14T13:25:30Z"));
        assert_eq!(c.url.as_deref(), Some("http://x/smithy/tree/commit/3"));
        // Branche par défaut : pas de paramètre `sha`
        let p2 = f.list_commits(&repo(), "", 2).await.unwrap();
        assert_eq!(p2.commits.len(), 1);
        assert!(!p2.has_more);
    }

    #[tokio::test]
    async fn depot_vide_branche_inconnue_et_acces_refuse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/commits"))
            .and(query_param("sha", "inconnue"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/commits"))
            .and(query_param("sha", "interdit"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repos/smithy/tree/commits"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(json!({"message": "Git Repository is empty."})),
            )
            .mount(&server)
            .await;
        let f = forgejo(&server);
        assert!(matches!(
            f.list_commits(&repo(), "inconnue", 1).await,
            Err(Error::NotFound(b)) if b == "inconnue"
        ));
        assert!(matches!(
            f.list_commits(&repo(), "interdit", 1).await,
            Err(Error::Unauthorized)
        ));
        // 409 : dépôt sans commit, ce n'est pas une erreur
        let empty = f.list_commits(&repo(), "", 1).await.unwrap();
        assert!(empty.commits.is_empty() && !empty.has_more);
    }

    const SHA: &str = "6c3e842f0d56c947ba064d98bedeb9fe64f8fc67";

    #[tokio::test]
    async fn detail_d_un_commit_fichiers_stats_et_correctif() {
        let server = MockServer::start().await;
        let mut detail = commit_json(7);
        detail["sha"] = json!(SHA);
        detail["parents"] = json!([{"sha": "aaaa1111", "url": "u"}]);
        detail["files"] = json!([
            {"filename": "a.txt", "status": "added"},
            {"filename": "b.txt", "status": "removed"},
            {"filename": "c.txt", "status": "modified"},
            {"filename": "d.txt", "status": "renamed"},
            {"filename": "e.txt", "status": "bizarre"}
        ]);
        detail["stats"] = json!({"total": 59, "additions": 2, "deletions": 57});
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/repos/smithy/tree/git/commits/{SHA}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(detail))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/repos/smithy/tree/git/commits/{SHA}.diff"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "commit 6c3e8\nAuthor: smithy\n\n    msg\n\ndiff --git a/a.txt b/a.txt\n+x\n",
            ))
            .mount(&server)
            .await;
        let d = forgejo(&server)
            .commit_detail(&repo(), SHA, 1 << 20)
            .await
            .unwrap();
        assert_eq!(d.commit.sha, SHA);
        assert_eq!(d.parents, ["aaaa1111"]);
        assert_eq!((d.additions, d.deletions), (2, 57));
        let kinds: Vec<_> = d.files.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                ChangeKind::Added,
                ChangeKind::Removed,
                ChangeKind::Modified,
                ChangeKind::Renamed,
                ChangeKind::Other
            ]
        );
        // L'en-tête de courriel d'un commit racine est écarté : le correctif commence au diff
        assert!(d.patch.starts_with("diff --git a/a.txt"), "{}", d.patch);
        assert!(!d.patch_truncated);
    }

    #[tokio::test]
    async fn correctif_plafonne_et_empreinte_invalide() {
        let server = MockServer::start().await;
        let mut detail = commit_json(7);
        detail["sha"] = json!(SHA);
        Mock::given(method("GET"))
            .and(path(format!("/api/v1/repos/smithy/tree/git/commits/{SHA}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(detail))
            .mount(&server)
            .await;
        let big = format!("diff --git a/x b/x\n{}", "+ligne\n".repeat(5000));
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/v1/repos/smithy/tree/git/commits/{SHA}.diff"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_string(big))
            .mount(&server)
            .await;
        let f = forgejo(&server);
        let d = f.commit_detail(&repo(), SHA, 1000).await.unwrap();
        assert!(d.patch_truncated && d.patch.len() <= 1000);
        // Une empreinte qui n'est pas hexadécimale n'atteint jamais le réseau
        for bad in ["../etc/passwd", "abc", "zzzzzzzz", "", "6c3e842/../x"] {
            assert!(
                matches!(
                    f.commit_detail(&repo(), bad, 100).await,
                    Err(Error::NotFound(_))
                ),
                "{bad}"
            );
        }
        // Commit inconnu
        assert!(matches!(
            f.commit_detail(&repo(), "deadbeef", 100).await,
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn en_tete_de_commit_racine_ecarte() {
        assert_eq!(
            strip_preamble("diff --git a/x b/x\n+1\n"),
            "diff --git a/x b/x\n+1\n"
        );
        assert_eq!(
            strip_preamble("commit abc\n\n    msg\n\ndiff --git a/x b/x\n+1\n"),
            "diff --git a/x b/x\n+1\n"
        );
        assert_eq!(
            strip_preamble("commit abc\nAuthor: x\n"),
            "",
            "commit sans diff"
        );
        assert_eq!(strip_preamble(""), "");
    }
}
