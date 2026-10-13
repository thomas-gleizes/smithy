use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    },
};
use smithy_core::Status;

use crate::app::{App, Focus, LineKind, LogState, LogView, LoginField, LoginForm};

pub fn status_style(s: Status) -> (&'static str, Color) {
    match s {
        Status::Success => ("✔ success", Color::Green),
        Status::Failure => ("✘ failure", Color::Red),
        Status::Running => ("● running", Color::Yellow),
        Status::Pending => ("◌ pending", Color::Cyan),
        Status::Cancelled => ("⊘ cancelled", Color::DarkGray),
        Status::Skipped => ("– skipped", Color::DarkGray),
        Status::Unknown => ("? unknown", Color::Magenta),
    }
}

fn border(focused: bool) -> Style {
    if focused {
        Style::new().fg(Color::Yellow)
    } else {
        Style::new()
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    if let Some(view) = &app.logs {
        draw_logs(f, app, view);
    } else {
        draw_lists(f, app);
    }
    if let Some(form) = &app.login {
        draw_login(f, form, &app.server_label);
    }
}

fn draw_logs(f: &mut Frame, app: &App, view: &LogView) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    let (status, color) = status_style(view.pipeline.status);
    let title = Line::from(vec![
        Span::raw(format!(
            " Logs · {} · {} · {} · ",
            view.repo.full_name(),
            view.pipeline.workflow,
            view.pipeline.job
        )),
        Span::styled(format!("{status} "), Style::new().fg(color)),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rows[0]);
    app.viewport.set(inner.height as usize);

    let mut position = String::new();
    let body: Vec<Line> = match &view.state {
        LogState::Loading => vec![Line::from(Span::styled(
            "chargement…",
            Style::new().fg(Color::Cyan),
        ))],
        LogState::Failed(e) => vec![Line::from(Span::styled(
            format!("erreur : {e}"),
            Style::new().fg(Color::Red),
        ))],
        LogState::Ready(lines) => {
            let end = (view.scroll + inner.height as usize).min(lines.len());
            position = format!(" {}-{}/{} ", (view.scroll + 1).min(end), end, lines.len());
            lines[view.scroll.min(lines.len())..end]
                .iter()
                .map(|l| match l.kind {
                    LineKind::Step(st) => Line::from(Span::styled(
                        format!("▸ {}", l.text),
                        Style::new().fg(status_style(st).1).bold(),
                    )),
                    LineKind::Text => Line::from(l.text.as_str()),
                })
                .collect()
        }
    };
    // La position va dans le cadre, à droite : le pied de page peut être tronqué
    // sur un terminal étroit, pas le cadre.
    let mut block = block
        .title(title)
        .title(Line::from(position).right_aligned());
    if let Some(notice) = &view.notice {
        // En bas du cadre, en jaune : visible sans prendre la place des logs.
        block = block.title_bottom(Line::from(Span::styled(
            format!(" ⚠ {notice} "),
            Style::new().fg(Color::Yellow),
        )));
    }
    f.render_widget(Paragraph::new(body).block(block), rows[0]);

    let footer = match &app.error {
        Some(e) => Line::from(Span::styled(
            format!("erreur : {e}"),
            Style::new().fg(Color::Red),
        )),
        None => Line::from(Span::styled(
            "j/k ↑/↓ défiler · PgUp/PgDn page · g/G début/fin · r rafraîchir · Esc retour",
            Style::new().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(footer), rows[1]);
}

fn draw_lists(f: &mut Frame, app: &App) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(f.area());
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(rows[0]);

    let items: Vec<ListItem> = app
        .repos
        .iter()
        .map(|r| ListItem::new(r.full_name()))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Dépôts ")
                .border_style(border(app.focus == Focus::Repos)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    if !app.repos.is_empty() {
        ls.select(Some(app.repo_idx));
    }
    f.render_stateful_widget(list, cols[0], &mut ls);

    let title = match app.selected_repo() {
        Some(r) => format!(" CI · {} ", r.full_name()),
        None => " CI ".to_owned(),
    };
    let table_rows: Vec<Row> = app
        .pipelines
        .iter()
        .map(|p| {
            let (label, color) = status_style(p.status);
            Row::new(vec![
                Cell::from(Span::styled(label, Style::new().fg(color))),
                Cell::from(p.workflow.clone()),
                Cell::from(p.branch.clone()),
                Cell::from(p.commit.chars().take(7).collect::<String>()),
                Cell::from(p.title.clone()),
            ])
        })
        .collect();
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(13),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(
        Row::new(["Statut", "Workflow", "Branche", "Commit", "Titre"]).style(Style::new().bold()),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border(app.focus == Focus::Pipelines)),
    )
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ts = TableState::default();
    if !app.pipelines.is_empty() {
        ts.select(Some(app.pipe_idx));
    }
    f.render_stateful_widget(table, cols[1], &mut ts);

    let second = if let Some(err) = &app.error {
        Line::from(Span::styled(
            format!("erreur : {err}"),
            Style::new().fg(Color::Red),
        ))
    } else if let Some(url) = app.selected_pipeline().and_then(|p| p.url.as_deref()) {
        Line::from(format!("run : {url}"))
    } else {
        Line::from("")
    };
    let help = Line::from(Span::styled(
        "j/k ↑/↓ naviguer · ↵ logs · tab changer de panneau · r rafraîchir · q quitter",
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(vec![help, second]), rows[1]);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

fn draw_login(f: &mut Frame, form: &LoginForm, server: &str) {
    let area = centered(f.area(), 64, 9);
    f.render_widget(Clear, area);

    let field = |label: &str, value: String, active: bool| {
        let cursor = if active && !form.busy { "▏" } else { "" };
        let style = if active {
            Style::new().fg(Color::Yellow)
        } else {
            Style::new()
        };
        Line::from(vec![
            Span::styled(format!("{label:<13}"), style),
            Span::raw(format!("{value}{cursor}")),
        ])
    };
    let mut lines = vec![
        field(
            "Identifiant",
            form.username.clone(),
            form.field == LoginField::Username,
        ),
        field(
            "Mot de passe",
            "•".repeat(form.password.chars().count()),
            form.field == LoginField::Password,
        ),
        Line::from(""),
    ];
    lines.push(if form.busy {
        Line::from(Span::styled("connexion…", Style::new().fg(Color::Cyan)))
    } else if let Some(e) = &form.error {
        Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red)))
    } else {
        Line::from("")
    });
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Entrée valider · Tab changer de champ · Esc quitter",
        Style::new().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Connexion · {server} "))
        .border_style(Style::new().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::{LineKind, LogState};
    use ratatui::{Terminal, backend::TestBackend};
    use smithy_core::{Pipeline, Repo};

    #[test]
    fn rendu_complet_sans_panique_et_contenu_visible() {
        let repo = Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        };
        let mut app = App::new();
        app.apply(Msg::Repos(Ok(vec![repo.clone()])));
        app.apply(Msg::Pipelines {
            repo,
            result: Ok(vec![Pipeline {
                id: 1,
                number: 2,
                job: "build".into(),
                workflow: "fail.yml".into(),
                title: "ci: workflow rouge".into(),
                branch: "main".into(),
                commit: "2f9ec647244c".into(),
                status: Status::Failure,
                url: Some("http://x/runs/2".into()),
            }]),
        });

        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        for needle in [
            "smithy/demo",
            "fail.yml",
            "failure",
            "2f9ec64",
            "http://x/runs/2",
        ] {
            assert!(text.contains(needle), "absent du rendu : {needle}");
        }
    }

    #[test]
    fn le_pied_de_page_suit_le_run_selectionne() {
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let pipe = |id: u64| Pipeline {
            id,
            number: id,
            job: "build".into(),
            workflow: "ci.yml".into(),
            title: "t".into(),
            branch: "main".into(),
            commit: "abc".into(),
            status: Status::Success,
            url: Some(format!("http://x/runs/{id}")),
        };
        let mut app = App::new();
        app.apply(Msg::Repos(Ok(vec![repo.clone()])));
        app.apply(Msg::Pipelines {
            repo,
            result: Ok(vec![pipe(3), pipe(2)]),
        });
        app.toggle_focus();
        app.step(1);

        let mut term = Terminal::new(TestBackend::new(100, 12)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("run : http://x/runs/2"));
        assert!(!text.contains("run : http://x/runs/3"));
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn app_with_logs(state: LogState) -> App {
        let repo = Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        };
        let mut app = App::new();
        app.logs_supported = true;
        app.apply(Msg::Repos(Ok(vec![repo.clone()])));
        app.apply(Msg::Pipelines {
            repo: repo.clone(),
            result: Ok(vec![Pipeline {
                id: 1,
                number: 3,
                job: "test".into(),
                workflow: "fail.yml".into(),
                title: "t".into(),
                branch: "main".into(),
                commit: "abc".into(),
                status: Status::Failure,
                url: None,
            }]),
        });
        app.focus = Focus::Pipelines;
        app.open_logs();
        app.logs.as_mut().unwrap().state = state;
        app
    }

    fn line(kind: LineKind, text: &str) -> crate::app::LogLine {
        crate::app::LogLine {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn vue_des_logs() {
        let lines = vec![
            line(LineKind::Step(Status::Failure), "cargo test"),
            line(LineKind::Text, "boum"),
            line(LineKind::Text, "autre ligne"),
        ];
        let app = app_with_logs(LogState::Ready(lines));
        let text = render(&app, 100, 12);
        for needle in [
            "smithy/demo",
            "fail.yml",
            "test",
            "▸ cargo test",
            "boum",
            "1-3/3",
            "Esc retour",
        ] {
            assert!(text.contains(needle), "absent : {needle}");
        }
        assert!(!text.contains("Dépôts"), "la liste ne doit plus s'afficher");
    }

    #[test]
    fn vue_des_logs_defilee_et_hauteur_publiee() {
        let lines: Vec<_> = (1..=50)
            .map(|i| line(LineKind::Text, &format!("ligne-{i:02}")))
            .collect();
        let mut app = app_with_logs(LogState::Ready(lines));
        let _ = render(&app, 80, 12); // le rendu publie la hauteur utile (12 - 2 bordures - 1 pied)
        assert_eq!(app.viewport.get(), 9);
        app.logs_scroll(20);
        let text = render(&app, 80, 12);
        assert!(text.contains("ligne-21") && text.contains("ligne-29"));
        assert!(!text.contains("ligne-20") && !text.contains("ligne-30"));
        assert!(text.contains("21-29/50"));
    }

    #[test]
    fn vue_des_logs_chargement_et_erreur() {
        let loading = app_with_logs(LogState::Loading);
        assert!(render(&loading, 80, 10).contains("chargement"));
        let failed = app_with_logs(LogState::Failed("session expirée".into()));
        assert!(render(&failed, 80, 10).contains("erreur : session expirée"));
        // Très petit terminal, ligne plus large que l'écran : pas de panique
        let long = app_with_logs(LogState::Ready(vec![line(
            LineKind::Text,
            &"x".repeat(500),
        )]));
        render(&long, 12, 4);
        render(&long, 1, 1);
    }

    #[test]
    fn reserve_de_version_affichee() {
        let mut app = app_with_logs(LogState::Ready(vec![line(LineKind::Text, "ok")]));
        app.logs.as_mut().unwrap().notice = Some("Forgejo 14.0.0 : version non vérifiée".into());
        let text = render(&app, 100, 10);
        assert!(text.contains("⚠") && text.contains("non vérifiée"));
        assert!(!render(&app_with_logs(LogState::Loading), 100, 10).contains("⚠"));
    }

    #[test]
    fn formulaire_par_dessus_les_logs() {
        let mut app = app_with_logs(LogState::Failed("session expirée".into()));
        app.open_login("thomas");
        let text = render(&app, 100, 20);
        assert!(text.contains("Connexion") && text.contains("Logs"));
    }

    #[test]
    fn formulaire_de_connexion_masque_le_mot_de_passe() {
        let mut app = App::new();
        app.server_label = "maison (http://forge)".into();
        app.open_login("thomas");
        "hunter2".chars().for_each(|c| app.login_type(c));
        let text = render(&app, 100, 20);
        assert!(text.contains("Connexion · maison"));
        assert!(text.contains("thomas"));
        assert!(text.contains("•••••••"));
        assert!(!text.contains("hunter2"), "mot de passe affiché en clair");

        app.login.as_mut().unwrap().error = Some("identifiant ou mot de passe incorrect".into());
        assert!(render(&app, 100, 20).contains("incorrect"));
    }

    #[test]
    fn formulaire_sur_tres_petit_terminal() {
        let mut app = App::new();
        app.open_login("");
        render(&app, 10, 3);
    }

    #[test]
    fn rendu_sur_petit_terminal_et_etat_vide() {
        let mut term = Terminal::new(TestBackend::new(20, 4)).unwrap();
        term.draw(|f| draw(f, &App::new())).unwrap();
    }
}
