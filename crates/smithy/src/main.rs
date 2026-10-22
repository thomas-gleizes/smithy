mod app;
mod browser;
mod clipboard;
mod config;
mod diff;
mod graph;
mod highlight;
mod history;
mod logview;
mod markdown;
mod mouse;
mod pages;
mod pulls;
mod recent;
mod request;
mod session;
mod textsearch;
mod timeago;
mod ui;
mod workspace;
mod wrap;

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::DefaultTerminal;
use smithy_core::{Error, Provider};
use smithy_forgejo::Forgejo;
use smithy_github::GitHub;
use smithy_gitlab::GitLab;
use tokio::sync::mpsc::{self, UnboundedSender};

use app::{App, Msg};
use browser::{Browser, FILE_LIMIT_BYTES, Screen};
use config::{Server, ServerKind};
use diff::DiffTarget;
use mouse::Target;
use recent::RecentStore;
use request::Request;
use session::{Session, SessionStore};

const REFRESH: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

#[tokio::main]
async fn main() -> Result<()> {
    // Les arguments d'abord : `--help` doit répondre sans configuration.
    let fresh = parse_args()?;
    let cfg = config::load()?;
    let wanted = std::env::var("SMITHY_SERVER").ok();
    // Vérifie le nom demandé (erreur claire) avant d'ouvrir le terminal. Sans serveur, rien à
    // vérifier : l'application en fait configurer un au lancement.
    if wanted.is_some() {
        cfg.server(wanted.as_deref())?;
    }
    let store = SessionStore::default_location();

    let mut terminal = ratatui::init();
    mouse_capture(true);
    let result = run(&mut terminal, cfg.servers.clone(), wanted, store, fresh).await;
    mouse_capture(false);
    ratatui::restore();
    result
}

/// Souris et trackpad (clic, molette). Le terminal ne sélectionne alors plus le texte de
/// lui-même : la plupart le permettent encore avec Maj (Option sur iTerm2 / Terminal.app).
fn mouse_capture(on: bool) {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let mut out = std::io::stdout();
    if on {
        let _ = crossterm::execute!(out, EnableMouseCapture);
        // `ratatui::init` remet le terminal en état après un plantage, mais pas la souris : sans
        // cela, le shell recevrait ensuite chaque mouvement sous forme de séquences.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
            previous(info);
        }));
    } else {
        let _ = crossterm::execute!(out, DisableMouseCapture);
    }
}

const USAGE: &str = "smithy : consulter la CI et le code de vos forges git dans le terminal

Usage : smithy [--fresh]

  --fresh     ne rouvre pas les onglets de la dernière session (démarre sur l'accueil)
  --version   affiche la version
  --help      affiche ce message

Configuration : voir le README (serveurs dans ~/.config/smithy/config.toml).";

/// Les arguments : `--fresh` seul. Renvoie `true` s'il faut démarrer sans restaurer les onglets.
fn parse_args() -> Result<bool> {
    let mut fresh = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--fresh" => fresh = true,
            "-V" | "--version" => {
                println!("smithy {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("argument inconnu : {other}\n\n{USAGE}"),
        }
    }
    Ok(fresh)
}

/// Le fournisseur d'un serveur, quel que soit son type : `Provider` n'est pas utilisable en
/// `dyn` (méthodes `impl Future`), on délègue donc à la main.
enum AnyProvider {
    Forgejo(Forgejo),
    GitHub(GitHub),
    GitLab(GitLab),
}

macro_rules! delegate {
    ($self:ident, $p:ident => $call:expr) => {
        match $self {
            AnyProvider::Forgejo($p) => $call,
            AnyProvider::GitHub($p) => $call,
            AnyProvider::GitLab($p) => $call,
        }
    };
}

impl Provider for AnyProvider {
    fn capabilities(&self) -> smithy_core::Capabilities {
        delegate!(self, p => p.capabilities())
    }

    async fn list_repos(
        &self,
        source: &smithy_core::RepoSource,
    ) -> smithy_core::Result<Vec<smithy_core::RepoInfo>> {
        delegate!(self, p => p.list_repos(source).await)
    }

    async fn repo_info(
        &self,
        repo: &smithy_core::Repo,
    ) -> smithy_core::Result<smithy_core::RepoInfo> {
        delegate!(self, p => p.repo_info(repo).await)
    }

    async fn list_pipelines(
        &self,
        repo: &smithy_core::Repo,
    ) -> smithy_core::Result<Vec<smithy_core::Pipeline>> {
        delegate!(self, p => p.list_pipelines(repo).await)
    }

    async fn list_branches(
        &self,
        repo: &smithy_core::Repo,
    ) -> smithy_core::Result<Vec<smithy_core::Branch>> {
        delegate!(self, p => p.list_branches(repo).await)
    }

    async fn list_commits(
        &self,
        repo: &smithy_core::Repo,
        reference: &str,
        page: u32,
    ) -> smithy_core::Result<smithy_core::CommitPage> {
        delegate!(self, p => p.list_commits(repo, reference, page).await)
    }

    async fn commit_detail(
        &self,
        repo: &smithy_core::Repo,
        sha: &str,
        max_bytes: usize,
    ) -> smithy_core::Result<smithy_core::CommitDetail> {
        delegate!(self, p => p.commit_detail(repo, sha, max_bytes).await)
    }

    async fn list_pulls(
        &self,
        repo: &smithy_core::Repo,
        filter: smithy_core::PullFilter,
        page: u32,
    ) -> smithy_core::Result<smithy_core::PullPage> {
        delegate!(self, p => p.list_pulls(repo, filter, page).await)
    }

    async fn pull_detail(
        &self,
        repo: &smithy_core::Repo,
        number: u64,
        max_bytes: usize,
    ) -> smithy_core::Result<smithy_core::PullDetail> {
        delegate!(self, p => p.pull_detail(repo, number, max_bytes).await)
    }

    async fn read_dir(
        &self,
        repo: &smithy_core::Repo,
        reference: &str,
        path: &str,
    ) -> smithy_core::Result<Vec<smithy_core::Entry>> {
        delegate!(self, p => p.read_dir(repo, reference, path).await)
    }

    async fn read_file(
        &self,
        repo: &smithy_core::Repo,
        reference: &str,
        path: &str,
        max_bytes: usize,
    ) -> smithy_core::Result<smithy_core::FileContent> {
        delegate!(self, p => p.read_file(repo, reference, path, max_bytes).await)
    }

    async fn job_log(
        &self,
        repo: &smithy_core::Repo,
        pipeline: &smithy_core::Pipeline,
    ) -> smithy_core::Result<smithy_core::JobLog> {
        delegate!(self, p => p.job_log(repo, pipeline).await)
    }
}

/// `refresh` : de quoi renouveler le token OAuth d'un serveur GitLab (il expire en 2 h) ; le
/// nouveau token est réécrit dans la session sauvegardée.
fn build_provider(
    server: &Server,
    token: String,
    cookies: BTreeMap<String, String>,
    refresh: Option<String>,
    store: Option<&SessionStore>,
) -> Arc<AnyProvider> {
    Arc::new(match server.kind {
        ServerKind::Forgejo => {
            AnyProvider::Forgejo(Forgejo::new(server.url.clone(), token).with_cookies(cookies))
        }
        // Ni cookie ni route web : le token suffit à tout.
        ServerKind::GitHub => AnyProvider::GitHub(GitHub::new(server.url.clone(), token)),
        ServerKind::GitLab => {
            let gitlab = GitLab::new(&server.url, token);
            match (refresh, device_client_id(server)) {
                (Some(refresh), Some(client_id)) => {
                    let (store, name, url) =
                        (store.cloned(), server.name.clone(), server.url.clone());
                    AnyProvider::GitLab(gitlab.with_refresh(client_id, refresh, move |t| {
                        // Rien à faire d'une erreur d'écriture ici : le token garde jusqu'au
                        // prochain renouvellement, et la reconnexion reste possible.
                        if let Some(store) = &store {
                            let _ = store.save(
                                &name,
                                &Session {
                                    url: url.clone(),
                                    username: String::new(),
                                    token: Some(t.access_token.clone()),
                                    refresh_token: t.refresh_token.clone(),
                                    cookies: BTreeMap::new(),
                                },
                            );
                        }
                    }))
                }
                _ => AnyProvider::GitLab(gitlab),
            }
        }
    })
}

/// Il manque des identifiants à ce serveur : le login (Forgejo), ou la saisie du token (GitHub,
/// qui n'a pas de connexion par mot de passe).
fn ask_credentials(
    app: &mut App,
    backends: &HashMap<String, Backend>,
    name: &str,
    switch_to: bool,
) {
    let Some(b) = backends.get(name) else { return };
    match b.server.kind {
        ServerKind::Forgejo => app.open_login(name, &b.username, switch_to),
        // Connexion par code si on connaît l'identifiant de l'OAuth App, sinon un token à saisir.
        ServerKind::GitHub | ServerKind::GitLab if device_client_id(&b.server).is_some() => {
            app.open_device(name, switch_to)
        }
        ServerKind::GitHub | ServerKind::GitLab => app.open_token_form(name),
    }
}

/// Identifiant public de l'OAuth App : `SMITHY_GITHUB_CLIENT_ID` (GitHub Enterprise, ou une App
/// à soi) prime sur celui compilé dans le binaire.
fn github_client_id() -> Option<String> {
    std::env::var("SMITHY_GITHUB_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            Some(smithy_github::oauth::DEFAULT_CLIENT_ID.to_owned()).filter(|v| !v.is_empty())
        })
}

