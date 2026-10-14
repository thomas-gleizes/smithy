mod app;
mod browser;
mod config;
mod logview;
mod pages;
mod recent;
mod request;
mod session;
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
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use smithy_core::{Error, Provider};
use smithy_forgejo::Forgejo;
use tokio::sync::mpsc::{self, UnboundedSender};

use app::{App, Msg};
use browser::{Browser, FILE_LIMIT_BYTES, Screen};
use config::{Server, ServerKind};
use recent::RecentStore;
use request::Request;
use session::{Session, SessionStore};

const REFRESH: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::load()?;
    let wanted = std::env::var("SMITHY_SERVER").ok();
    // Vérifie le nom demandé (erreur claire) avant d'ouvrir le terminal.
    cfg.server(wanted.as_deref())?;
    let fresh = parse_args()?;
    let store = SessionStore::default_location();

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, cfg.servers.clone(), wanted, store, fresh).await;
    ratatui::restore();
    result
}

const USAGE: &str = "smithy : consulter la CI et le code de vos forges git dans le terminal

Usage : smithy [--fresh]

  --fresh   ne rouvre pas les onglets de la dernière session (démarre sur l'accueil)
  --help    affiche ce message

Configuration : voir le README (serveurs dans ~/.config/smithy/config.toml).";

/// Les arguments : `--fresh` seul. Renvoie `true` s'il faut démarrer sans restaurer les onglets.
fn parse_args() -> Result<bool> {
    let mut fresh = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--fresh" => fresh = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => anyhow::bail!("argument inconnu : {other}\n\n{USAGE}"),
        }
    }
    Ok(fresh)
}

fn build_provider(
    server: &Server,
    token: String,
    cookies: BTreeMap<String, String>,
) -> Arc<Forgejo> {
    match server.kind {
        ServerKind::Forgejo => {
            Arc::new(Forgejo::new(server.url.clone(), token).with_cookies(cookies))
        }
    }
}

