//! Code et historique : branches, dossiers, fichiers, commits et leur diff.
//!
//! Mesuré : `/branches` ne donne du commit de tête que son empreinte (ni message ni date) ; le
//! contenu brut d'un fichier s'obtient sur `/contents/{chemin}` avec
//! `Accept: application/vnd.github.raw+json` ; le diff d'un commit avec
//! `Accept: application/vnd.github.diff`. Un dépôt sans commit répond 409 (commits) ou 404
//! (contenu) : traités comme des listes vides.

use std::time::Duration;

use serde::Deserialize;
use smithy_core::{
    Branch, ChangeKind, Commit, CommitDetail, CommitPage, Entry, EntryKind, Error, FileChange,
    FileContent, Repo, Result,
};

use crate::{Http, MAX_PER_PAGE, encode_path};

/// Taille d'une page de l'historique (comme Forgejo : l'application charge au fil de l'eau).
const COMMITS_PAGE: usize = 50;
/// Pages de 100 branches au plus.
const BRANCH_PAGES: usize = 10;
/// Octets regardés pour reconnaître un binaire (octet nul), comme pour Forgejo.
const BINARY_SNIFF: usize = 8 * 1024;
const FILE_TIMEOUT: Duration = Duration::from_secs(60);
const RAW: &str = "application/vnd.github.raw+json";
const DIFF: &str = "application/vnd.github.diff";

#[derive(Deserialize)]
struct ApiBranch {
    name: String,
    #[serde(default)]
    protected: bool,
}

#[derive(Deserialize)]
struct ApiDefault {
    #[serde(default)]
    default_branch: String,
}

#[derive(Deserialize)]
struct ApiEntry {
    name: String,
    path: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    size: u64,
}

#[derive(Deserialize)]
pub(crate) struct ApiCommit {
    sha: String,
    #[serde(default)]
    html_url: Option<String>,
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
    author: Option<ApiSignature>,
}

#[derive(Deserialize, Default)]
struct ApiSignature {
    #[serde(default)]
    name: String,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Deserialize)]
struct ApiParent {
    sha: String,
}

#[derive(Deserialize)]
struct ApiFile {
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
    pub(crate) fn to_commit(&self) -> Commit {
        let author = self.commit.author.as_ref();
        Commit {
            sha: self.sha.clone(),
            message: self.commit.message.clone(),
            author: author.map(|a| a.name.clone()).unwrap_or_default(),
            date: author
                .and_then(|a| a.date.clone())
                .filter(|d| !d.is_empty()),
            url: self.html_url.clone().filter(|u| !u.is_empty()),
            parents: self.parents.iter().map(|p| p.sha.clone()).collect(),
        }
    }
}

fn entry_kind(t: &str) -> EntryKind {
    match t {
        "dir" => EntryKind::Dir,
        "symlink" => EntryKind::Symlink,
        "submodule" => EntryKind::Submodule,
        _ => EntryKind::File,
    }
}

/// `added`, `removed`, `modified`, `renamed`, `copied`, `changed`, `unchanged`.
pub(crate) fn change_kind(status: &str) -> ChangeKind {
    match status {
        "added" => ChangeKind::Added,
        "modified" | "changed" => ChangeKind::Modified,
        "removed" => ChangeKind::Removed,
        "renamed" => ChangeKind::Renamed,
        _ => ChangeKind::Other,
    }
}