/// L'identifiant de l'application OAuth à utiliser pour la connexion par code de ce serveur.
/// GitLab : `SMITHY_GITLAB_CLIENT_ID`, sinon celui de gitlab.com (une instance auto-hébergée n'a
/// pas l'application de smithy : jeton à saisir, ou son propre identifiant).
fn device_client_id(server: &Server) -> Option<String> {
    match server.kind {
        ServerKind::Forgejo => None,
        ServerKind::GitHub => github_client_id(),
        ServerKind::GitLab => gitlab_client_id(&server.url),
    }
}

fn gitlab_client_id(url: &str) -> Option<String> {
    std::env::var("SMITHY_GITLAB_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .or_else(|| {
            smithy_gitlab::is_default_host(url)
                .then(|| smithy_gitlab::oauth::DEFAULT_CLIENT_ID.to_owned())
        })
}

/// Connexion par code : demande le code (montré à l'utilisateur par `Msg::Device`), attend son
/// autorisation, puis rend le token comme un login réussi (`Msg::LoggedIn`). Sans cookie : le
/// token suffit à GitHub.
fn spawn_device(
    server: &Server,
    tx: &UnboundedSender<(String, Msg)>,
) -> tokio::task::JoinHandle<()> {
    let (server, tx) = (server.clone(), tx.clone());
    tokio::spawn(async move {
        let name = server.name.clone();
        let fail = |e: String| {
            let _ = tx.send((
                name.clone(),
                Msg::Device {
                    server: name.clone(),
                    result: Err(e),
                },
            ));
        };
        let Some(client_id) = device_client_id(&server) else {
            return fail("identifiant d'application OAuth absent".to_owned());
        };
        // Les deux forges font le même protocole ; seuls le serveur web, les noms et ce que
        // rend l'attente (GitLab : aussi de quoi renouveler le token) changent.
        let polled = match server.kind {
            ServerKind::GitLab => {
                let web = smithy_gitlab::oauth::web_base(&server.url);
                let code = match smithy_gitlab::oauth::request_code(&web, &client_id).await {
                    Ok(c) => c,
                    Err(e) => return fail(e),
                };
                let shown = app::DeviceInfo {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                };
                let _ = tx.send((
                    name.clone(),
                    Msg::Device {
                        server: name.clone(),
                        result: Ok(shown),
                    },
                ));
                smithy_gitlab::oauth::poll_token(&web, &client_id, &code)
                    .await
                    .map(|t| (t.access_token, t.refresh_token))
            }
            _ => {
                let web = smithy_github::oauth::web_base(&server.url);
                let code = match smithy_github::oauth::request_code(&web, &client_id).await {
                    Ok(c) => c,
                    Err(e) => return fail(e),
                };
                let shown = app::DeviceInfo {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                };
                let _ = tx.send((
                    name.clone(),
                    Msg::Device {
                        server: name.clone(),
                        result: Ok(shown),
                    },
                ));
                smithy_github::oauth::poll_token(&web, &client_id, &code)
                    .await
                    .map(|t| (t, None))
            }
        };
        match polled {
            Ok((token, refresh_token)) => {
                let _ = tx.send((
                    name.clone(),
                    Msg::LoggedIn {
                        server: name.clone(),
                        result: Ok(Session {
                            url: server.url.clone(),
                            username: String::new(),
                            token: Some(token),
                            refresh_token,
                            cookies: BTreeMap::new(),
                        }),
                    },
                ));
            }
            Err(e) => fail(e),
        }
    })
}

/// Touches des onglets, valables sur toutes les pages. Renvoie `true` si la touche est prise.
fn tab_key(app: &mut App, code: KeyCode) -> bool {
    // En pleine saisie (filtre, recherche), `t`, `x` et les autres sont du texte.
    if app.is_typing() {
        return false;
    }
    match code {
        KeyCode::Tab => app.switch_tab(1),
        KeyCode::BackTab => app.switch_tab(-1),
        KeyCode::Char('t') => app.open_in_new_tab(),
        KeyCode::Char('x') => app.close_tab(),
        KeyCode::Char('T') => app.open_tab_picker(),
        _ => return false,
    }
    true
}

/// Touches de la liste des onglets (`T`).
fn tab_picker_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('q' | 'T') => app.tab_picker = None,
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => app.tab_picker_move(1),
        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => app.tab_picker_move(-1),
        KeyCode::Home | KeyCode::Char('g') => app.tab_picker_move(isize::MIN / 2),
        KeyCode::End | KeyCode::Char('G') => app.tab_picker_move(isize::MAX / 2),
        KeyCode::Enter => app.tab_picker_go(),
        KeyCode::Char('x') => app.tab_picker_close(),
        _ => {}
    }
}