/// Touches des onglets, valables sur toutes les pages. Renvoie `true` si la touche est prise.
fn tab_key(app: &mut App, code: KeyCode) -> bool {
    // En pleine saisie du filtre, `t`, `x` et les autres sont du texte.
    if app.home().is_some_and(|h| h.editing) {
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
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Backspace => {
                if let Some(b) = app.browser_mut() {
                    b.up();
                }
            }
            KeyCode::Char('j') | KeyCode::Down => app.text_scroll(1),
            KeyCode::Char('k') | KeyCode::Up => app.text_scroll(-1),
            KeyCode::PageDown | KeyCode::Char('f') => app.text_page(1),
            KeyCode::PageUp | KeyCode::Char('b') => app.text_page(-1),
            KeyCode::Char('g') | KeyCode::Home => app.text_top(),
            KeyCode::Char('G') | KeyCode::End => app.text_bottom(),
            KeyCode::Char('w') => app.text_toggle_wrap(),
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
        Screen::Dir => match code {
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
                Msg::File {
                    repo,
                    reference,
                    path,
                    result,
                }
            }
        };
        let _ = tx.send((server, msg));
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
        }
        .map(|l| Session {
            url: server.url.clone(),
            username: l.username,
            token: l.token.or(known_token),
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
    provider: Option<Arc<Forgejo>>,
    username: String,
    saved_token: Option<String>,
}

impl Backend {
    /// Le token de la config prime ; sinon celui créé lors d'un login précédent. Les cookies,
    /// eux, viennent toujours de la session sauvegardée.
    fn load(server: Server, store: Option<&SessionStore>) -> Self {
        let saved = store.and_then(|s| s.load(&server.name, &server.url).ok().flatten());
        let username = saved.as_ref().map_or(String::new(), |s| s.username.clone());
        let saved_token = saved.as_ref().and_then(|s| s.token.clone());
        let token = server.token.clone().or_else(|| saved_token.clone());
        let cookies = saved.map(|s| s.cookies).unwrap_or_default();
        let provider = token.map(|t| build_provider(&server, t, cookies));
        Self {
            server,
            provider,
            username,
            saved_token,
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
        kind: ServerKind::Forgejo,
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
    // L'onglet affiché est sur ce serveur et il n'est plus connecté (URL changée) : login.
    if server.name == app.server_name && !app.is_connected(&server.name) && app.login.is_none() {
        app.open_login(&server.name, &backends[&server.name].username, false);
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
    Ok(())
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
    app.set_servers(
        servers
            .iter()
            .map(|s| app::ServerEntry {
                name: s.name.clone(),
                url: s.url.clone(),
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
    if backends[&initial].provider.is_none() {
        app.open_login(&initial, &backends[&initial].username, false);
    }
    let mut last_refresh = Instant::now();

    let _lock = owner_lock;
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(TICK)?
            && let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl && k.code == KeyCode::Char('c') {
                app.should_quit = true;
            } else if let Some(form) = app.login.as_ref().map(|f| f.server.clone()) {
                match k.code {
                    KeyCode::Esc => app.should_quit = app.login_escape(),
                    KeyCode::Enter => {
                        if let Some((user, pass)) = app.login_enter() {
                            let b = &backends[&form];
                            spawn_login(&b.server, &tx, user, pass, b.known_token());
                        }
                    }
                    KeyCode::Tab | KeyCode::Up | KeyCode::Down => app.login_toggle_field(),
                    KeyCode::Backspace => app.login_backspace(),
                    KeyCode::Char(c) if !ctrl => app.login_type(c),
                    _ => {}
                }
            } else if app.server_form.is_some() {
                match k.code {
                    KeyCode::Esc => app.server_form_cancel(),
                    KeyCode::Enter => {
                        if let Some(draft) = app.server_form_enter()
                            && let Err(e) =
                                save_server(&mut app, &mut backends, store.as_ref(), draft)
                        {
                            app.server_form_fail(e);
                        }
                    }
                    KeyCode::Tab | KeyCode::Down => app.server_form_move(1),
                    KeyCode::BackTab | KeyCode::Up => app.server_form_move(-1),
                    KeyCode::Backspace => app.server_form_backspace(),
                    KeyCode::Char(c) if !ctrl => app.server_form_type(c),
                    _ => {}
                }
            } else if app.server_picker.is_some() {
                if app.server_delete.is_some() {
                    match k.code {
                        KeyCode::Char('y' | 'o') => {
                            if let Some(name) = app.server_delete_confirm()
                                && let Err(e) =
                                    delete_server(&mut app, &mut backends, store.as_ref(), &name)
                            {
                                app.server_note = Some(e);
                            }
                        }
                        _ => app.server_delete_cancel(),
                    }
                } else {
                    match k.code {
                        KeyCode::Esc | KeyCode::Char('q' | 'S') => app.close_server_picker(),
                        KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                            app.server_picker_move(1)
                        }
                        KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                            app.server_picker_move(-1)
                        }
                        KeyCode::Char('a') => app.server_form_add(),
                        KeyCode::Char('e') => app.server_form_edit(),
                        KeyCode::Char('d') => app.server_delete_ask(),
                        KeyCode::Enter => {
                            // Pas encore connecté : le login d'abord, la bascule ensuite.
                            if let Some(name) = app.server_picker_choose() {
                                app.open_login(&name, &backends[&name].username, true);
                            }
                        }
                        _ => {}
                    }
                }
            } else if app.tab_picker.is_some() {
                tab_picker_key(&mut app, k.code);
            } else if k.code == KeyCode::Char('S') && !app.home().is_some_and(|h| h.editing) {
                app.open_server_picker();
            } else if tab_key(&mut app, k.code) {
                // pris par les onglets
            } else if app.home().is_some() {
                home_key(&mut app, k.code);
            } else if app.repo_page().is_some() {
                list_key(&mut app, k.code, App::repo_enter);
            } else if app.runs().is_some() {
                list_key(&mut app, k.code, App::open_logs);
            } else if app.logs().is_some() {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => app.close_logs(),
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
                browser_key(&mut app, k.code);
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
                        // Le token de la config prime ; les cookies viennent de ce login.
                        if let Some(token) = b.known_token() {
                            b.provider =
                                Some(build_provider(&b.server, token, session.cookies.clone()));
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
                } if app.login.is_none() && origin == app.server_name => {
                    let username = backends.get(&origin).map_or("", |b| b.username.as_str());
                    app.open_login(&origin, username, false);
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

        if last_refresh.elapsed() >= REFRESH {
            app.tick();
            last_refresh = Instant::now();
        }
        // Sondes de joignabilité demandées par la liste des serveurs.
        for (name, url) in app.take_probes() {
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = smithy_forgejo::ping(&url).await;
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
