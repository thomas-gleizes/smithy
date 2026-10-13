mod app;
mod config;
mod logview;
mod session;
mod ui;
mod wrap;

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use smithy_core::{Error, Pipeline, Provider, Repo};
use smithy_forgejo::Forgejo;
use tokio::sync::mpsc::{self, UnboundedSender};

use app::{App, Msg};
use config::{Server, ServerKind};
use session::{Session, SessionStore};

const REFRESH: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::load()?;
    let server = cfg
        .server(std::env::var("SMITHY_SERVER").ok().as_deref())?
        .clone();
    let store = SessionStore::default_location();

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, server, store).await;
    ratatui::restore();
    result
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

fn fetch_repos<P: Provider + 'static>(p: &Arc<P>, tx: &UnboundedSender<Msg>) {
    let (p, tx) = (p.clone(), tx.clone());
    tokio::spawn(async move {
        let r = p.list_repos().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Repos(r));
    });
}

fn fetch_pipelines<P: Provider + 'static>(p: &Arc<P>, tx: &UnboundedSender<Msg>, app: &App) {
    let Some(repo) = app.selected_repo().cloned() else {
        return;
    };
    let (p, tx) = (p.clone(), tx.clone());
    tokio::spawn(async move {
        let result = p.list_pipelines(&repo).await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Pipelines { repo, result });
    });
}

fn fetch_logs<P: Provider + 'static>(
    p: &Arc<P>,
    tx: &UnboundedSender<Msg>,
    (repo, pipeline): (Repo, Pipeline),
) {
    let (p, tx) = (p.clone(), tx.clone());
    tokio::spawn(async move {
        let result = p.job_log(&repo, &pipeline).await;
        let _ = tx.send(Msg::Logs {
            repo,
            pipeline_id: pipeline.id,
            result,
        });
    });
}

/// Un seul mot de passe, jamais conservé : il donne le cookie de session et, au premier
/// login seulement, un token. Si `known_token` existe déjà, on ne fait que renouveler le
/// cookie : sinon chaque expiration de session laisserait un token de plus sur le compte.
fn spawn_login(
    server: &Server,
    tx: &UnboundedSender<Msg>,
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
        let _ = tx.send(Msg::LoggedIn(result));
    });
}

async fn run(
    terminal: &mut DefaultTerminal,
    server: Server,
    store: Option<SessionStore>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    app.server_label = format!("{} ({})", server.name, server.url);

    // Le token de la config prime ; sinon celui créé lors d'un login précédent.
    // Les cookies, eux, viennent toujours de la session sauvegardée.
    let saved = store
        .as_ref()
        .and_then(|s| s.load(&server.name, &server.url).ok().flatten());
    let mut username = saved.as_ref().map_or(String::new(), |s| s.username.clone());
    let mut saved_token = saved.as_ref().and_then(|s| s.token.clone());
    let token = server.token.clone().or_else(|| saved_token.clone());
    let cookies = saved.map(|s| s.cookies).unwrap_or_default();
    let mut provider = token.map(|t| build_provider(&server, t, cookies));
    match &provider {
        Some(p) => {
            app.logs_supported = p.capabilities().logs;
            fetch_repos(p, &tx);
        }
        None => app.open_login(&username),
    }
    let mut last_refresh = Instant::now();

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        let mut need_pipelines = false;
        let mut need_repos = false;
        let mut need_logs = None;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                    if ctrl && k.code == KeyCode::Char('c') {
                        app.should_quit = true;
                    } else if app.login.is_some() {
                        match k.code {
                            KeyCode::Esc if provider.is_none() => app.should_quit = true,
                            KeyCode::Esc => app.login = None,
                            KeyCode::Enter => {
                                if let Some((user, pass)) = app.login_enter() {
                                    // Un token de la config suffit aussi : pas besoin d'en créer.
                                    let known =
                                        server.token.clone().or_else(|| saved_token.clone());
                                    spawn_login(&server, &tx, user, pass, known);
                                }
                            }
                            KeyCode::Tab | KeyCode::Up | KeyCode::Down => app.login_toggle_field(),
                            KeyCode::Backspace => app.login_backspace(),
                            KeyCode::Char(c) if !ctrl => app.login_type(c),
                            _ => {}
                        }
                    } else if app.logs.is_some() {
                        match k.code {
                            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Backspace => {
                                app.close_logs()
                            }
                            KeyCode::Char('j') | KeyCode::Down => app.logs_scroll(1),
                            KeyCode::Char('k') | KeyCode::Up => app.logs_scroll(-1),
                            KeyCode::PageDown | KeyCode::Char(' ') => app.logs_page(1),
                            KeyCode::PageUp | KeyCode::Char('b') => app.logs_page(-1),
                            KeyCode::Char('g') | KeyCode::Home => app.logs_top(),
                            KeyCode::Char('G') | KeyCode::End => app.logs_bottom(),
                            KeyCode::Char('w') => app.logs_toggle_wrap(),
                            KeyCode::Char('r') => need_logs = app.refetch_logs(),
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Char('j') | KeyCode::Down => need_pipelines = app.step(1),
                            KeyCode::Char('k') | KeyCode::Up => need_pipelines = app.step(-1),
                            KeyCode::Tab => app.toggle_focus(),
                            KeyCode::Enter => need_logs = app.open_logs(),
                            KeyCode::Char('r') => {
                                need_repos = true;
                                need_pipelines = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        while let Ok(msg) = rx.try_recv() {
            let mut save_error = None;
            match &msg {
                Msg::LoggedIn(Ok(session)) => {
                    username.clone_from(&session.username);
                    saved_token.clone_from(&session.token);
                    // Le token de la config prime ; les cookies viennent de ce login.
                    if let Some(token) = server.token.clone().or_else(|| session.token.clone()) {
                        let p = build_provider(&server, token, session.cookies.clone());
                        app.logs_supported = p.capabilities().logs;
                        provider = Some(p);
                        need_repos = true;
                        // Des logs étaient demandés quand la session a expiré : on les relance.
                        need_logs = app.refetch_logs();
                    }
                    if let Some(Err(e)) = store.as_ref().map(|s| s.save(&server.name, session)) {
                        save_error = Some(format!("session non sauvegardée : {e}"));
                    }
                }
                Msg::Logs {
                    result: Err(Error::SessionExpired),
                    ..
                } if app.login.is_none() => app.open_login(&username),
                _ => {}
            }
            need_pipelines |= app.apply(msg);
            // Après `apply`, qui efface le bandeau d'erreur.
            if save_error.is_some() {
                app.error = save_error;
            }
        }

        if last_refresh.elapsed() >= REFRESH {
            need_pipelines = true;
            if need_logs.is_none() && app.logs_are_live() {
                need_logs = app.refetch_logs();
            }
            last_refresh = Instant::now();
        }
        if let Some(p) = &provider {
            if need_repos {
                fetch_repos(p, &tx);
            }
            if need_pipelines {
                fetch_pipelines(p, &tx, &app);
            }
            if let Some(req) = need_logs {
                fetch_logs(p, &tx, req);
            }
        }
    }
    Ok(())
}