/// Touches de l'accueil. Pendant une saisie (filtre ou recherche), presque tout est du texte.
fn home_key(app: &mut App, code: KeyCode) {
    if app.home().is_some_and(|h| h.editing) {
        match code {
            KeyCode::Enter => app.home_commit(),
            KeyCode::Esc => {
                if let Some(h) = app.home_mut() {
                    h.cancel_edit();
                }
            }
            KeyCode::Backspace => {
                if let Some(h) = app.home_mut() {
                    h.backspace();
                }
            }
            KeyCode::Char(c) => {
                if let Some(h) = app.home_mut() {
                    h.type_char(c);
                }
            }
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        // Échap ne quitte jamais : à l'accueil il ne fait que retirer le filtre.
        KeyCode::Esc => {
            if let Some(h) = app.home_mut() {
                h.clear_filter();
            }
        }
        KeyCode::Char('/') => {
            if let Some(h) = app.home_mut() {
                h.start_edit();
            }
        }
        KeyCode::Char('j') | KeyCode::Down => app.list_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.list_move(-1),
        KeyCode::PageDown | KeyCode::Char('f') => app.list_page(1),
        KeyCode::PageUp | KeyCode::Char('b') => app.list_page(-1),
        KeyCode::Home | KeyCode::Char('g') => app.list_move(isize::MIN / 2),
        KeyCode::End | KeyCode::Char('G') => app.list_move(isize::MAX / 2),
        KeyCode::Right => app.home_switch(1),
        KeyCode::Left => app.home_switch(-1),
        KeyCode::Enter => app.home_enter(),
        KeyCode::Char('r') => app.refresh_current(),
        _ => {}
    }
}

/// Saisie de la recherche dans un volet de texte : presque tout est du texte.
fn text_prompt_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Enter => app.text_prompt_commit(),
        KeyCode::Esc => app.text_prompt_cancel(),
        KeyCode::Backspace => app.text_prompt_backspace(),
        KeyCode::Char(c) => app.text_prompt_type(c),
        _ => {}
    }
}

/// Touches de la liste des runs : le filtre (`/` texte, `s` statut), puis la liste.
fn runs_key(app: &mut App, code: KeyCode) {
    if app.runs().is_some_and(|r| r.editing) {
        if let Some(r) = app.runs_mut() {
            match code {
                KeyCode::Enter => r.commit(),
                KeyCode::Esc => r.cancel_edit(),
                KeyCode::Backspace => r.backspace(),
                KeyCode::Char(c) => r.type_char(c),
                _ => {}
            }
        }
        return;
    }
    match code {
        KeyCode::Char('/') => {
            if let Some(r) = app.runs_mut() {
                r.start_edit();
            }
        }
        KeyCode::Char('s') => {
            if let Some(r) = app.runs_mut() {
                r.cycle_status();
            }
        }
        // Échap retire d'abord les filtres, puis il fait son travail habituel (retour).
        KeyCode::Esc if app.runs_clear_filters() => {}
        _ => list_key(app, code, App::open_logs),
    }
}

/// Déplacements communs aux listes (`j`/`k`, pages, début et fin).
fn list_nav(app: &mut App, code: KeyCode) -> bool {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.list_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.list_move(-1),
        KeyCode::PageDown | KeyCode::Char('f') => app.list_page(1),
        KeyCode::PageUp | KeyCode::Char('b') => app.list_page(-1),
        KeyCode::Home | KeyCode::Char('g') => app.list_move(isize::MIN / 2),
        KeyCode::End | KeyCode::Char('G') => app.list_move(isize::MAX / 2),
        _ => return false,
    }
    true
}

/// Touches de la liste des commits : sélecteur de branche, filtre, liste.
fn commits_key(app: &mut App, code: KeyCode) {
    if app.commits().is_some_and(|v| v.picker.is_some()) {
        let mut requests = Vec::new();
        if let Some(v) = app.commits_mut() {
            match code {
                KeyCode::Esc | KeyCode::Char('q' | 'B') => v.picker = None,
                KeyCode::Char('j') | KeyCode::Down => v.picker_move(1),
                KeyCode::Char('k') | KeyCode::Up => v.picker_move(-1),
                KeyCode::Home | KeyCode::Char('g') => v.picker_move(isize::MIN / 2),
                KeyCode::End | KeyCode::Char('G') => v.picker_move(isize::MAX / 2),
                KeyCode::Enter => requests = v.picker_choose(),
                _ => {}
            }
        }
        app.history_queue(requests);
        return;
    }
    if app.commits().is_some_and(|v| v.filter.editing) {
        let mut requests = Vec::new();
        if let Some(v) = app.commits_mut() {
            match code {
                KeyCode::Enter => v.filter.commit(),
                KeyCode::Esc => {
                    v.filter.cancel();
                    requests = v.filter_changed();
                }
                KeyCode::Backspace => {
                    v.filter.backspace();
                    requests = v.filter_changed();
                }
                KeyCode::Char(c) => {
                    v.filter.type_char(c);
                    requests = v.filter_changed();
                }
                _ => {}
            }
        }
        app.history_queue(requests);
        return;
    }
    match code {
        KeyCode::Char('/') => {
            if let Some(v) = app.commits_mut() {
                v.filter.start();
            }
        }
        KeyCode::Char('B') => {
            let requests = app
                .commits_mut()
                .map(|v| v.open_picker())
                .unwrap_or_default();
            app.history_queue(requests);
        }
        // Échap retire d'abord le filtre, puis il revient en arrière.
        KeyCode::Esc if app.commits_mut().is_some_and(|v| v.filter.clear()) => {
            let requests = app
                .commits_mut()
                .map(|v| v.filter_changed())
                .unwrap_or_default();
            app.history_queue(requests);
        }
        KeyCode::Enter | KeyCode::Right => app.commits_enter(),
        KeyCode::Char('r') => app.refresh_current(),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
            app.pop();
        }
        _ => {
            list_nav(app, code);
        }
    }
}

/// Touches de la liste des branches.
fn branch_list_key(app: &mut App, code: KeyCode) {
    if app.branch_list().is_some_and(|v| v.filter.editing) {
        if let Some(v) = app.branch_list_mut() {
            match code {
                KeyCode::Enter => v.filter.commit(),
                KeyCode::Esc => {
                    v.filter.cancel();
                    v.filter_changed();
                }
                KeyCode::Backspace => {
                    v.filter.backspace();
                    v.filter_changed();
                }
                KeyCode::Char(c) => {
                    v.filter.type_char(c);
                    v.filter_changed();
                }
                _ => {}
            }
        }
        return;
    }
    match code {
        KeyCode::Char('/') => {
            if let Some(v) = app.branch_list_mut() {
                v.filter.start();
            }
        }
        KeyCode::Esc
            if app.branch_list_mut().is_some_and(|v| {
                let had = v.filter.clear();
                if had {
                    v.filter_changed();
                }
                had
            }) => {}
        KeyCode::Enter | KeyCode::Right => app.branches_enter(),
        KeyCode::Char('e') => app.branches_code(),
        KeyCode::Char('r') => app.refresh_current(),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
            app.pop();
        }
        _ => {
            list_nav(app, code);
        }
    }
}

