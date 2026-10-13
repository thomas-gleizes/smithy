use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    },
};
use smithy_core::{EntryKind, Status};

use crate::{
    app::{App, Focus, LoginField, LoginForm},
    browser::{Browser, DirState, Mode, fmt_size},
    logview::{LineKind, LogState, LogView, TextView},
};

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
    } else if let Some(b) = &app.browser {
        draw_browser(f, app, b);
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
    draw_text_pane(f, app, rows[0], title, view);
    draw_footer(f, app, rows[1], logs_help(f.area().width as usize), None);
}

/// Volet de texte plein cadre : sortie d'un job ou contenu d'un fichier, avec sa position et sa
/// réserve éventuelle dans la bordure du bas.
fn draw_text_pane(f: &mut Frame, app: &App, area: Rect, title: Line<'static>, view: &TextView) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(area);
    let (width, height) = (inner.width as usize, inner.height as usize);
    // Le rendu seul connaît la taille utile : il la publie pour le défilement.
    app.viewport.set(height);
    app.viewport_width.set(width);

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
        LogState::Ready(_) => {
            let total = view.total_rows(width);
            let end = (view.scroll + height).min(total);
            position = format!(
                " {}{}-{}/{} ",
                if view.wrap { "↩ " } else { "" },
                (view.scroll + 1).min(end),
                end,
                total
            );
            view.window(width, height)
                .into_iter()
                .map(|r| match r.kind {
                    LineKind::Step(st) => {
                        let style = Style::new().fg(status_style(st).1).bold();
                        // La section courante (celle que Espace plie) est en vidéo inverse.
                        let style = if r.current {
                            style.add_modifier(Modifier::REVERSED)
                        } else {
                            style
                        };
                        Line::from(Span::styled(r.text, style))
                    }
                    LineKind::Text => Line::from(r.text),
                })
                .collect()
        }
    };
    // Position et état du repli dans la bordure du bas, à droite : le long titre du haut les
    // recouvrirait sur un terminal étroit, et le pied de page peut être tronqué ; la bordure,
    // non. La réserve éventuelle (⚠) est à gauche, sur la même bordure.
    let mut block = block
        .title(title)
        .title_bottom(Line::from(position).right_aligned());
    if let Some(notice) = &view.notice {
        block = block.title_bottom(Line::from(Span::styled(
            format!(" ⚠ {notice} "),
            Style::new().fg(Color::Yellow),
        )));
    }
    f.render_widget(Paragraph::new(body).block(block), area);
}

/// Pied de page : l'erreur en rouge, sinon un message d'information en jaune, sinon l'aide.
fn draw_footer(f: &mut Frame, app: &App, area: Rect, help: &str, info: Option<&str>) {
    let footer = match (&app.error, info) {
        (Some(e), _) => Line::from(Span::styled(
            format!("erreur : {e}"),
            Style::new().fg(Color::Red),
        )),
        (None, Some(i)) => Line::from(Span::styled(i.to_owned(), Style::new().fg(Color::Yellow))),
        (None, None) => Line::from(Span::styled(
            help.to_owned(),
            Style::new().fg(Color::DarkGray),
        )),
    };
    f.render_widget(Paragraph::new(footer), area);
}

/// La première aide qui tient dans `width` colonnes ; tronquée, elle ferait disparaître
/// justement les touches les moins connues. `variants` va de la plus complète à la plus courte.
fn pick_help<'a>(width: usize, variants: &[&'a str]) -> &'a str {
    variants
        .iter()
        .copied()
        .find(|h| h.chars().count() <= width)
        // Rien ne tient : la plus courte, qui indique au moins comment sortir.
        .or(variants.last().copied())
        .unwrap_or("")
}

/// Aide de la liste des dépôts et des runs.
fn lists_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ↵ logs · e explorer le code · tab changer de panneau · r rafraîchir · q quitter",
            "j/k naviguer · ↵ logs · e code · tab panneau · r rafraîchir · q quitter",
            "↵ logs · e code · tab · r · q quitter",
            "↵ · e · tab · q",
            "q",
        ],
    )
}