/// Une empreinte ne contient que des chiffres hexadécimaux : tout le reste est refusé avant
/// d'être mis dans une URL.
fn valid_sha(sha: &str) -> bool {
    (4..=64).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// UTF-8 avec remplacement ; une coupure en plein caractère ne laisse pas de `�` final.
pub(crate) fn decode_text(buf: &[u8], truncated: bool) -> String {
    match std::str::from_utf8(buf) {
        Ok(s) => s.to_owned(),
        Err(e) if truncated && e.error_len().is_none() => {
            String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(buf).into_owned(),
    }
}

/// Ce qui précède le premier `diff --git` n'est pas du diff.
pub(crate) fn strip_preamble(patch: &str) -> &str {
    if patch.starts_with("diff --git") {
        return patch;
    }
    patch.find("\ndiff --git").map_or("", |i| &patch[i + 1..])
}

impl Http {
    fn repo_path(repo: &Repo) -> String {
        format!("/repos/{}/{}", repo.owner, repo.name)
    }

    pub(crate) async fn fetch_branches(&self, repo: &Repo) -> Result<Vec<Branch>> {
        let base = Self::repo_path(repo);
        let info: ApiDefault = self.get(&base, &[]).await?;
        let mut out: Vec<Branch> = Vec::new();
        for page in 1..=BRANCH_PAGES {
            let batch: Vec<ApiBranch> = self
                .get(
                    &format!("{base}/branches"),
                    &[
                        ("per_page", MAX_PER_PAGE.to_string()),
                        ("page", page.to_string()),
                    ],
                )
                .await?;
            let n = batch.len();
            out.extend(batch.into_iter().map(|b| {
                let mut branch = Branch::new(b.name.clone(), b.name == info.default_branch);
                branch.protected = b.protected;
                branch
            }));
            if n < MAX_PER_PAGE {
                break;
            }
        }
        // La branche par défaut en premier, comme le veut `Provider::list_branches`.
        out.sort_by_key(|b| !b.default);
        Ok(out)
    }

    pub(crate) async fn fetch_dir(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
    ) -> Result<Vec<Entry>> {
        let api = format!("{}/contents/{}", Self::repo_path(repo), encode_path(path));
        let mut query = Vec::new();
        if !reference.is_empty() {
            query.push(("ref", reference.to_owned()));
        }
        let value: serde_json::Value = match self.get(&api, &query).await {
            Ok(v) => v,
            // Dépôt sans commit : « This repository is empty. »
            Err(Error::NotFound(_)) if path.is_empty() => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        // Un fichier répond par un objet, un dossier par une liste.
        let list: Vec<ApiEntry> = match value {
            serde_json::Value::Array(_) => {
                serde_json::from_value(value).map_err(|e| Error::Decode(e.to_string()))?
            }
            _ => return Err(Error::NotFound(format!("{path} n'est pas un dossier"))),
        };
        Ok(list
            .into_iter()
            .map(|e| Entry {
                kind: entry_kind(&e.kind),
                size: if e.kind == "dir" { 0 } else { e.size },
                name: e.name,
                path: e.path,
            })
            .collect())
    }

    pub(crate) async fn fetch_file(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<FileContent> {
        let api = format!("{}/contents/{}", Self::repo_path(repo), encode_path(path));
        let mut query = Vec::new();
        if !reference.is_empty() {
            query.push(("ref", reference.to_owned()));
        }
        let (buf, announced, truncated) = self
            .get_raw(&api, &query, RAW, max_bytes, FILE_TIMEOUT)
            .await?;
        let size = announced.unwrap_or(buf.len() as u64);
        let text = if buf[..buf.len().min(BINARY_SNIFF)].contains(&0) {
            None
        } else {
            Some(decode_text(&buf, truncated))
        };
        Ok(FileContent {
            size,
            text,
            truncated,
        })
    }

    pub(crate) async fn fetch_commits(
        &self,
        repo: &Repo,
        reference: &str,
        page: u32,
    ) -> Result<CommitPage> {
        let mut query = vec![
            ("per_page", COMMITS_PAGE.to_string()),
            ("page", page.max(1).to_string()),
        ];
        if !reference.is_empty() {
            query.push(("sha", reference.to_owned()));
        }
        let list: Vec<ApiCommit> = match self
            .get(&format!("{}/commits", Self::repo_path(repo)), &query)
            .await
        {
            Ok(list) => list,
            Err(Error::NotFound(_)) => return Err(Error::NotFound(reference.to_owned())),
            // Dépôt sans commit : 409 « Git Repository is empty. »
            Err(Error::Network(m)) if m.starts_with("HTTP 409") => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(CommitPage {
            has_more: list.len() >= COMMITS_PAGE,
            commits: list.iter().map(ApiCommit::to_commit).collect(),
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
        let api = format!("{}/commits/{sha}", Self::repo_path(repo));
        let c: ApiCommit = self.get(&api, &[]).await?;
        let (patch, patch_truncated) = self.fetch_diff(&api, max_bytes).await?;
        let (additions, deletions) = c
            .stats
            .as_ref()
            .map_or((0, 0), |s| (s.additions, s.deletions));
        Ok(CommitDetail {
            commit: c.to_commit(),
            parents: c.parents.iter().map(|p| p.sha.clone()).collect(),
            files: c
                .files
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|f| FileChange {
                    path: f.filename.clone(),
                    kind: change_kind(&f.status),
                })
                .collect(),
            additions,
            deletions,
            patch,
            patch_truncated,
        })
    }

    /// Le diff d'un commit ou d'une PR (même chemin que leur JSON, autre `Accept`). Trop gros,
    /// GitHub refuse de le produire (406) : on le dit coupé plutôt que d'échouer.
    pub(crate) async fn fetch_diff(&self, api: &str, max_bytes: usize) -> Result<(String, bool)> {
        match self.get_raw(api, &[], DIFF, max_bytes, FILE_TIMEOUT).await {
            Ok((buf, _, truncated)) => {
                let text = decode_text(&buf, truncated);
                Ok((strip_preamble(&text).to_owned(), truncated))
            }
            Err(Error::Network(m)) if m.starts_with("HTTP 406") => Ok((String::new(), true)),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn branches_par_defaut_en_tete_et_protegees() {
        let server = MockServer::start().await;
        Mock::given(path("/repos/octo/app"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/app/branches"))
            .and(query_param("per_page", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"name": "dev", "commit": {"sha": "a1", "url": "u"}, "protected": false},
                {"name": "main", "commit": {"sha": "b2", "url": "u"}, "protected": true}
            ])))
            .mount(&server)
            .await;
        let b = github(&server).list_branches(&repo()).await.unwrap();
        let got: Vec<_> = b
            .iter()
            .map(|b| (b.name.as_str(), b.default, b.protected, b.head.is_none()))
            .collect();
        assert_eq!(
            got,
            [("main", true, true, true), ("dev", false, false, true)]
        );
    }

    #[tokio::test]
    async fn dossier_fichier_brut_binaire_et_depot_vide() {
        let server = MockServer::start().await;
        Mock::given(path("/repos/octo/app/contents/"))
            .and(query_param("ref", "feature/x"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"name": "src", "path": "src", "type": "dir", "size": 0},
                {"name": "a b.txt", "path": "a b.txt", "type": "file", "size": 5},
                {"name": "lien", "path": "lien", "type": "symlink", "size": 3},
                {"name": "mod", "path": "mod", "type": "submodule", "size": 0}
            ])))
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/app/contents/a%20b.txt"))
            .and(header("Accept", RAW))
            .respond_with(ResponseTemplate::new(200).set_body_string("bonjour\n"))
            .mount(&server)
            .await;
        Mock::given(path("/repos/octo/app/contents/bin"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8, 0, 2, 3]))
            .mount(&server)
            .await;
        let g = github(&server);
        let dir = g.read_dir(&repo(), "feature/x", "").await.unwrap();
        let kinds: Vec<_> = dir.iter().map(|e| (e.name.as_str(), e.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("src", EntryKind::Dir),
                ("a b.txt", EntryKind::File),
                ("lien", EntryKind::Symlink),
                ("mod", EntryKind::Submodule)
            ]
        );
        let f = g.read_file(&repo(), "", "a b.txt", 1 << 20).await.unwrap();
        assert_eq!(f.text.as_deref(), Some("bonjour\n"));
        assert!(!f.truncated);
        let cut = g.read_file(&repo(), "", "a b.txt", 3).await.unwrap();
        assert_eq!((cut.text.as_deref(), cut.truncated), (Some("bon"), true));
        let bin = g.read_file(&repo(), "", "bin", 100).await.unwrap();
        assert_eq!(bin.text, None);
        // Dépôt vide : la racine répond 404
        let empty = MockServer::start().await;
        Mock::given(path("/repos/octo/app/contents/"))
            .respond_with(
                ResponseTemplate::new(404)
                    .set_body_json(json!({"message": "This repository is empty."})),
            )
            .mount(&empty)
            .await;
        assert!(
            github(&empty)
                .read_dir(&repo(), "", "")
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn commit_json(n: usize) -> serde_json::Value {
        json!({
            "sha": format!("{n:040x}"), "html_url": format!("https://github.com/octo/app/commit/{n}"),
            "commit": {"message": format!("feat: n°{n}\n\ncorps"),
                       "author": {"name": "Ana", "email": "a@x", "date": "2026-10-14T10:00:00Z"}},
            "author": {"login": "ana"}, "parents": []
        })
    }

    #[tokio::test]
    async fn historique_detail_et_diff() {
        let server = MockServer::start().await;
        let full: Vec<_> = (0..COMMITS_PAGE).map(commit_json).collect();
        Mock::given(method("GET"))
            .and(path("/repos/octo/app/commits"))
            .and(query_param("sha", "dev"))
            .and(query_param("per_page", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(full))
            .mount(&server)
            .await;
        let sha = "6c3e842f0d56c947ba064d98bedeb9fe64f8fc67";
        let mut detail = commit_json(7);
        detail["sha"] = json!(sha);
        detail["parents"] = json!([{"sha": "aaaa1111", "url": "u"}]);
        detail["stats"] = json!({"total": 3, "additions": 2, "deletions": 1});
        detail["files"] = json!([
            {"filename": "a.txt", "status": "added", "patch": "@@"},
            {"filename": "b.txt", "status": "removed"},
            {"filename": "c.txt", "status": "renamed", "previous_filename": "d.txt"}
        ]);
        Mock::given(method("GET"))
            .and(path(format!("/repos/octo/app/commits/{sha}")))
            .and(header("Accept", DIFF))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("diff --git a/a.txt b/a.txt\n+x\n"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/octo/app/commits/{sha}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(detail))
            .mount(&server)
            .await;
        let g = github(&server);
        let page = g.list_commits(&repo(), "dev", 1).await.unwrap();
        assert!(page.has_more && page.commits.len() == COMMITS_PAGE);
        let c = &page.commits[3];
        assert_eq!((c.summary(), c.author.as_str()), ("feat: n°3", "Ana"));
        assert_eq!(c.date.as_deref(), Some("2026-10-14T10:00:00Z"));
        let d = g.commit_detail(&repo(), sha, 1 << 20).await.unwrap();
        assert_eq!(d.parents, ["aaaa1111"]);
        assert_eq!((d.additions, d.deletions), (2, 1));
        let kinds: Vec<_> = d.files.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [ChangeKind::Added, ChangeKind::Removed, ChangeKind::Renamed]
        );
        assert!(d.patch.starts_with("diff --git a/a.txt"));
        assert!(matches!(
            g.commit_detail(&repo(), "../x", 10).await,
            Err(Error::NotFound(_))
        ));
        // Dépôt sans commit : 409, une liste vide
        let empty = MockServer::start().await;
        Mock::given(path("/repos/octo/app/commits"))
            .respond_with(
                ResponseTemplate::new(409)
                    .set_body_json(json!({"message": "Git Repository is empty."})),
            )
            .mount(&empty)
            .await;
        let none = github(&empty).list_commits(&repo(), "", 1).await.unwrap();
        assert!(none.commits.is_empty() && !none.has_more);
    }
}