/// Touches de la liste des pull requests : filtre local (`/`), état (`s`), liste.
fn pulls_key(app: &mut App, code: KeyCode) {
    if app.pulls().is_some_and(|v| v.filter.editing) {
        let mut requests = Vec::new();
        if let Some(v) = app.pulls_mut() {
            match code {
                KeyCode::Enter => v.filter.commit(),
                KeyCode::Esc => {
                    v.filter.cancel();
                    requests = v.filter_changed();
                }
                KeyCode::Backspace => {
                    v.filter.backspace();
                    requests = v.filter_changed();
                }
                KeyCode::Char(c) => {
                    v.filter.type_char(c);
                    requests = v.filter_changed();
                }
                _ => {}
            }
        }
        app.history_queue(requests);
        return;
    }
    match code {
        KeyCode::Char('/') => {
            if let Some(v) = app.pulls_mut() {
                v.filter.start();
            }
        }
        KeyCode::Char('s') => {
            let requests = app.pulls_mut().map(|v| v.cycle_state()).unwrap_or_default();
            app.history_queue(requests);
        }
        // Échap retire d'abord le filtre, puis il revient en arrière.
        KeyCode::Esc if app.pulls_mut().is_some_and(|v| v.filter.clear()) => {
            let requests = app
                .pulls_mut()
                .map(|v| v.filter_changed())
                .unwrap_or_default();
            app.history_queue(requests);
        }
        KeyCode::Enter | KeyCode::Right => app.pulls_enter(),
        KeyCode::Char('e') => app.pull_code(),
        KeyCode::Char('r') => app.refresh_current(),
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
            app.pop();
        }
        _ => {
            list_nav(app, code);
        }
    }
}

/// Touches du détail d'un commit : comme les logs (sections par fichier, recherche).
fn commit_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            if !app.text_escape() {
                app.pop();
            }
        }
        KeyCode::Char('q') | KeyCode::Backspace => {
            app.pop();
        }
        KeyCode::Char('/') => app.text_search_begin(),
        KeyCode::Char('n') => app.text_search_step(1),
        KeyCode::Char('N') => app.text_search_step(-1),
        KeyCode::Char('j') | KeyCode::Down => app.text_scroll(1),
        KeyCode::Char('k') | KeyCode::Up => app.text_scroll(-1),
        KeyCode::Char(' ') => app.text_toggle_section(),
        KeyCode::Right => app.text_select_section(1),
        KeyCode::Left => app.text_select_section(-1),
        KeyCode::Char('a') => app.text_toggle_all(),
        KeyCode::PageDown | KeyCode::Char('f') => app.text_page(1),
        KeyCode::PageUp | KeyCode::Char('b') => app.text_page(-1),
        KeyCode::Char('g') | KeyCode::Home => app.text_top(),
        KeyCode::Char('G') | KeyCode::End => app.text_bottom(),
        KeyCode::Char('w') => app.text_toggle_wrap(),
        KeyCode::Char('v') => app.text_toggle_split(),
        KeyCode::Char('r') => app.refresh_current(),
        _ => {}
    }
}

/// Touches de la page d'un dépôt et de la liste de ses runs.
fn list_key(app: &mut App, code: KeyCode, enter: fn(&mut App)) {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
            app.pop();
        }
        KeyCode::Char('j') | KeyCode::Down => app.list_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.list_move(-1),
        KeyCode::PageDown | KeyCode::Char('f') => app.list_page(1),
        KeyCode::PageUp | KeyCode::Char('b') => app.list_page(-1),
        KeyCode::Home | KeyCode::Char('g') => app.list_move(isize::MIN / 2),
        KeyCode::End | KeyCode::Char('G') => app.list_move(isize::MAX / 2),
        KeyCode::Enter | KeyCode::Right => enter(app),
        KeyCode::Char('r') => app.refresh_current(),
        _ => {}
    }
}

/// Touches de l'explorateur de code : un écran à la fois (dossier, sélecteur de branche, fichier).
fn browser_key(app: &mut App, code: KeyCode) {
    let Some(screen) = app.browser().map(Browser::screen) else {
        return;
    };
    match screen {
        Screen::File => match code {
            // Échap efface d'abord une recherche active.
            KeyCode::Esc if app.text_escape() => {}
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
                if let Some(b) = app.browser_mut() {
                    b.up();
                }
            }
            KeyCode::Char('/') => app.text_search_begin(),
            KeyCode::Char('n') => app.text_search_step(1),
            KeyCode::Char('N') => app.text_search_step(-1),
            KeyCode::Char('j') | KeyCode::Down => app.text_scroll(1),
            KeyCode::Char('k') | KeyCode::Up => app.text_scroll(-1),
            KeyCode::PageDown | KeyCode::Char('f') => app.text_page(1),
            KeyCode::PageUp | KeyCode::Char('b') => app.text_page(-1),
            KeyCode::Char('g') | KeyCode::Home => app.text_top(),
            KeyCode::Char('G') | KeyCode::End => app.text_bottom(),
            KeyCode::Char('w') => app.text_toggle_wrap(),
            KeyCode::Char('m') => {
                if let Some(b) = app.browser_mut() {
                    b.toggle_rendered();
                }
            }
            KeyCode::Char('r') => app.refresh_current(),
            _ => {}
        },
        Screen::Branches => {
            let Some(b) = app.browser_mut() else {
                return;
            };
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    b.back();
                }
                KeyCode::Char('j') | KeyCode::Down => b.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => b.move_selection(-1),
                KeyCode::Home | KeyCode::Char('g') => b.select_first(),
                KeyCode::End | KeyCode::Char('G') => b.select_last(),
                KeyCode::Enter | KeyCode::Right => b.enter(),
                KeyCode::Char('r') => b.refresh(),
                _ => {}
            }
        }
        Screen::Dir if app.browser().is_some_and(|b| b.editing) => {
            // Saisie du filtre par nom : presque tout est du texte.
            if let Some(b) = app.browser_mut() {
                match code {
                    KeyCode::Enter => b.filter_commit(),
                    KeyCode::Esc => b.filter_cancel(),
                    KeyCode::Backspace => b.filter_backspace(),
                    KeyCode::Char(c) => b.filter_type(c),
                    _ => {}
                }
            }
        }
        Screen::Dir => match code {
            // Échap retire d'abord le filtre, puis il ferme l'explorateur.
            KeyCode::Esc if app.browser_mut().is_some_and(Browser::clear_filter) => {}
            KeyCode::Esc | KeyCode::Char('q') => {
                if app.browser_mut().is_some_and(Browser::back) {
                    app.close_browser();
                }
            }
            KeyCode::PageDown | KeyCode::Char('f') => app.list_page(1),
            KeyCode::PageUp | KeyCode::Char('b') => app.list_page(-1),
            _ => {
                let Some(b) = app.browser_mut() else {
                    return;
                };
                match code {
                    KeyCode::Char('j') | KeyCode::Down => b.move_selection(1),
                    KeyCode::Char('k') | KeyCode::Up => b.move_selection(-1),
                    KeyCode::Home | KeyCode::Char('g') => b.select_first(),
                    KeyCode::End | KeyCode::Char('G') => b.select_last(),
                    KeyCode::Enter | KeyCode::Right => b.enter(),
                    KeyCode::Left | KeyCode::Backspace => b.up(),
                    KeyCode::Char('/') => b.start_filter(),
                    KeyCode::Char('B') => b.open_branches(),
                    KeyCode::Char('r') => b.refresh(),
                    _ => {}
                }
            }
        },
    }
}

