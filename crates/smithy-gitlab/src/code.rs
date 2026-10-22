//! Code et historique : branches, dossiers, fichiers, commits et leur diff.
//!
//! Mesuré : l'arbre (`/repository/tree`) ne donne ni taille ni date ; `/repository/files/…/raw`
//! sert les octets avec `Content-Length` ; une branche inconnue donne **200 `[]`** pour les
//! commits (pas 404) et 404 pour l'arbre ; le diff d'un commit est **par fichier en JSON** (le
//! champ `diff` commence à `@@`, sans en-tête) : `patch_of` reconstruit un correctif unifié, car
//! la route web `.diff` redirige vers la page de connexion sur un dépôt privé.

use std::time::Duration;

use serde::Deserialize;
use smithy_core::{
    Branch, BranchHead, ChangeKind, Commit, CommitDetail, CommitPage, Entry, EntryKind, Error,
    FileChange, FileContent, Repo, Result,
};

use crate::{Http, MAX_PER_PAGE, encode, project};

/// Taille d'une page de l'historique (comme Forgejo : l'application charge au fil de l'eau).
const COMMITS_PAGE: usize = 50;
/// Pages de 100 branches (ou entrées de dossier) au plus.
const MAX_PAGES: usize = 20;
/// Octets regardés pour reconnaître un binaire (octet nul), comme pour Forgejo.
const BINARY_SNIFF: usize = 8 * 1024;
const FILE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ApiBranch {
    name: String,
    #[serde(default)]
    default: bool,
    #[serde(default)]
    protected: bool,
    #[serde(default)]
    commit: Option<ApiCommit>,
}

#[derive(Deserialize)]
struct ApiEntry {
    name: String,
    path: String,
    /// `blob`, `tree` ou `commit` (sous-module).
    #[serde(rename = "type", default)]
    kind: String,
    /// `100644`, `120000` (lien symbolique), `160000` (sous-module)…
    #[serde(default)]
    mode: String,
}

#[derive(Deserialize)]
pub(crate) struct ApiCommit {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    author_name: String,
    #[serde(default)]
    authored_date: Option<String>,
    #[serde(default)]
    committed_date: Option<String>,
    #[serde(default)]
    web_url: Option<String>,
    #[serde(default)]
    parent_ids: Vec<String>,
    #[serde(default)]
    stats: Option<ApiStats>,
}

#[derive(Deserialize)]
struct ApiStats {
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
}

/// Un fichier d'un diff : `diff` ne contient que les morceaux (`@@ … @@`).
#[derive(Deserialize)]
pub(crate) struct ApiFileDiff {
    #[serde(default)]
    old_path: String,
    #[serde(default)]
    new_path: String,
    #[serde(default)]
    a_mode: Option<String>,
    #[serde(default)]
    b_mode: Option<String>,
    #[serde(default)]
    new_file: bool,
    #[serde(default)]
    renamed_file: bool,
    #[serde(default)]
    deleted_file: bool,
    #[serde(default)]
    diff: String,
    /// GitLab a refusé de produire ce diff (fichier trop gros).
    #[serde(default)]
    too_large: bool,
    #[serde(default)]
    collapsed: bool,
}

