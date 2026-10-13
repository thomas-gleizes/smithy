mod app;
mod config;
mod session;
mod ui;

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use smithy_core::Provider;
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

fn build_provider(server: &Server, token: String) -> Arc<Forgejo> {
    match server.kind {
        ServerKind::Forgejo => Arc::new(Forgejo::new(server.url.clone(), token)),
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

/// Un seul mot de passe, jamais conservé : il donne le cookie de session et un token.
fn spawn_login(server: &Server, tx: &UnboundedSender<Msg>, username: String, password: String) {
    let (server, tx) = (server.clone(), tx.clone());
    tokio::spawn(async move {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Nom reconnaissable dans les paramètres du compte, pour pouvoir le révoquer.
        let token_name = format!("smithy-{}-{secs}", server.name);
        let result = match server.kind {
            ServerKind::Forgejo => {
                smithy_forgejo::login(&server.url, &username, &password, &token_name).await
            }
        }
        .map(|l| Session {
            url: server.url.clone(),
            username: l.username,
            token: Some(l.token),
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
    let saved = store
        .as_ref()
        .and_then(|s| s.load(&server.name, &server.url).ok().flatten());
    let token = server
        .token
        .clone()
        .or_else(|| saved.as_ref().and_then(|s| s.token.clone()));
    let mut provider = token.map(|t| build_provider(&server, t));
    match &provider {
        Some(p) => fetch_repos(p, &tx),
        None => app.open_login(saved.as_ref().map_or("", |s| s.username.as_str())),
    }
    let mut last_refresh = Instant::now();

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        let mut need_pipelines = false;
        let mut need_repos = false;

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
                                    spawn_login(&server, &tx, user, pass);
                                }
                            }
                            KeyCode::Tab | KeyCode::Up | KeyCode::Down => app.login_toggle_field(),
                            KeyCode::Backspace => app.login_backspace(),
                            KeyCode::Char(c) if !ctrl => app.login_type(c),
                            _ => {}
                        }
                    } else {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                            KeyCode::Char('j') | KeyCode::Down => need_pipelines = app.step(1),
                            KeyCode::Char('k') | KeyCode::Up => need_pipelines = app.step(-1),
                            KeyCode::Tab => app.toggle_focus(),
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
            if let Msg::LoggedIn(Ok(session)) = &msg {
                if let Some(token) = session.token.clone() {
                    provider = Some(build_provider(&server, token));
                    need_repos = true;
                }
                if let Some(Err(e)) = store.as_ref().map(|s| s.save(&server.name, session)) {
                    save_error = Some(format!("session non sauvegardée : {e}"));
                }
            }
            need_pipelines |= app.apply(msg);
            // Après `apply`, qui efface le bandeau d'erreur.
            if save_error.is_some() {
                app.error = save_error;
            }
        }

        if last_refresh.elapsed() >= REFRESH {
            need_pipelines = true;
            last_refresh = Instant::now();
        }
        if let Some(p) = &provider {
            if need_repos {
                fetch_repos(p, &tx);
            }
            if need_pipelines {
                fetch_pipelines(p, &tx, &app);
            }
        }
    }
    Ok(())
}