/// Lance une requête de l'application ; la réponse revient par le canal sous forme de `Msg`.
fn run_request<P: Provider + 'static>(
    p: &Arc<P>,
    tx: &UnboundedSender<(String, Msg)>,
    server: String,
    req: Request,
) {
    let (p, tx) = (p.clone(), tx.clone());
    tokio::spawn(async move {
        let mut styling = None;
        let mut diff_styling = None;
        let msg = match req {
            Request::Repos(source) => {
                let result = p.list_repos(&source).await;
                Msg::Repos { source, result }
            }
            Request::RepoInfo(repo) => {
                let result = p.repo_info(&repo).await;
                Msg::RepoInfo { repo, result }
            }
            Request::Pipelines(repo) => {
                let result = p.list_pipelines(&repo).await.map_err(|e| e.to_string());
                Msg::Pipelines { repo, result }
            }
            Request::Logs { repo, pipeline } => {
                let result = p.job_log(&repo, &pipeline).await;
                Msg::Logs {
                    repo,
                    pipeline_id: pipeline.id,
                    result,
                }
            }
            Request::Branches(repo) => {
                let result = p.list_branches(&repo).await;
                Msg::Branches { repo, result }
            }
            Request::Commits {
                repo,
                reference,
                page,
            } => {
                let result = p.list_commits(&repo, &reference, page).await;
                Msg::Commits {
                    repo,
                    reference,
                    page,
                    result,
                }
            }
            Request::CommitDetail { repo, sha } => {
                let result = p
                    .commit_detail(&repo, &sha, history::PATCH_LIMIT_BYTES)
                    .await;
                if let Ok(d) = &result {
                    diff_styling = Some((
                        repo.clone(),
                        DiffTarget::Commit(sha.clone()),
                        d.patch.clone(),
                    ));
                }
                Msg::CommitDetail { repo, sha, result }
            }
            Request::Pulls { repo, filter, page } => {
                let result = p.list_pulls(&repo, filter, page).await;
                Msg::Pulls {
                    repo,
                    filter,
                    page,
                    result,
                }
            }
            Request::PullDetail { repo, number } => {
                let result = p
                    .pull_detail(&repo, number, pulls::PATCH_LIMIT_BYTES)
                    .await
                    .map(Box::new);
                if let Ok(d) = &result {
                    diff_styling = Some((repo.clone(), DiffTarget::Pull(number), d.patch.clone()));
                }
                Msg::PullDetail {
                    repo,
                    number,
                    result,
                }
            }
            Request::Dir {
                repo,
                reference,
                path,
            } => {
                let result = p.read_dir(&repo, &reference, &path).await;
                Msg::Dir {
                    repo,
                    reference,
                    path,
                    result,
                }
            }
            Request::File {
                repo,
                reference,
                path,
            } => {
                let result = p
                    .read_file(&repo, &reference, &path, FILE_LIMIT_BYTES)
                    .await;
                // Le texte s'affiche tout de suite ; la coloration suit, calculée à part.
                if let Some(text) = result.as_ref().ok().and_then(|c| c.text.as_deref()) {
                    let (lines, _) = browser::file_lines(text);
                    if !lines.is_empty() {
                        let source = markdown::is_markdown(&path).then(|| text.to_owned());
                        styling =
                            Some((repo.clone(), reference.clone(), path.clone(), lines, source));
                    }
                }
                Msg::File {
                    repo,
                    reference,
                    path,
                    result,
                }
            }
        };
        let _ = tx.send((server.clone(), msg));
        if let Some((repo, target, patch)) =
            diff_styling.filter(|(_, _, p): &(_, _, String)| !p.is_empty())
        {
            // Même principe que pour un fichier : le texte est affiché, la coloration suit.
            let key = diff::patch_key(&patch);
            let job = tokio::task::spawn_blocking(move || {
                diff::highlight_patch(&patch, history::PATCH_LIMIT_LINES)
            });
            if let Ok(styles) = job.await {
                let _ = tx.send((
                    server.clone(),
                    Msg::DiffStyles {
                        repo,
                        target,
                        key,
                        styles,
                    },
                ));
            }
        }
        if let Some((repo, reference, path, lines, markdown_source)) = styling {
            // Chargement des grammaires et analyse : hors du fil de l'interface.
            let job = tokio::task::spawn_blocking(move || {
                let styles = highlight::highlight_file(&path, &lines);
                // Markdown : la vue mise en forme, avec ses blocs de code colorés (seules les
                // régions nous intéressent : le texte est déjà affiché).
                let rendered = markdown_source.map(|src| {
                    let doc = markdown::render(&src, true);
                    let fp = logview::fingerprint_of(doc.iter().map(|l| &l.text));
                    (fp, doc.into_iter().map(|l| l.runs).collect::<Vec<_>>())
                });
                if styles.is_none() && rendered.is_none() {
                    return None;
                }
                Some(Msg::FileStyles {
                    repo,
                    reference,
                    path,
                    fingerprint: logview::fingerprint_of(&lines),
                    styles: styles.unwrap_or_default(),
                    rendered,
                })
            });
            if let Ok(Some(msg)) = job.await {
                let _ = tx.send((server, msg));
            }
        }
    });
}

/// Un seul mot de passe, jamais conservé : il donne le cookie de session et, au premier
/// login seulement, un token. Si `known_token` existe déjà, on ne fait que renouveler le
/// cookie : sinon chaque expiration de session laisserait un token de plus sur le compte.
fn spawn_login(
    server: &Server,
    tx: &UnboundedSender<(String, Msg)>,
    username: String,
    password: String,
    known_token: Option<String>,
) {
    let (server, tx) = (server.clone(), tx.clone());
    tokio::spawn(async move {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Nom reconnaissable dans les paramètres du compte, pour pouvoir le révoquer.
        let token_name = format!("smithy-{}-{secs}", server.name);
        let create = known_token.is_none().then_some(token_name.as_str());
        let result = match server.kind {
            ServerKind::Forgejo => {
                smithy_forgejo::login(&server.url, &username, &password, create).await
            }
            // Jamais ouvert pour GitHub ni GitLab (voir `ask_credentials`).
            ServerKind::GitHub | ServerKind::GitLab => {
                Err(Error::Unsupported("connexion par mot de passe"))
            }
        }
        .map(|l| Session {
            url: server.url.clone(),
            username: l.username,
            token: l.token.or(known_token),
            refresh_token: None,
            cookies: l.cookies,
        })
        .map_err(|e| e.to_string());
        let name = server.name.clone();
        let _ = tx.send((
            name.clone(),
            Msg::LoggedIn {
                server: name,
                result,
            },
        ));
    });
}

/// Ce que `main` sait d'un serveur : son fournisseur s'il est connecté, et de quoi se
/// reconnecter sans créer de token de plus.
struct Backend {
    server: Server,
    provider: Option<Arc<AnyProvider>>,
    username: String,
    saved_token: Option<String>,
    /// Pour renouveler le token OAuth (GitLab) sans refaire la connexion.
    saved_refresh: Option<String>,
}

impl Backend {
    /// Le token de la config prime ; sinon celui créé lors d'un login précédent. Les cookies,
    /// eux, viennent toujours de la session sauvegardée.
    fn load(server: Server, store: Option<&SessionStore>) -> Self {
        let saved = store.and_then(|s| s.load(&server.name, &server.url).ok().flatten());
        let username = saved.as_ref().map_or(String::new(), |s| s.username.clone());
        let saved_token = saved.as_ref().and_then(|s| s.token.clone());
        let saved_refresh = saved.as_ref().and_then(|s| s.refresh_token.clone());
        let token = server.token.clone().or_else(|| saved_token.clone());
        let cookies = saved.map(|s| s.cookies).unwrap_or_default();
        // Le token de la config n'expire pas : seul celui d'une session se renouvelle.
        let refresh = saved_refresh.clone().filter(|_| server.token.is_none());
        let provider = token.map(|t| build_provider(&server, t, cookies, refresh, store));
        Self {
            server,
            provider,
            username,
            saved_token,
            saved_refresh,
        }
    }

