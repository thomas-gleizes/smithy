use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Paragraph, Row, Table, TableState},
};
use smithy_core::Status;

use crate::app::{App, Focus};

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
        "j/k ↑/↓ naviguer · tab changer de panneau · r rafraîchir · q quitter",
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(vec![help, second]), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
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

    #[test]
    fn rendu_sur_petit_terminal_et_etat_vide() {
        let mut term = Terminal::new(TestBackend::new(20, 4)).unwrap();
        term.draw(|f| draw(f, &App::new())).unwrap();
    }
}