fn logs_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · ←/→ section · Espace plier · a tout · w retour à la ligne · r rafraîchir · Esc retour",
            "j/k défiler · ←/→ section · Espace plier · a tout · w repli · Esc retour",
            "←/→ · Espace plier · w repli · Esc retour",
            "←/→ · Espace · w · Esc",
            "Esc",
        ],
    )
}

fn dir_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · →/↵ ouvrir · ←/⌫ remonter · B branche · r actualiser · Esc retour",
            "j/k · →/↵ ouvrir · ← remonter · B branche · Esc retour",
            "→ ouvrir · ← haut · B branche · Esc",
            "→ · ← · B · Esc",
            "Esc",
        ],
    )
}

fn file_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · w retour à la ligne · r actualiser · Esc/← retour",
            "j/k défiler · g/G · w retour à la ligne · Esc retour",
            "j/k · w repli · Esc retour",
            "w · Esc",
            "Esc",
        ],
    )
}

fn branches_help(width: usize) -> &'static str {
    pick_help(
        width,
        &["j/k choisir · ↵ ouvrir · Esc annuler", "↵ · Esc", "Esc"],
    )
}

/// Taille d'un octet à l'écran : `Nom` à gauche, `Taille` à droite.
fn draw_browser(f: &mut Frame, app: &App, b: &Browser) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(f.area());
    let width = f.area().width as usize;
    let reference = if b.reference.is_empty() {
        "…"
    } else {
        &b.reference
    };
    if let Mode::File(fv) = &b.mode {
        let title = Line::from(format!(
            " Explorateur · {} @ {} · /{} ",
            b.repo.full_name(),
            reference,
            fv.path
        ));
        draw_text_pane(f, app, rows[0], title, &fv.text);
        draw_footer(f, app, rows[1], file_help(width), None);
        return;
    }
    let title = format!(
        " Explorateur · {} @ {} · /{} ",
        b.repo.full_name(),
        reference,
        b.path
    );
    draw_dir(f, app, b, rows[0], title);
    let help = if matches!(b.mode, Mode::Branches { .. }) {
        branches_help(width)
    } else {
        dir_help(width)
    };
    draw_footer(f, app, rows[1], help, b.info.as_deref());
    if let Mode::Branches { selected } = b.mode {
        draw_branch_picker(f, b, selected);
    }
}

fn draw_dir(f: &mut Frame, app: &App, b: &Browser, area: Rect, title: String) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(area);
    // Une ligne d'en-tête : la hauteur utile pour la sélection en est réduite d'autant.
    app.viewport.set((inner.height as usize).saturating_sub(1));
    match &b.dir {
        DirState::Loading => draw_message(f, area, block, "chargement…".to_owned(), Color::Cyan),
        DirState::Failed(e) => draw_message(f, area, block, format!("erreur : {e}"), Color::Red),
        DirState::Ready(entries) if entries.is_empty() => draw_message(
            f,
            area,
            block,
            if b.empty_repo {
                "(dépôt vide : aucune branche)".to_owned()
            } else {
                "(dossier vide)".to_owned()
            },
            Color::DarkGray,
        ),
        DirState::Ready(entries) => {
            let table_rows: Vec<Row> = entries
                .iter()
                .map(|e| {
                    let (label, style) = match e.kind {
                        EntryKind::Dir => (
                            format!("▸ {}/", e.name),
                            Style::new().fg(Color::Cyan).bold(),
                        ),
                        EntryKind::File => (format!("  {}", e.name), Style::new()),
                        EntryKind::Symlink => (
                            format!("  {} (lien)", e.name),
                            Style::new().fg(Color::DarkGray),
                        ),
                        EntryKind::Submodule => (
                            format!("  {} (sous-module)", e.name),
                            Style::new().fg(Color::DarkGray),
                        ),
                    };
                    let size = if e.kind == EntryKind::File {
                        fmt_size(e.size)
                    } else {
                        String::new()
                    };
                    Row::new(vec![
                        Cell::from(Span::styled(label, style)),
                        Cell::from(Line::from(size).right_aligned()),
                    ])
                })
                .collect();
            let table = Table::new(table_rows, [Constraint::Min(10), Constraint::Length(10)])
                .header(Row::new(["Nom", "Taille"]).style(Style::new().bold()))
                .block(block.title_bottom(
                    Line::from(format!(" {}/{} ", b.selected + 1, entries.len())).right_aligned(),
                ))
                .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            // Le décalage est mémorisé d'une image à l'autre : sinon la sélection resterait
            // collée au bas de l'écran à chaque déplacement vers le haut.
            let mut ts = TableState::default().with_offset(b.list_offset.get());
            ts.select(Some(b.selected));
            f.render_stateful_widget(table, area, &mut ts);
            b.list_offset.set(ts.offset());
        }
    }
}