    /// Un token de la config suffit aussi : pas besoin d'en créer.
    fn known_token(&self) -> Option<String> {
        self.server
            .token
            .clone()
            .or_else(|| self.saved_token.clone())
    }
}

fn announce(app: &mut App, b: &Backend) {
    if let Some(p) = &b.provider {
        app.set_caps(
            &b.server.name,
            p.capabilities().logs,
            p.capabilities().browse,
        );
    }
}

fn entries(servers: &[Server]) -> Vec<app::ServerEntry> {
    servers
        .iter()
        .map(|s| app::ServerEntry {
            name: s.name.clone(),
            url: s.url.clone(),
            kind: s.kind,
        })
        .collect()
}

/// Les serveurs connus de l'application, dans l'ordre de la liste.
fn current_servers(app: &App, backends: &HashMap<String, Backend>) -> Vec<Server> {
    app.servers
        .iter()
        .filter_map(|e| backends.get(&e.name).map(|b| b.server.clone()))
        .collect()
}

/// Les serveurs que le fichier ne connaît pas (définis par `SMITHY_URL`) sont ajoutés en tête,
/// sans token, pour qu'écrire le fichier ne les fasse pas disparaître ni ne copie un secret.
fn file_with_env_servers(known: &[Server]) -> Result<Vec<Server>, String> {
    let mut file = config::file_servers().map_err(|e| e.to_string())?;
    for s in known.iter().rev() {
        if !file.iter().any(|f| f.name == s.name) {
            file.insert(
                0,
                Server {
                    token: None,
                    ..s.clone()
                },
            );
        }
    }
    Ok(file)
}

/// Ajoute ou modifie un serveur : écrit d'abord la config (rien ne change si ça échoue), puis
/// met à jour le registre des fournisseurs et l'état de l'application.
fn save_server(
    app: &mut App,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    draft: app::ServerDraft,
) -> Result<(), String> {
    let known = current_servers(app, backends);
    let old = draft
        .original
        .as_ref()
        .and_then(|n| known.iter().find(|s| &s.name == n));
    let server = Server {
        name: draft.name.clone(),
        kind: draft.kind,
        url: draft.url.clone(),
        token: draft
            .token
            .clone()
            .or_else(|| old.and_then(|o| o.token.clone())),
    };
    let mut file = file_with_env_servers(&known)?;
    // Dans le fichier, on ne réécrit que ce que l'utilisateur vient de saisir.
    let file_server = Server {
        token: draft.token.clone().or_else(|| {
            file.iter()
                .find(|f| f.name == server.name)
                .and_then(|f| f.token.clone())
        }),
        ..server.clone()
    };
    match file.iter_mut().find(|f| f.name == server.name) {
        Some(f) => *f = file_server,
        None => file.push(file_server),
    }
    config::save_servers(&file).map_err(|e| format!("config non enregistrée : {e:#}"))?;

    let mut list = known;
    match list.iter_mut().find(|s| s.name == server.name) {
        Some(s) => *s = server.clone(),
        None => list.push(server.clone()),
    }
    backends.insert(server.name.clone(), Backend::load(server.clone(), store));
    match backends[&server.name].provider {
        Some(_) => announce(app, &backends[&server.name]),
        None => app.forget_caps(&server.name),
    }
    app.server_form_done(entries(&list), &server.name);
    // Premier serveur du premier lancement : l'onglet, encore sans serveur, devient le sien.
    if app.server_name.is_empty() {
        app.set_initial_server(&server.name);
    }
    // L'onglet affiché est sur ce serveur et il n'est plus connecté (URL changée) : login.
    if server.name == app.server_name
        && !app.is_connected(&server.name)
        && app.login.is_none()
        && app.server_form.is_none()
    {
        ask_credentials(app, backends, &server.name, false);
    }
    Ok(())
}

/// Retire un serveur de la config, du registre et de la liste ; sa session sauvegardée est effacée.
fn delete_server(
    app: &mut App,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    name: &str,
) -> Result<(), String> {
    let known = current_servers(app, backends);
    let mut file = config::file_servers().map_err(|e| e.to_string())?;
    if file.iter().any(|f| f.name == name) {
        file.retain(|f| f.name != name);
        config::save_servers(&file).map_err(|e| format!("config non enregistrée : {e:#}"))?;
    }
    backends.remove(name);
    if let Some(Err(e)) = store.map(|s| s.clear(name)) {
        app.error = Some(format!("session non effacée : {e}"));
    }
    app.forget_caps(name);
    let list: Vec<Server> = known.into_iter().filter(|s| s.name != name).collect();
    app.server_delete_done(entries(&list));
    // L'onglet affiché a pu passer sur un autre serveur, pas encore connecté.
    let shown = app.server_name.clone();
    if !app.is_connected(&shown) && app.login.is_none() && app.device.is_none() {
        ask_credentials(app, backends, &shown, false);
    }
    Ok(())
}

/// Oublie la session sauvegardée d'un serveur (cookie et token issus d'une connexion) : on peut
/// alors refaire le parcours de connexion, ou changer de compte. Un token écrit dans la config
/// reste (c'est le choix de l'utilisateur : `e` pour le retirer). Rien n'est révoqué côté forge.
fn logout_server(
    app: &mut App,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    name: &str,
) {
    let Some(b) = backends.get_mut(name) else {
        return;
    };
    if let Some(Err(e)) = store.map(|s| s.clear(name)) {
        app.server_note = Some(format!("session non effacée : {e}"));
        return;
    }
    b.saved_token = None;
    b.saved_refresh = None;
    b.username.clear();
    let from_config = b.server.token.clone();
    b.provider = from_config
        .clone()
        .map(|t| build_provider(&b.server, t, BTreeMap::new(), None, store));
    if b.provider.is_some() {
        announce(app, b);
        app.server_note =
            Some("session effacée ; le token de config.toml reste (e pour le retirer)".to_owned());
        return;
    }
    app.forget_caps(name);
    if name == app.server_name {
        app.close_server_picker();
        ask_credentials(app, backends, name, false);
    } else {
        app.server_note = Some(format!("« {name} » déconnecté"));
    }
}