impl ApiCommit {
    pub(crate) fn to_commit(&self) -> Commit {
        Commit {
            sha: self.id.clone(),
            message: if self.message.is_empty() {
                self.title.clone()
            } else {
                self.message.clone()
            },
            author: self.author_name.clone(),
            date: self
                .authored_date
                .clone()
                .or_else(|| self.committed_date.clone())
                .filter(|d| !d.is_empty()),
            url: self.web_url.clone().filter(|u| !u.is_empty()),
            parents: self.parent_ids.clone(),
        }
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

fn change_kind(f: &ApiFileDiff) -> ChangeKind {
    if f.new_file {
        ChangeKind::Added
    } else if f.deleted_file {
        ChangeKind::Removed
    } else if f.renamed_file {
        ChangeKind::Renamed
    } else {
        ChangeKind::Modified
    }
}

/// Le correctif unifié d'un fichier, avec les en-têtes que GitLab ne donne pas.
fn file_patch(f: &ApiFileDiff) -> String {
    let (old, new) = (&f.old_path, &f.new_path);
    let mut out = format!("diff --git a/{old} b/{new}\n");
    if f.new_file {
        out.push_str(&format!(
            "new file mode {}\n",
            f.b_mode.as_deref().unwrap_or("100644")
        ));
    } else if f.deleted_file {
        out.push_str(&format!(
            "deleted file mode {}\n",
            f.a_mode.as_deref().unwrap_or("100644")
        ));
    } else if f.renamed_file {
        out.push_str(&format!("rename from {old}\nrename to {new}\n"));
    }
    if f.diff.is_empty() {
        // Rien à montrer : un binaire modifié, un fichier vide, un simple renommage…
        let binary = !f.new_file && !f.deleted_file && !f.renamed_file && !f.too_large;
        if binary {
            out.push_str(&format!("Binary files a/{old} and b/{new} differ\n"));
        }
        return out;
    }
    out.push_str(&if f.new_file {
        format!("--- /dev/null\n+++ b/{new}\n")
    } else if f.deleted_file {
        format!("--- a/{old}\n+++ /dev/null\n")
    } else {
        format!("--- a/{old}\n+++ b/{new}\n")
    });
    out.push_str(&f.diff);
    if !f.diff.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Fichiers, lignes ajoutées et lignes retirées d'un correctif unifié : la liste des merge
/// requests n'en donne aucun et le détail seulement `changes_count`.
pub(crate) fn patch_stats(patch: &str) -> (u64, u64, u64) {
    let (mut files, mut added, mut removed) = (0, 0, 0);
    let mut in_hunk = false;
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk {
            // Dans un morceau, `+++`/`---` sont des lignes de contenu, pas des en-têtes.
            match line.as_bytes().first() {
                Some(b'+') => added += 1,
                Some(b'-') => removed += 1,
                _ => {}
            }
        }
    }
    (files, added, removed)
}

/// Assemble les fichiers d'un diff, en s'arrêtant au plafond d'octets. Renvoie le correctif et
/// s'il est incomplet (plafond atteint, ou fichier que GitLab n'a pas voulu produire).
pub(crate) fn patch_of(files: &[ApiFileDiff], max_bytes: usize) -> (String, bool) {
    let mut patch = String::new();
    let mut truncated = false;
    for f in files {
        if f.too_large || f.collapsed {
            truncated = true;
        }
        let part = file_patch(f);
        if patch.len() + part.len() > max_bytes {
            truncated = true;
            break;
        }
        patch.push_str(&part);
    }
    (patch, truncated)
}

impl Http {
    pub(crate) async fn fetch_branches(&self, repo: &Repo) -> Result<Vec<Branch>> {
        let path = format!("/projects/{}/repository/branches", project(repo));
        let mut out: Vec<Branch> = Vec::new();
        let mut page = 1;
        for _ in 0..MAX_PAGES {
            let (batch, next) = self
                .get_page::<Vec<ApiBranch>>(
                    &path,
                    &[
                        ("per_page", MAX_PER_PAGE.to_string()),
                        ("page", page.to_string()),
                    ],
                )
                .await?;
            out.extend(batch.into_iter().map(|b| {
                let mut branch = Branch::new(b.name, b.default);
                branch.protected = b.protected;
                branch.head = b.commit.map(|c| BranchHead {
                    message: c.title.clone(),
                    author: c.author_name.clone(),
                    date: c.committed_date.clone().filter(|d| !d.is_empty()),
                    sha: c.id,
                });
                branch
            }));
            match next {
                Some(n) => page = n,
                None => break,
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
        let api = format!("/projects/{}/repository/tree", project(repo));
        let mut out = Vec::new();
        let mut page = 1;
        for _ in 0..MAX_PAGES {
            let mut query = vec![
                ("per_page", MAX_PER_PAGE.to_string()),
                ("page", page.to_string()),
            ];
            if !reference.is_empty() {
                query.push(("ref", reference.to_owned()));
            }
            if !path.is_empty() {
                query.push(("path", path.to_owned()));
            }
            let (batch, next) = match self.get_page::<Vec<ApiEntry>>(&api, &query).await {
                Ok(r) => r,
                // Dépôt sans commit : pas d'arbre.
                Err(Error::NotFound(_)) if path.is_empty() && reference.is_empty() => {
                    return Ok(Vec::new());
                }
                Err(e) => return Err(e),
            };
            out.extend(batch.into_iter().map(|e| Entry {
                kind: match (e.kind.as_str(), e.mode.as_str()) {
                    ("tree", _) => EntryKind::Dir,
                    ("commit", _) | (_, "160000") => EntryKind::Submodule,
                    (_, "120000") => EntryKind::Symlink,
                    _ => EntryKind::File,
                },
                // L'arbre ne donne pas la taille d'un fichier.
                size: 0,
                name: e.name,
                path: e.path,
            }));
            match next {
                Some(n) => page = n,
                None => break,
            }
        }
        Ok(out)
    }

    pub(crate) async fn fetch_file(
        &self,
        repo: &Repo,
        reference: &str,
        path: &str,
        max_bytes: usize,
    ) -> Result<FileContent> {
        let api = format!(
            "/projects/{}/repository/files/{}/raw",
            project(repo),
            encode(path)
        );
        let mut query = Vec::new();
        if !reference.is_empty() {
            query.push(("ref", reference.to_owned()));
        }
        let (buf, announced, truncated) =
            self.get_raw(&api, &query, max_bytes, FILE_TIMEOUT).await?;
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
            query.push(("ref_name", reference.to_owned()));
        }
        let (list, next) = match self
            .get_page::<Vec<ApiCommit>>(
                &format!("/projects/{}/repository/commits", project(repo)),
                &query,
            )
            .await
        {
            Ok(r) => r,
            // Dépôt sans commit.
            Err(Error::NotFound(_)) if reference.is_empty() => (Vec::new(), None),
            Err(e) => return Err(e),
        };
        // Une branche inconnue répond 200 `[]` (mesuré) : une vraie branche a au moins un commit.
        if list.is_empty() && page <= 1 && !reference.is_empty() {
            return Err(Error::NotFound(reference.to_owned()));
        }
        Ok(CommitPage {
            has_more: next.is_some(),
            commits: list.iter().map(ApiCommit::to_commit).collect(),
        })
    }

    /// Les fichiers d'un diff (commit : `/repository/commits/:sha/diff`), toutes pages, sans lire
    /// au-delà de ce que le plafond d'octets laisse espérer.
    pub(crate) async fn fetch_file_diffs(&self, api: &str) -> Result<Vec<ApiFileDiff>> {
        let mut files = Vec::new();
        let mut page = 1;
        for _ in 0..MAX_PAGES {
            let (batch, next) = self
                .get_page::<Vec<ApiFileDiff>>(
                    api,
                    &[
                        ("per_page", MAX_PER_PAGE.to_string()),
                        ("page", page.to_string()),
                    ],
                )
                .await?;
            files.extend(batch);
            match next {
                Some(n) => page = n,
                None => break,
            }
        }
        Ok(files)
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
        let api = format!("/projects/{}/repository/commits/{sha}", project(repo));
        let diff_api = format!("{api}/diff");
        let (c, files) = tokio::join!(
            self.get::<ApiCommit>(&api, &[]),
            self.fetch_file_diffs(&diff_api)
        );
        let (c, files) = (c?, files?);
        let (patch, patch_truncated) = patch_of(&files, max_bytes);
        let (additions, deletions) = c
            .stats
            .as_ref()
            .map_or((0, 0), |s| (s.additions, s.deletions));
        Ok(CommitDetail {
            commit: c.to_commit(),
            parents: c.parent_ids.clone(),
            files: files
                .iter()
                .map(|f| FileChange {
                    path: f.new_path.clone(),
                    kind: change_kind(f),
                })
                .collect(),
            additions,
            deletions,
            patch,
            patch_truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::*;
    use serde_json::json;
    use smithy_core::Provider;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const BASE: &str = "/api/v4/projects/groupe%2Fsous%2Fapp/repository";

    fn commit_json(id: &str, parents: &[&str]) -> serde_json::Value {
        // Réduit d'une réponse réelle (`/repository/commits`).
        json!({
            "id": id, "short_id": &id[..4], "title": "feat: un", "message": "feat: un\n\ncorps\n",
            "author_name": "Thomas", "authored_date": "2026-10-22T14:41:37.000+02:00",
            "committed_date": "2026-10-22T14:41:38.000+02:00", "parent_ids": parents,
            "web_url": format!("https://gitlab.com/groupe/sous/app/-/commit/{id}")
        })
    }

    #[tokio::test]
    async fn branches_par_defaut_en_tete_avec_leur_tete() {
        let server = MockServer::start().await;
        Mock::given(path(format!("{BASE}/branches")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"name": "autre", "default": false, "protected": false,
                 "commit": commit_json("aaaa1111", &[])},
                {"name": "main", "default": true, "protected": true,
                 "commit": commit_json("bbbb2222", &["aaaa1111"])},
            ])))
            .mount(&server)
            .await;
        let b = gitlab(&server).list_branches(&repo()).await.unwrap();
        assert_eq!(b[0].name, "main");
        assert!(b[0].default && b[0].protected);
        let head = b[0].head.as_ref().unwrap();
        assert_eq!(
            (head.sha.as_str(), head.message.as_str()),
            ("bbbb2222", "feat: un")
        );
        assert_eq!(head.date.as_deref(), Some("2026-10-22T14:41:38.000+02:00"));
    }

    #[tokio::test]
    async fn dossiers_liens_et_fichiers() {
        let server = MockServer::start().await;
        Mock::given(path(format!("{BASE}/tree")))
            .and(query_param("ref", "main"))
            .and(query_param("path", "src"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "1", "name": "sous", "type": "tree", "path": "src/sous", "mode": "040000"},
                {"id": "2", "name": "a b.rs", "type": "blob", "path": "src/a b.rs", "mode": "100644"},
                {"id": "3", "name": "lien", "type": "blob", "path": "src/lien", "mode": "120000"},
                {"id": "4", "name": "mod", "type": "commit", "path": "src/mod", "mode": "160000"},
            ])))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/files/src%2Fa%20b.rs/raw")))
            .and(query_param("ref", "main"))
            .respond_with(ResponseTemplate::new(200).set_body_string("fn main() {}\n"))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/files/bin/raw")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![1u8, 0, 2]))
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let dir = g.read_dir(&repo(), "main", "src").await.unwrap();
        let kinds: Vec<_> = dir.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [
                EntryKind::Dir,
                EntryKind::File,
                EntryKind::Symlink,
                EntryKind::Submodule
            ]
        );
        assert_eq!(dir[1].path, "src/a b.rs");
        let f = g
            .read_file(&repo(), "main", "src/a b.rs", 1 << 20)
            .await
            .unwrap();
        assert_eq!(f.text.as_deref(), Some("fn main() {}\n"));
        assert!(!f.truncated);
        let b = g.read_file(&repo(), "", "bin", 1 << 20).await.unwrap();
        assert!(b.text.is_none(), "octet nul : binaire");
        let cut = g.read_file(&repo(), "main", "src/a b.rs", 4).await.unwrap();
        assert!(cut.truncated);
    }

    #[tokio::test]
    async fn historique_pages_par_en_tete_et_branche_inconnue() {
        let server = MockServer::start().await;
        Mock::given(path(format!("{BASE}/commits")))
            .and(query_param("ref_name", "main"))
            .and(query_param("page", "1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-next-page", "2")
                    .set_body_json(json!([commit_json("bbbb2222", &["aaaa1111"])])),
            )
            .mount(&server)
            .await;
        // Mesuré : une référence inconnue répond 200 avec une liste vide
        Mock::given(path(format!("{BASE}/commits")))
            .and(query_param("ref_name", "fantome"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let page = g.list_commits(&repo(), "main", 1).await.unwrap();
        assert!(page.has_more);
        let c = &page.commits[0];
        assert_eq!(c.summary(), "feat: un");
        assert_eq!(c.parents, ["aaaa1111"]);
        assert_eq!(c.author, "Thomas");
        assert!(matches!(
            g.list_commits(&repo(), "fantome", 1).await,
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn correctif_reconstruit_depuis_les_fichiers() {
        let file = |old: &str, new: &str, flags: (bool, bool, bool), diff: &str| ApiFileDiff {
            old_path: old.into(),
            new_path: new.into(),
            a_mode: Some("100644".into()),
            b_mode: Some("100644".into()),
            new_file: flags.0,
            renamed_file: flags.1,
            deleted_file: flags.2,
            diff: diff.into(),
            too_large: false,
            collapsed: false,
        };
        let files = [
            file(
                "a.js",
                "a.js",
                (false, false, false),
                "@@ -1 +1 @@\n-x\n+y\n",
            ),
            file(
                "n.js",
                "n.js",
                (true, false, false),
                "@@ -0,0 +1 @@\n+neuf\n",
            ),
            file(
                "s.js",
                "s.js",
                (false, false, true),
                "@@ -1 +0,0 @@\n-vieux\n",
            ),
            file("r.js", "r2.js", (false, true, false), ""),
            file("img.png", "img.png", (false, false, false), ""),
        ];
        let (patch, truncated) = patch_of(&files, 1 << 20);
        assert!(!truncated);
        assert!(
            patch.starts_with("diff --git a/a.js b/a.js\n--- a/a.js\n+++ b/a.js\n@@ -1 +1 @@\n")
        );
        assert!(patch.contains("new file mode 100644\n--- /dev/null\n+++ b/n.js\n"));
        assert!(patch.contains("deleted file mode 100644\n--- a/s.js\n+++ /dev/null\n"));
        assert!(patch.contains("rename from r.js\nrename to r2.js\n"));
        assert!(patch.contains("Binary files a/img.png and b/img.png differ\n"));
        assert!(
            !patch.contains("Binary files a/r.js"),
            "un renommage n'est pas un binaire"
        );
        // Plafond : on s'arrête entre deux fichiers
        let (short, cut) = patch_of(&files, 120);
        assert!(cut && short.matches("diff --git").count() < 5);
        // Un fichier que GitLab n'a pas produit rend le correctif incomplet
        let mut big = file("g.bin", "g.bin", (false, false, false), "");
        big.too_large = true;
        assert!(patch_of(&[big], 1 << 20).1);
        assert_eq!(
            strip_preamble("From abc\n\ndiff --git a b\n"),
            "diff --git a b\n"
        );
    }

    #[tokio::test]
    async fn detail_d_un_commit_avec_stats_et_fichiers() {
        let server = MockServer::start().await;
        let mut c = commit_json("cccc3333", &["bbbb2222"]);
        c["stats"] = json!({"additions": 3, "deletions": 1, "total": 4});
        Mock::given(method("GET"))
            .and(path(format!("{BASE}/commits/cccc3333")))
            .respond_with(ResponseTemplate::new(200).set_body_json(c))
            .mount(&server)
            .await;
        Mock::given(path(format!("{BASE}/commits/cccc3333/diff")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                "old_path": "a.js", "new_path": "a.js", "a_mode": "100644", "b_mode": "100644",
                "new_file": false, "renamed_file": false, "deleted_file": false,
                "diff": "@@ -1 +1 @@\n-x\n+y\n", "too_large": false, "collapsed": false
            }])))
            .mount(&server)
            .await;
        let g = gitlab(&server);
        let d = g.commit_detail(&repo(), "cccc3333", 1 << 20).await.unwrap();
        assert_eq!((d.additions, d.deletions), (3, 1));
        assert_eq!(
            d.files,
            [FileChange {
                path: "a.js".into(),
                kind: ChangeKind::Modified
            }]
        );
        assert!(d.patch.starts_with("diff --git a/a.js b/a.js\n"));
        assert!(!d.patch_truncated);
        assert_eq!(d.parents, ["bbbb2222"]);
        assert!(matches!(
            g.commit_detail(&repo(), "../x", 1).await,
            Err(Error::NotFound(_))
        ));
    }
}