/// Un message seul dans le cadre (chargement, erreur, dossier vide).
fn draw_message(f: &mut Frame, area: Rect, block: Block, text: String, color: Color) {
    let line = Line::from(Span::styled(text, Style::new().fg(color)));
    f.render_widget(Paragraph::new(line).block(block), area);
}

fn draw_branch_picker(f: &mut Frame, b: &Browser, selected: usize) {
    let (items, state): (Vec<ListItem>, bool) = match (&b.branches, &b.branches_error) {
        (Some(list), _) => (
            list.iter()
                .map(|br| {
                    let current = if br.name == b.reference { "● " } else { "  " };
                    let default = if br.default { " (défaut)" } else { "" };
                    ListItem::new(format!("{current}{}{default}", br.name))
                })
                .collect(),
            true,
        ),
        (None, Some(e)) => (
            vec![ListItem::new(Span::styled(
                format!("erreur : {e}"),
                Style::new().fg(Color::Red),
            ))],
            false,
        ),
        (None, None) => (
            vec![ListItem::new(Span::styled(
                "chargement…",
                Style::new().fg(Color::Cyan),
            ))],
            false,
        ),
    };
    let height = (items.len() as u16).clamp(1, 14) + 2;
    let area = centered(f.area(), 56, height);
    f.render_widget(Clear, area);
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Branche ")
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default().with_offset(b.picker_offset.get());
    if state {
        ls.select(Some(selected));
    }
    f.render_stateful_widget(list, area, &mut ls);
    b.picker_offset.set(ls.offset());
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
        lists_help(f.area().width as usize),
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
    use crate::logview::{LineKind, LogLine, LogState};
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

    fn line(kind: LineKind, text: &str) -> LogLine {
        LogLine {
            kind,
            text: text.into(),
            step: None,
        }
    }

    #[test]
    fn vue_des_logs() {
        let lines = vec![
            line(LineKind::Step(Status::Failure), "▸ cargo test"),
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
        app.text_scroll(20);
        let text = render(&app, 80, 12);
        assert!(text.contains("ligne-21") && text.contains("ligne-29"));
        assert!(!text.contains("ligne-20") && !text.contains("ligne-30"));
        assert!(text.contains("21-29/50"));
    }

    fn app_with_job(steps: Vec<smithy_core::StepLog>) -> App {
        let mut app = app_with_logs(LogState::Loading);
        let v = app.logs.as_mut().unwrap();
        v.set_log(&smithy_core::JobLog {
            steps,
            notice: None,
        });
        app
    }

    fn step(name: &str, status: Status, n: usize) -> smithy_core::StepLog {
        smithy_core::StepLog {
            name: name.into(),
            status,
            lines: (1..=n).map(|i| format!("{name}-ligne-{i}")).collect(),
        }
    }

    #[test]
    fn sections_repliees_et_depliees_a_l_ecran() {
        let app = app_with_job(vec![
            step("Preparation", Status::Success, 9),
            step("Tests", Status::Failure, 2),
        ]);
        let text = render(&app, 100, 14);
        assert!(text.contains("▸ Preparation (9 lignes)"));
        assert!(
            !text.contains("Preparation-ligne-1"),
            "le contenu replié est caché"
        );
        assert!(text.contains("▾ Tests") && text.contains("Tests-ligne-2"));
    }

    #[test]
    fn la_section_courante_est_en_video_inverse() {
        use ratatui::style::Modifier;
        let app = app_with_job(vec![
            step("Preparation", Status::Success, 1),
            step("Tests", Status::Failure, 1),
        ]);
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let reversed = |symbol: &str| {
            term.backend()
                .buffer()
                .content()
                .iter()
                .find(|c| c.symbol() == symbol)
                .map(|c| c.modifier.contains(Modifier::REVERSED))
        };
        // En haut de l'écran : « Preparation » (repliée), c'est la section courante.
        assert_eq!(reversed("▸"), Some(true));
        assert_eq!(
            reversed("▾"),
            Some(false),
            "les autres titres ne le sont pas"
        );
    }

    // --- explorateur de code

    use crate::browser::Browser;
    use smithy_core::{Branch, Entry, FileContent};

    fn tree_repo() -> Repo {
        Repo {
            owner: "smithy".into(),
            name: "tree".into(),
        }
    }

    fn explorer_app() -> App {
        let mut app = App::new();
        app.browse_supported = true;
        app.apply(Msg::Repos(Ok(vec![tree_repo()])));
        app.open_browser();
        app
    }

    fn feed_dir(app: &mut App, path: &str, entries: Vec<Entry>) {
        let b = app.browser.as_mut().unwrap();
        b.on_branches(
            &tree_repo(),
            Ok(vec![
                Branch {
                    name: "main".into(),
                    default: true,
                },
                Branch {
                    name: "feature/x".into(),
                    default: false,
                },
            ]),
        );
        b.on_dir(&tree_repo(), "main", path, Ok(entries));
    }

    fn e(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.into(),
            path: name.into(),
            kind,
            size,
        }
    }

    fn browser(app: &App) -> &Browser {
        app.browser.as_ref().unwrap()
    }

    #[test]
    fn explorateur_liste_dossiers_puis_fichiers_avec_tailles() {
        let mut app = explorer_app();
        feed_dir(
            &mut app,
            "",
            vec![
                e("README.md", EntryKind::File, 1_536),
                e("src", EntryKind::Dir, 0),
                e("lien", EntryKind::Symlink, 4),
                e("mod", EntryKind::Submodule, 0),
            ],
        );
        let text = render(&app, 100, 14);
        for needle in [
            "Explorateur",
            "smithy/tree @ main",
            "▸ src/",
            "README.md",
            "1,5 Kio",
            "lien (lien)",
            "mod (sous-module)",
            "Nom",
            "Taille",
            "1/4",
        ] {
            assert!(text.contains(needle), "absent : {needle}\n{text}");
        }
        let (src, readme) = (
            text.find("▸ src/").unwrap(),
            text.find("README.md").unwrap(),
        );
        assert!(src < readme, "les dossiers viennent d'abord");
        assert!(text.contains("B branche"), "aide du dossier");
        assert!(!text.contains("Logs ·"));
    }

    #[test]
    fn explorateur_chemin_etats_et_messages() {
        let mut app = explorer_app();
        assert!(render(&app, 100, 10).contains("chargement"));
        assert!(
            render(&app, 100, 10).contains("@ …"),
            "branche inconnue pendant le chargement"
        );
        feed_dir(&mut app, "", vec![]);
        assert!(render(&app, 100, 10).contains("(dossier vide)"));
        app.browser.as_mut().unwrap().empty_repo = true;
        assert!(render(&app, 100, 10).contains("dépôt vide"));
        let b = app.browser.as_mut().unwrap();
        b.on_dir(
            &tree_repo(),
            "main",
            "",
            Err(smithy_core::Error::NotFound("x".into())),
        );
        assert!(render(&app, 100, 10).contains("erreur"));
        // Message d'information (lien symbolique) : en pied de page à la place de l'aide
        app.browser.as_mut().unwrap().info = Some("lien : lien symbolique, non ouvert".into());
        let t = render(&app, 100, 10);
        assert!(t.contains("non ouvert") && !t.contains("B branche"));
    }

    #[test]
    fn explorateur_affiche_le_dossier_courant() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("src", EntryKind::Dir, 0)]);
        {
            let b = app.browser.as_mut().unwrap();
            b.path = "src/deep".into();
            b.on_dir(
                &tree_repo(),
                "main",
                "src/deep",
                Ok(vec![e("x.rs", EntryKind::File, 3)]),
            );
        }
        assert!(render(&app, 100, 10).contains("@ main · /src/deep"));
    }

    #[test]
    fn explorateur_selecteur_de_branche() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("a.txt", EntryKind::File, 1)]);
        app.browser.as_mut().unwrap().open_branches();
        let t = render(&app, 100, 14);
        assert!(
            t.contains("Branche") && t.contains("● main (défaut)") && t.contains("feature/x"),
            "{t}"
        );
        assert!(t.contains("j/k choisir"));
        // Liste pas encore chargée, puis en erreur
        app.browser.as_mut().unwrap().branches = None;
        assert!(render(&app, 100, 14).contains("chargement"));
        app.browser.as_mut().unwrap().branches_error = Some("coupé".into());
        assert!(render(&app, 100, 14).contains("erreur : coupé"));
    }

    #[test]
    fn explorateur_beaucoup_de_branches_et_d_entrees_defilent() {
        let mut app = explorer_app();
        let entries: Vec<_> = (0..200)
            .map(|i| e(&format!("f{i:03}.txt"), EntryKind::File, i))
            .collect();
        feed_dir(&mut app, "", entries);
        for _ in 0..150 {
            app.browser.as_mut().unwrap().move_selection(1);
            render(&app, 80, 12);
        }
        let t = render(&app, 80, 12);
        assert!(
            t.contains("f150.txt") && !t.contains("f000.txt"),
            "la sélection reste visible"
        );
        assert!(t.contains("151/200"));
        // On remonte d'une entrée : la liste ne saute pas (décalage mémorisé)
        let before = browser(&app).list_offset.get();
        app.browser.as_mut().unwrap().move_selection(-1);
        render(&app, 80, 12);
        assert_eq!(browser(&app).list_offset.get(), before);
    }

    #[test]
    fn remonter_d_un_long_dossier_vers_un_court_montre_le_debut() {
        // Régression : le décalage de l'ancien dossier (130 entrées, sélection en bas) restait
        // appliqué à la racine (14 entrées), dont le début disparaissait au-dessus de l'écran.
        let mut app = explorer_app();
        let root: Vec<_> = ["bin", "docs", "dossier", "many", "src", "zzz"]
            .iter()
            .map(|n| e(n, EntryKind::Dir, 0))
            .collect();
        feed_dir(&mut app, "", root.clone());
        {
            let b = app.browser.as_mut().unwrap();
            b.move_selection(3); // « many »
            b.enter();
            let many: Vec<_> = (0..130)
                .map(|i| Entry {
                    name: format!("f{i:03}.txt"),
                    path: format!("many/f{i:03}.txt"),
                    kind: EntryKind::File,
                    size: 1,
                })
                .collect();
            b.on_dir(&tree_repo(), "main", "many", Ok(many));
            b.select_last();
        }
        for _ in 0..3 {
            render(&app, 80, 14); // le rendu mémorise un décalage élevé
        }
        assert!(browser(&app).list_offset.get() > 100);
        {
            let b = app.browser.as_mut().unwrap();
            b.up();
            b.on_dir(&tree_repo(), "main", "", Ok(root));
        }
        let t = render(&app, 80, 14);
        assert!(
            t.contains("▸ bin/") && t.contains("▸ docs/"),
            "le début de la racine doit être visible\n{t}"
        );
        assert!(t.contains("4/6"), "« many » est resélectionné");
        assert_eq!(browser(&app).list_offset.get(), 0);
    }

    #[test]
    fn aide_de_la_liste_des_depots_adaptee_a_la_largeur() {
        // 80 colonnes : l'ancienne aide tenait, la nouvelle (avec « e ») doit tenir aussi.
        let h = lists_help(80);
        assert!(h.contains("e code") && h.contains("q quitter"), "{h}");
        assert!(lists_help(120).contains("e explorer le code"));
        for w in [100, 80, 60, 40, 20, 8, 3, 0] {
            let h = lists_help(w);
            assert!(h.chars().count() <= w.max(1), "{h:?} dépasse {w}");
            assert!(h.contains('q'), "toujours la sortie : {h:?}");
        }
        // Rendu réel à 80 colonnes
        let app = App::new();
        assert!(render(&app, 80, 10).contains("q quitter"));
    }

    #[test]
    fn explorateur_fichier() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("a.txt", EntryKind::File, 30)]);
        app.browser.as_mut().unwrap().enter();
        assert!(render(&app, 100, 10).contains("chargement"));
        app.browser.as_mut().unwrap().on_file(
            &tree_repo(),
            "main",
            "a.txt",
            Ok(FileContent {
                size: 3 * 1024 * 1024,
                text: Some("première ligne\nseconde\n".into()),
                truncated: true,
            }),
        );
        let t = render(&app, 100, 10);
        for needle in [
            "a.txt",
            "première ligne",
            "seconde",
            "⚠",
            "tronqué",
            "1-2/2",
            "w retour à la ligne",
        ] {
            assert!(t.contains(needle), "absent : {needle}\n{t}");
        }
        assert!(!t.contains("Nom"), "plus de liste de dossier");
        // Erreur, et très petits terminaux
        app.browser.as_mut().unwrap().on_file(
            &tree_repo(),
            "main",
            "a.txt",
            Err(smithy_core::Error::Network("coupé".into())),
        );
        assert!(render(&app, 100, 10).contains("erreur : "));
        for (w, h) in [(1, 1), (10, 3), (30, 4)] {
            render(&app, w, h);
        }
    }

    #[test]
    fn explorateur_tres_petit_terminal() {
        let mut app = explorer_app();
        feed_dir(
            &mut app,
            "",
            vec![e("a.txt", EntryKind::File, 1), e("d", EntryKind::Dir, 0)],
        );
        for (w, h) in [(1, 1), (5, 2), (12, 4)] {
            render(&app, w, h);
        }
        app.browser.as_mut().unwrap().open_branches();
        for (w, h) in [(1, 1), (8, 3), (20, 6)] {
            render(&app, w, h);
        }
    }

    #[test]
    fn aides_de_l_explorateur_adaptees_a_la_largeur() {
        for w in [120, 80, 50, 30, 12, 5, 0] {
            for h in [dir_help(w), file_help(w), branches_help(w)] {
                assert!(h.chars().count() <= w.max(3), "{h:?} dépasse {w}");
                assert!(h.contains("Esc"));
            }
        }
        assert!(dir_help(120).contains("B branche") && dir_help(120).contains("remonter"));
        assert!(file_help(120).contains("w retour à la ligne"));
    }

    #[test]
    fn aide_adaptee_a_la_largeur() {
        for width in [140, 120, 80, 50, 30, 12, 5, 0] {
            let h = logs_help(width);
            assert!(h.chars().count() <= width.max(4), "{h:?} dépasse {width}");
            assert!(h.contains("Esc"), "la sortie reste toujours indiquée");
        }
        assert!(
            logs_help(140).contains("w retour à la ligne"),
            "l'aide complète fait 133 colonnes"
        );
        assert!(
            logs_help(45).contains('w'),
            "la touche w survit aux largeurs courantes"
        );
    }

    #[test]
    fn vue_des_logs_avec_retour_a_la_ligne() {
        let long = format!("DEBUT-{}-FIN", "x".repeat(80));
        let mut app = app_with_logs(LogState::Ready(vec![line(LineKind::Text, &long)]));
        let off = render(&app, 45, 12);
        assert!(
            off.contains("DEBUT") && !off.contains("-FIN"),
            "ligne coupée sans repli"
        );
        assert!(!off.contains('↩'));

        app.text_toggle_wrap(); // la largeur utile a été publiée par le rendu précédent
        let on = render(&app, 45, 12);
        assert!(
            on.contains("DEBUT") && on.contains("-FIN"),
            "tout est lisible une fois replié"
        );
        assert!(on.contains('↩'), "l'état du repli est signalé");
        assert!(on.contains("w repli"), "l'aide s'adapte à la largeur");
        assert!(render(&app, 140, 12).contains("w retour à la ligne"));
        // Replié sur un terminal minuscule : pas de panique
        render(&app, 4, 5);
        render(&app, 1, 1);
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
