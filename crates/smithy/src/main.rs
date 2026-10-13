mod app;
mod config;
// Branché à l'étape suivante (login par session) ; testé en attendant.
#[allow(dead_code)]
mod session;
mod ui;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use smithy_core::Provider;
use smithy_forgejo::Forgejo;
use tokio::sync::mpsc::{self, UnboundedSender};

use app::{App, Msg};

const REFRESH: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(100);

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::load()?;
    let server = cfg.server(std::env::var("SMITHY_SERVER").ok().as_deref())?;
    let token = server.token.clone().with_context(|| {
        format!(
            "serveur « {} » : `token` manquant (le login par session arrive bientôt)",
            server.name
        )
    })?;
    let provider = Arc::new(match server.kind {
        config::ServerKind::Forgejo => Forgejo::new(server.url.clone(), token),
    });

    let mut terminal = ratatui::init();
    let result = run(&mut terminal, provider).await;
    ratatui::restore();
    result
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

async fn run<P: Provider + 'static>(
    terminal: &mut DefaultTerminal,
    provider: Arc<P>,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new();
    let mut last_refresh = Instant::now();
    fetch_repos(&provider, &tx);

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        let mut need_pipelines = false;
        let mut need_repos = false;

        if event::poll(TICK)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.should_quit = true
                        }
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

        while let Ok(msg) = rx.try_recv() {
            need_pipelines |= app.apply(msg);
        }

        if last_refresh.elapsed() >= REFRESH {
            need_pipelines = true;
            last_refresh = Instant::now();
        }
        if need_repos {
            fetch_repos(&provider, &tx);
        }
        if need_pipelines {
            fetch_pipelines(&provider, &tx, &app);
        }
    }
    Ok(())
}