/// Une touche, sur la page affichée ou la fenêtre ouverte par-dessus.
fn on_key(
    app: &mut App,
    code: KeyCode,
    ctrl: bool,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    tx: &UnboundedSender<(String, Msg)>,
) {
    if ctrl && code == KeyCode::Char('c') {
        app.should_quit = true;
    } else if let Some(form) = app.login.as_ref().map(|f| f.server.clone()) {
        match code {
            KeyCode::Esc => app.should_quit = app.login_escape(),
            KeyCode::Enter => {
                if let Some((user, pass)) = app.login_enter() {
                    let b = &backends[&form];
                    spawn_login(&b.server, tx, user, pass, b.known_token());
                }
            }
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => app.login_toggle_field(),
            KeyCode::Backspace => app.login_backspace(),
            KeyCode::Char(c) if !ctrl => app.login_type(c),
            _ => {}
        }
    } else if app.device.is_some() {
        match code {
            KeyCode::Esc => app.should_quit = app.device_escape(),
            KeyCode::Enter => app.device_retry(),
            KeyCode::Char('t') if !ctrl => app.device_manual(),
            KeyCode::Char('c') if !ctrl => app.device_copy(false),
            KeyCode::Char('u') if !ctrl => app.device_copy(true),
            _ => {}
        }
    } else if app.server_form.is_some() {
        match code {
            KeyCode::Esc => app.should_quit = app.server_form_escape(),
            KeyCode::Enter => {
                if let Some(draft) = app.server_form_enter()
                    && let Err(e) = save_server(app, backends, store, draft)
                {
                    app.server_form_fail(e);
                }
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if app
                    .server_form
                    .as_ref()
                    .is_some_and(|f| f.field == app::ServerField::Kind) =>
            {
                app.server_form_toggle_kind()
            }
            KeyCode::Tab | KeyCode::Down => app.server_form_move(1),
            KeyCode::BackTab | KeyCode::Up => app.server_form_move(-1),
            KeyCode::Backspace => app.server_form_backspace(),
            KeyCode::Char(c) if !ctrl => app.server_form_type(c),
            _ => {}
        }
    } else if app.server_picker.is_some() {
        if app.server_delete.is_some() {
            match code {
                KeyCode::Char('y' | 'o') => {
                    if let Some(name) = app.server_delete_confirm()
                        && let Err(e) = delete_server(app, backends, store, &name)
                    {
                        app.server_note = Some(e);
                    }
                }
                _ => app.server_delete_cancel(),
            }
        } else {
            match code {
                KeyCode::Esc | KeyCode::Char('q' | 'S') => app.close_server_picker(),
                KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => app.server_picker_move(1),
                KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => app.server_picker_move(-1),
                KeyCode::Char('a') => app.server_form_add(),
                KeyCode::Char('e') => app.server_form_edit(),
                KeyCode::Char('d') => app.server_delete_ask(),
                KeyCode::Char('x') => {
                    if let Some(name) = app.server_logout_ask() {
                        logout_server(app, backends, store, &name);
                    }
                }
                KeyCode::Enter => {
                    // Pas encore connecté : le login d'abord, la bascule ensuite.
                    if let Some(name) = app.server_picker_choose() {
                        ask_credentials(app, backends, &name, true);
                    }
                }
                _ => {}
            }
        }
    } else if app.tab_picker.is_some() {
        tab_picker_key(app, code);
    } else if code == KeyCode::Char('S') && !app.is_typing() {
        app.open_server_picker();
    } else if tab_key(app, code) {
        // pris par les onglets
    } else if app.text_prompt_active() {
        text_prompt_key(app, code);
    } else if app.home().is_some() {
        home_key(app, code);
    } else if app.repo_page().is_some() {
        list_key(app, code, App::repo_enter);
    } else if app.commits().is_some() {
        commits_key(app, code);
    } else if app.branch_list().is_some() {
        branch_list_key(app, code);
    } else if app.commit_view().is_some() {
        commit_key(app, code);
    } else if app.pulls().is_some() {
        pulls_key(app, code);
    } else if app.pull_view().is_some() {
        // Comme un commit (sections, recherche), plus `e` : le code de la branche.
        if code == KeyCode::Char('e') {
            app.pull_code();
        } else {
            commit_key(app, code);
        }
    } else if app.runs().is_some() {
        runs_key(app, code);
    } else if app.logs().is_some() {
        match code {
            KeyCode::Esc => {
                if !app.text_escape() {
                    app.close_logs();
                }
            }
            KeyCode::Char('q') | KeyCode::Backspace => app.close_logs(),
            KeyCode::Char('/') => app.text_search_begin(),
            KeyCode::Char('n') => app.text_search_step(1),
            KeyCode::Char('N') => app.text_search_step(-1),
            KeyCode::Char('j') | KeyCode::Down => app.text_scroll(1),
            KeyCode::Char('k') | KeyCode::Up => app.text_scroll(-1),
            KeyCode::Char(' ') => app.text_toggle_section(),
            KeyCode::Right => app.text_select_section(1),
            KeyCode::Left => app.text_select_section(-1),
            KeyCode::Char('a') => app.text_toggle_all(),
            KeyCode::PageDown | KeyCode::Char('f') => app.text_page(1),
            KeyCode::PageUp | KeyCode::Char('b') => app.text_page(-1),
            KeyCode::Char('g') | KeyCode::Home => app.text_top(),
            KeyCode::Char('G') | KeyCode::End => app.text_bottom(),
            KeyCode::Char('w') => app.text_toggle_wrap(),
            KeyCode::Char('r') => app.refetch_logs(),
            _ => {}
        }
    } else if app.browser().is_some() {
        browser_key(app, code);
    }
}

/// Lignes défilées par un cran de molette dans un volet de texte.
const WHEEL_LINES: isize = 3;

/// Souris et trackpad : la molette fait défiler, un clic choisit ce qui est sous le pointeur
/// (les zones viennent du rendu, voir `mouse`). Un clic sur la ligne déjà sélectionnée l'ouvre,
/// comme `Entrée` : un double-clic revient donc au même.
fn on_mouse(
    app: &mut App,
    m: MouseEvent,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    tx: &UnboundedSender<(String, Msg)>,
) {
    // Les formulaires se remplissent au clavier.
    if app.login.is_some() || app.device.is_some() || app.server_form.is_some() {
        return;
    }
    let delta = match m.kind {
        MouseEventKind::ScrollDown => 1,
        MouseEventKind::ScrollUp => -1,
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(target) = app.zones.hit(m.column, m.row) {
                click(app, target, backends, store, tx);
            }
            return;
        }
        _ => return,
    };
    if app.picker_selected().is_some() {
        app.picker_move(delta);
    } else if app.list_selected().is_some() {
        app.list_move(delta);
    } else {
        app.text_scroll(delta * WHEEL_LINES);
    }
}

fn click(
    app: &mut App,
    target: Target,
    backends: &mut HashMap<String, Backend>,
    store: Option<&SessionStore>,
    tx: &UnboundedSender<(String, Msg)>,
) {
    // Une suppression attend sa confirmation : comme au clavier, autre chose l'annule.
    if app.server_delete.is_some() {
        app.server_delete_cancel();
        return;
    }
    let typing = app.is_typing();
    match target {
        Target::Tab(i) => app.go_tab(i),
        Target::Section(i) if !typing => app.home_section(i),
        Target::Crumb(None) if !typing => app.open_server_picker(),
        Target::Crumb(Some(page)) => app.pop_to(page),
        Target::Row(i) if !typing => {
            if let Some(current) = app.list_selected() {
                if current == i {
                    on_key(app, KeyCode::Enter, false, backends, store, tx);
                } else {
                    app.list_move(i as isize - current as isize);
                }
            }
        }
        Target::PickerRow(i) => {
            if let Some(current) = app.picker_selected() {
                if current == i {
                    on_key(app, KeyCode::Enter, false, backends, store, tx);
                } else {
                    app.picker_move(i as isize - current as isize);
                }
            }
        }
        Target::TextRow(row) if !typing => app.text_click(row),
        Target::Outside => on_key(app, KeyCode::Esc, false, backends, store, tx),
        _ => {}
    }
}

async fn run(
    terminal: &mut DefaultTerminal,
    servers: Vec<Server>,
    wanted: Option<String>,
    store: Option<SessionStore>,
    fresh: bool,
) -> Result<()> {
    let recent_store = RecentStore::default_location();
    let (tx, mut rx) = mpsc::unbounded_channel::<(String, Msg)>();
    let mut app = App::new();
    app.github_device = github_client_id().is_some();
    app.gitlab_device_env =
        std::env::var("SMITHY_GITLAB_CLIENT_ID").is_ok_and(|v| !v.trim().is_empty());
    app.set_servers(
        servers
            .iter()
            .map(|s| app::ServerEntry {
                name: s.name.clone(),
                url: s.url.clone(),
                kind: s.kind,
            })
            .collect(),
    );
    if let Some(r) = &recent_store {
        app.recents = r.load();
    }

    // Tous les serveurs qui ont déjà des identifiants sont prêts dès le départ.
    let mut backends: HashMap<String, Backend> = servers
        .into_iter()
        .map(|s| (s.name.clone(), Backend::load(s, store.as_ref())))
        .collect();
    let order: Vec<String> = app.servers.iter().map(|s| s.name.clone()).collect();
    for name in &order {
        announce(&mut app, &backends[name]);
    }
    // Une seule instance restaure et sauvegarde les onglets (voir `workspace`).
    let dir = workspace::state_dir();
    let (tabs_store, owner_lock) = match dir.as_deref().map(workspace::acquire) {
        Some(workspace::Ownership::Owner(lock)) => {
            (dir.as_deref().map(workspace::TabsStore::in_dir), Some(lock))
        }
        Some(workspace::Ownership::Other) => {
            app.shared_instance = true;
            (None, None)
        }
        _ => (None, None),
    };
    let restored = !fresh
        && tabs_store
            .as_ref()
            .and_then(workspace::TabsStore::load)
            .is_some_and(|saved| app.restore(saved));
    // Sinon : le serveur demandé, sinon le premier qui a des identifiants (sinon le premier,
    // avec login). Restauré, c'est le serveur de l'onglet affiché.
    let initial = if restored {
        app.server_name.clone()
    } else {
        let initial = wanted
            .filter(|w| backends.contains_key(w))
            .or_else(|| {
                order
                    .iter()
                    .find(|n| backends[*n].provider.is_some())
                    .cloned()
            })
            .or_else(|| order.first().cloned())
            .unwrap_or_default();
        app.set_initial_server(&initial);
        initial
    };
    let mut last_saved_tabs: Option<workspace::SavedTabs> = None;
    match backends.get(&initial) {
        // Premier lancement : aucun serveur configuré, le formulaire d'ajout s'impose.
        None => app.open_first_server_form(),
        Some(b) if b.provider.is_none() => ask_credentials(&mut app, &backends, &initial, false),
        Some(_) => {}
    }
    let mut last_refresh = Instant::now();
    let mut device_task: Option<tokio::task::JoinHandle<()>> = None;

    let _lock = owner_lock;
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;
        if let Some(text) = app.take_copy() {
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(clipboard::osc52(&text).as_bytes());
            let _ = out.flush();
        }

        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                    on_key(&mut app, k.code, ctrl, &mut backends, store.as_ref(), &tx);
                }
                Event::Mouse(m) => on_mouse(&mut app, m, &mut backends, store.as_ref(), &tx),
                _ => {}
            }
        }

        while let Ok((origin, msg)) = rx.try_recv() {
            let mut save_error = None;
            match &msg {
                Msg::LoggedIn {
                    server: name,
                    result: Ok(session),
                } => {
                    if let Some(b) = backends.get_mut(name) {
                        b.username.clone_from(&session.username);
                        b.saved_token.clone_from(&session.token);
                        b.saved_refresh.clone_from(&session.refresh_token);
                        // Le token de la config prime ; les cookies viennent de ce login.
                        if let Some(token) = b.known_token() {
                            let refresh =
                                b.saved_refresh.clone().filter(|_| b.server.token.is_none());
                            b.provider = Some(build_provider(
                                &b.server,
                                token,
                                session.cookies.clone(),
                                refresh,
                                store.as_ref(),
                            ));
                            announce(&mut app, b);
                        }
                        if let Some(Err(e)) = store.as_ref().map(|s| s.save(name, session)) {
                            save_error = Some(format!("session non sauvegardée : {e}"));
                        }
                    }
                }
                // Une session expirée ne fait surgir le login que pour le serveur de l'onglet
                // affiché : un onglet en arrière-plan attendra qu'on y revienne.
                Msg::Logs {
                    result: Err(Error::SessionExpired),
                    ..
                } if app.login.is_none() && app.device.is_none() && origin == app.server_name => {
                    ask_credentials(&mut app, &backends, &origin, false);
                }
                _ => {}
            }
            let logged_in = matches!(&msg, Msg::LoggedIn { result: Ok(_), .. });
            app.apply_for(&origin, msg);
            if logged_in {
                // Des logs étaient demandés quand la session a expiré : on les relance.
                app.refetch_logs();
            }
            // Après `apply`, qui efface le bandeau d'erreur.
            if save_error.is_some() {
                app.error = save_error;
            }
        }

        // Connexion par code : la tâche qui attend l'autorisation s'arrête avec la fenêtre.
        if app.device.is_none()
            && let Some(t) = device_task.take()
        {
            t.abort();
        }
        if let Some(name) = app.take_device_start() {
            if let Some(t) = device_task.take() {
                t.abort();
            }
            if let Some(b) = backends.get(&name) {
                device_task = Some(spawn_device(&b.server, &tx));
            }
        }

        if last_refresh.elapsed() >= REFRESH {
            app.tick();
            last_refresh = Instant::now();
        }
        // Sondes de joignabilité demandées par la liste des serveurs.
        for (name, url) in app.take_probes() {
            let tx = tx.clone();
            let kind = backends
                .get(&name)
                .map(|b| b.server.kind)
                .unwrap_or_default();
            tokio::spawn(async move {
                let result = match kind {
                    ServerKind::Forgejo => smithy_forgejo::ping(&url).await,
                    ServerKind::GitHub => smithy_github::ping(&url).await,
                    ServerKind::GitLab => smithy_gitlab::ping(&url).await,
                };
                let _ = tx.send((
                    name.clone(),
                    Msg::Reach {
                        server: name,
                        url,
                        result,
                    },
                ));
            });
        }
        // Chaque requête part vers le fournisseur de son serveur ; sans fournisseur (pas encore
        // connecté), elle attend dans la file.
        let mut waiting = Vec::new();
        for (name, req) in app.take_tagged_requests() {
            match backends.get(&name).and_then(|b| b.provider.as_ref()) {
                Some(p) => run_request(p, &tx, name, req),
                None => waiting.push((name, req)),
            }
        }
        app.requeue(waiting);
        if app.recents_dirty {
            app.recents_dirty = false;
            match recent_store.as_ref().map(|r| r.save_merged(&app.recents)) {
                Some(Ok(merged)) => app.recents = merged,
                Some(Err(e)) => app.error = Some(format!("historique non sauvegardé : {e}")),
                None => {}
            }
        }
        // Les onglets sont réécrits dès qu'ils changent (et non à la fermeture : un plantage ou
        // un Ctrl+C ne doit rien faire perdre).
        if let Some(store) = &tabs_store {
            let snapshot = app.snapshot();
            if last_saved_tabs.as_ref() != Some(&snapshot) {
                match store.save(&snapshot) {
                    Ok(()) => last_saved_tabs = Some(snapshot),
                    Err(e) => app.error = Some(format!("onglets non sauvegardés : {e}")),
                }
            }
        }
    }
    Ok(())
}
