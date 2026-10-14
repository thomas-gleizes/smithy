use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
        Wrap,
    },
};
use smithy_core::{EntryKind, PullFilter, RepoInfo, Status};

use crate::{
    app::{App, LoginField, LoginForm, Reach, ServerField, ServerForm, TabMarker, TabTitle},
    browser::{Browser, DirState, Mode, fmt_size},
    config::ServerKind,
    highlight::{self, Tok},
    history::{BranchesView, CommitView, CommitsView},
    logview::{LineKind, LogState, LogView, TextView},
    pages::{HomeView, LastRun, Load, RepoView, RunPos, RunsView, Section, run_positions},
    pulls::{self, PullView, PullsView},
    textsearch::{self, Hit},
    timeago,
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

pub fn draw(f: &mut Frame, app: &App) {
    // En haut : la barre d'onglets (s'il y en a plusieurs), puis une ligne pour dire où l'on est ;
    // le reste est pour la page.
    let tabs = app.tab_count() > 1;
    let layout = Layout::vertical([
        Constraint::Length(u16::from(tabs)),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .split(f.area());
    if tabs {
        draw_tab_bar(f, app, layout[0]);
    }
    let (top, body) = (layout[1], layout[2]);
    draw_breadcrumb(f, app, top);
    if let Some(view) = app.logs() {
        draw_logs(f, app, view, body);
    } else if let Some(b) = app.browser() {
        draw_browser(f, app, b, body);
    } else if let Some(v) = app.commit_view() {
        draw_commit(f, app, v, body);
    } else if let Some(v) = app.pull_view() {
        draw_pull(f, app, v, body);
    } else if let Some(v) = app.pulls() {
        draw_pulls(f, app, v, body);
    } else if let Some(v) = app.commits() {
        draw_commits(f, app, v, body);
    } else if let Some(v) = app.branch_list() {
        draw_branch_list(f, app, v, body);
    } else if let Some(r) = app.runs() {
        draw_runs(f, app, r, body);
    } else if let Some(r) = app.repo_page() {
        draw_repo(f, app, r, body);
    } else if let Some(h) = app.home() {
        draw_home(f, app, h, body);
    }
    if let Some(selected) = app.tab_picker {
        draw_tab_picker(f, app, selected);
    }
    if let Some(selected) = app.server_picker {
        draw_server_picker(f, app, selected);
    }
    if let Some(form) = &app.server_form {
        draw_server_form(f, form);
    }
    if let Some(form) = &app.login {
        let quits = app.login_quits(form);
        draw_login(f, form, quits);
    }
}

/// Une case de la barre d'onglets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TabCell {
    label: String,
    marker: Option<TabMarker>,
    active: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct TabBar {
    cells: Vec<TabCell>,
    more_left: bool,
    more_right: bool,
}

/// Largeur maximale du titre d'un onglet.
const TAB_TITLE_MAX: usize = 20;

fn truncate_end(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut used = 1; // la place du « … »
    for c in s.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push('…');
    out
}

fn marker_symbol(m: TabMarker) -> (&'static str, Color) {
    match m {
        TabMarker::Running => ("●", Color::Yellow),
        TabMarker::Done(Status::Success) => ("✔", Color::Green),
        TabMarker::Done(Status::Failure) => ("✘", Color::Red),
        TabMarker::Done(_) => ("⊘", Color::DarkGray),
    }
}

fn cell_width(c: &TabCell) -> usize {
    // espace, marqueur éventuel + espace, titre, espace
    1 + if c.marker.is_some() { 2 } else { 0 } + text_width(&c.label) + 1
}

/// Les onglets qui tiennent dans `width` colonnes, autour de l'onglet affiché ; ce qui est
/// hors de la barre est signalé par `‹` / `›`.
fn fit_tabs(titles: &[TabTitle], width: usize) -> TabBar {
    let cells: Vec<TabCell> = titles
        .iter()
        .map(|t| TabCell {
            label: truncate_end(&t.text, TAB_TITLE_MAX),
            marker: t.marker,
            active: t.active,
        })
        .collect();
    let Some(active) = cells.iter().position(|c| c.active) else {
        return TabBar {
            cells: Vec::new(),
            more_left: false,
            more_right: false,
        };
    };
    // On grandit autour de l'onglet affiché, un côté puis l'autre, tant que ça tient
    // (2 colonnes en réserve pour les indicateurs, 1 par séparateur).
    let budget = width.saturating_sub(4);
    let (mut lo, mut hi) = (active, active);
    let mut used = cell_width(&cells[active]);
    loop {
        let mut grew = false;
        if hi + 1 < cells.len() {
            let w = 1 + cell_width(&cells[hi + 1]);
            if used + w <= budget {
                used += w;
                hi += 1;
                grew = true;
            }
        }
        if lo > 0 {
            let w = 1 + cell_width(&cells[lo - 1]);
            if used + w <= budget {
                used += w;
                lo -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    TabBar {
        more_left: lo > 0,
        more_right: hi + 1 < cells.len(),
        cells: cells[lo..=hi].to_vec(),
    }
}

/// Les titres d'onglets, préfixés du serveur quand il y en a plusieurs : sans cela deux onglets
/// « secret · CI » de deux serveurs se ressembleraient.
fn display_titles(app: &App) -> Vec<TabTitle> {
    let mut titles = app.tab_titles();
    if app.multi_server() {
        for t in &mut titles {
            t.text = format!("{} · {}", t.server, t.text);
        }
    }
    titles
}

fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let bar = fit_tabs(&display_titles(app), area.width as usize);
    let dim = Style::new().fg(Color::DarkGray);
    let mut spans: Vec<Span> = Vec::new();
    if bar.more_left {
        spans.push(Span::styled("‹ ", dim));
    }
    for (i, c) in bar.cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("│", dim));
        }
        let style = if c.active {
            Style::new().add_modifier(Modifier::REVERSED).bold()
        } else {
            dim
        };
        spans.push(Span::styled(" ", style));
        if let Some(m) = c.marker {
            let (sym, color) = marker_symbol(m);
            let marker_style = if c.active {
                style.fg(color)
            } else {
                Style::new().fg(color)
            };
            spans.push(Span::styled(format!("{sym} "), marker_style));
        }
        spans.push(Span::styled(format!("{} ", c.label), style));
    }
    if bar.more_right {
        spans.push(Span::styled(" ›", dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_server_picker(f: &mut Frame, app: &App, selected: usize) {
    let items: Vec<ListItem> = app
        .servers
        .iter()
        .map(|s| {
            let here = s.name == app.server_name;
            // D'abord le réseau (le serveur répond-il ?), ensuite la session (identifiants).
            let network = match app.reach.get(&s.name) {
                Some(Reach::Up(_)) => Span::styled("joignable", Style::new().fg(Color::Green)),
                Some(Reach::Down(why)) => Span::styled(
                    format!("injoignable ({why})"),
                    Style::new().fg(Color::Red).bold(),
                ),
                _ => Span::styled("vérification…", Style::new().fg(Color::DarkGray)),
            };
            let session = if app.is_connected(&s.name) {
                Span::styled("session ouverte", Style::new().fg(Color::Green))
            } else {
                Span::styled("pas de session", Style::new().fg(Color::DarkGray))
            };
            ListItem::new(Line::from(vec![
                Span::raw(if here { "● " } else { "  " }),
                Span::raw(format!("{} ", s.name)),
                Span::styled(
                    format!("[{}] ({}) ", s.kind.label(), s.url),
                    Style::new().fg(Color::DarkGray),
                ),
                network,
                Span::styled(" · ", Style::new().fg(Color::DarkGray)),
                session,
            ]))
        })
        .collect();
    let height = (items.len() as u16).clamp(1, 12) + 2;
    let area = centered(f.area(), 104, height);
    f.render_widget(Clear, area);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(" Serveurs (cet onglet) ")
        .border_style(Style::new().fg(Color::Yellow));
    if let Some(name) = &app.server_delete {
        block = block.title_bottom(Line::from(Span::styled(
            format!(" Supprimer « {name} » ? y oui · autre touche : non "),
            Style::new().fg(Color::Red).bold(),
        )));
    } else if let Some(note) = &app.server_note {
        block = block.title_bottom(Line::from(Span::styled(
            format!(" {note} "),
            Style::new().fg(Color::Red),
        )));
    } else {
        block = block.title_bottom(
            Line::from(" ↵ choisir · a ajouter · e modifier · d supprimer · Échap ")
                .right_aligned(),
        );
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    ls.select(Some(selected));
    f.render_stateful_widget(list, area, &mut ls);
}

fn draw_server_form(f: &mut Frame, form: &ServerForm) {
    let area = centered(f.area(), 76, 15);
    f.render_widget(Clear, area);
    let editing = form.original.is_some();
    let field = |label: &str, value: String, which: ServerField, locked: bool| {
        let active = form.field == which;
        let cursor = if active { "▏" } else { "" };
        let style = if active {
            Style::new().fg(Color::Yellow)
        } else if locked {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        };
        Line::from(vec![
            Span::styled(format!("{label:<8}"), style),
            Span::raw(format!("{value}{cursor}")),
        ])
    };
    let token_hint = match (editing, form.kind) {
        (true, _) => "(vide : inchangé)",
        (false, ServerKind::GitHub) => "(obligatoire : GitHub n'accepte pas de mot de passe)",
        (false, ServerKind::Forgejo) => "(facultatif : sinon connexion par mot de passe)",
    };
    let kind = if editing {
        form.kind.label().to_owned()
    } else {
        format!("‹ {} ›", form.kind.label())
    };
    let mut lines = Vec::new();
    if form.first_run {
        lines.push(Line::from(Span::styled(
            "Aucun serveur configuré : indiquez la forge à consulter.",
            Style::new().fg(Color::DarkGray),
        )));
    }
    lines.extend([
        field("Type", kind, ServerField::Kind, editing),
        field("Nom", form.name.clone(), ServerField::Name, editing),
        field("URL", form.url.clone(), ServerField::Url, false),
        field(
            "Token",
            "•".repeat(form.token.chars().count()),
            ServerField::Token,
            false,
        ),
        Line::from(Span::styled(
            format!("        {token_hint}"),
            Style::new().fg(Color::DarkGray),
        )),
        Line::from(""),
    ]);
    lines.push(match &form.error {
        Some(e) => Line::from(Span::styled(e.clone(), Style::new().fg(Color::Red))),
        None => Line::from(""),
    });
    let escape = if form.first_run { "quitter" } else { "annuler" };
    let help = if form.field == ServerField::Kind {
        format!("←/→/Espace changer de type · Entrée suivant · Esc {escape}")
    } else {
        format!("Entrée suivant / enregistrer · Tab changer de champ · Esc {escape}")
    };
    lines.push(Line::from(Span::styled(
        help,
        Style::new().fg(Color::DarkGray),
    )));
    let title = match &form.original {
        Some(n) => format!(" Modifier le serveur · {n} "),
        None if form.first_run => " Bienvenue dans smithy · premier serveur ".to_owned(),
        None => " Nouveau serveur ".to_owned(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    // Le message (erreur, ou ce qu'il faut pour GitHub) peut tenir sur deux lignes.
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_tab_picker(f: &mut Frame, app: &App, selected: usize) {
    let titles = display_titles(app);
    let items: Vec<ListItem> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut spans = vec![Span::raw(format!("{:>2}. ", i + 1))];
            if let Some(m) = t.marker {
                let (sym, color) = marker_symbol(m);
                spans.push(Span::styled(format!("{sym} "), Style::new().fg(color)));
            }
            spans.push(Span::raw(t.text.clone()));
            if t.active {
                spans.push(Span::styled(
                    "  (affiché)",
                    Style::new().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let height = (items.len() as u16).clamp(1, 12) + 2;
    let area = centered(f.area(), 50, height);
    f.render_widget(Clear, area);
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Onglets ")
                .title_bottom(Line::from(" ↵ aller · x fermer · Échap ").right_aligned())
                .border_style(Style::new().fg(Color::Yellow)),
        )
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut ls = ListState::default();
    ls.select(Some(selected));
    f.render_stateful_widget(list, area, &mut ls);
}

const CRUMB_SEP: &str = " › ";

fn text_width(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

fn joined_width(parts: &[String]) -> usize {
    text_width(&parts.join(CRUMB_SEP))
}

/// Fil d'Ariane qui tient dans `width` colonnes : on garde le serveur au début et la page à la
/// fin (ce qui compte), on remplace le milieu par « … » en commençant par le plus ancien, et en
/// dernier recours on coupe le début du texte.
fn fit_breadcrumb(crumbs: &[String], width: usize) -> Vec<String> {
    if crumbs.is_empty() || joined_width(crumbs) <= width {
        return crumbs.to_vec();
    }
    let n = crumbs.len();
    for dropped in 1..n.saturating_sub(1) {
        let mut parts = vec![crumbs[0].clone(), "…".to_owned()];
        parts.extend_from_slice(&crumbs[1 + dropped..]);
        if joined_width(&parts) <= width {
            return parts;
        }
    }
    let mut parts = vec![crumbs[0].clone()];
    if n > 1 {
        parts.push("…".to_owned());
        parts.push(crumbs[n - 1].clone());
    }
    if joined_width(&parts) <= width {
        return parts;
    }
    // Même ainsi trop long : on ne garde que la fin, la partie la plus précise.
    let text = crumbs.join(CRUMB_SEP);
    let mut kept: Vec<char> = Vec::new();
    let mut used = 1; // la place du « … » de tête
    for c in text.chars().rev() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        kept.push(c);
    }
    if width == 0 {
        return vec![String::new()];
    }
    kept.reverse();
    vec![format!("…{}", kept.into_iter().collect::<String>())]
}

fn draw_breadcrumb(f: &mut Frame, app: &App, area: Rect) {
    let parts = fit_breadcrumb(&app.breadcrumb(), area.width as usize);
    let last = parts.len().saturating_sub(1);
    let mut spans = Vec::new();
    for (i, p) in parts.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(CRUMB_SEP, Style::new().fg(Color::DarkGray)));
        }
        let style = if i == last {
            Style::new().bold()
        } else {
            Style::new().fg(Color::DarkGray)
        };
        spans.push(Span::styled(p, style));
    }
    let used: usize = spans.iter().map(|s| text_width(&s.content)).sum();
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    // Une autre instance gère les onglets : on le dit à droite s'il reste de la place.
    if app.shared_instance {
        let note = " onglets non sauvegardés : autre instance ";
        if used + text_width(note) < area.width as usize {
            f.render_widget(
                Paragraph::new(Span::styled(note, Style::new().fg(Color::Yellow)))
                    .alignment(ratatui::layout::Alignment::Right),
                area,
            );
        }
    }
}

fn draw_logs(f: &mut Frame, app: &App, view: &LogView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let (status, color) = status_style(view.pipeline.status);
    // Le dépôt, le workflow et le job sont déjà dans le fil d'Ariane : le cadre garde le statut.
    let title = Line::from(vec![
        Span::raw(format!(" {} · ", view.pipeline.job)),
        Span::styled(format!("{status} "), Style::new().fg(color)),
    ]);
    draw_text_pane(f, app, rows[0], title, view);
    let search = search_line(view);
    draw_footer(
        f,
        app,
        rows[1],
        logs_help(area.width as usize),
        search.as_deref(),
    );
}

/// Volet de texte plein cadre : sortie d'un job ou contenu d'un fichier, avec sa position et sa
/// réserve éventuelle dans la bordure du bas.
/// Les couleurs des jetons : les 16 couleurs ANSI, donc celles du terminal de l'utilisateur
/// (aucun thème RGB imposé).
fn tok_style(tok: Tok) -> Style {
    let fg = |c| Style::new().fg(c);
    match tok {
        Tok::Comment | Tok::Quote => fg(Color::DarkGray),
        Tok::Str => fg(Color::Green),
        Tok::Number | Tok::Constant | Tok::Attr => fg(Color::Yellow),
        Tok::Keyword => fg(Color::Magenta),
        Tok::Type => fg(Color::Cyan),
        Tok::Function => fg(Color::LightBlue),
        Tok::Tag => fg(Color::Red),
        Tok::Heading => fg(Color::Cyan).bold(),
        Tok::Strong => Style::new().bold(),
        Tok::Emph => Style::new().add_modifier(Modifier::ITALIC),
        Tok::Strike => Style::new().add_modifier(Modifier::CROSSED_OUT),
        Tok::Link => fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
        Tok::Code => fg(Color::Green),
        Tok::Added => fg(Color::Green),
        Tok::Removed => fg(Color::Red),
        Tok::Hunk => fg(Color::Cyan),
        Tok::Meta => fg(Color::DarkGray),
    }
}

/// Mise en évidence des occurrences d'une recherche : en vidéo inverse, et en jaune pour celle
/// où l'on est arrivé avec `n` / `N`.
fn hit_style(hit: Hit) -> Style {
    match hit {
        Hit::No => Style::new(),
        Hit::Other => Style::new().add_modifier(Modifier::REVERSED),
        Hit::Current => Style::new()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    }
}

/// Ligne d'état de la recherche d'un volet de texte : la saisie en cours, ou le bilan et les
/// touches. `None` sans recherche : le pied de page affiche alors l'aide.
fn search_line(view: &TextView) -> Option<String> {
    if let Some(p) = &view.prompt {
        return Some(format!("/{p}▏   Entrée valider · Échap annuler"));
    }
    view.search_status().map(|(q, current, total)| {
        if total == 0 {
            format!("/{q}   aucune occurrence · / autre recherche · Échap efface")
        } else if current == 0 {
            format!("/{q}   {total} occurrence(s) · n suivante · N précédente · Échap efface")
        } else {
            format!("/{q}   {current}/{total} · n suivante · N précédente · Échap efface")
        }
    })
}

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
                    LineKind::Text
                        if r.runs.is_empty() && r.hang.is_empty() && !view.has_search() =>
                    {
                        Line::from(r.text)
                    }
                    LineKind::Text => {
                        let mut spans = Vec::new();
                        // Suite d'une ligne repliée : le retrait de la puce, la barre de citation.
                        if r.continuation && !r.hang.is_empty() {
                            spans.push(Span::styled(r.hang, tok_style(Tok::Quote)));
                        }
                        let segments = highlight::segments(r.text, r.offset, r.runs);
                        // Les occurrences de la recherche, de toute la ligne (une occurrence peut
                        // être à cheval sur deux lignes d'écran), par-dessus les couleurs.
                        let ranges = view.line_matches(r.line);
                        let current = view
                            .current_match()
                            .filter(|m| m.line == r.line_no)
                            .map(|m| (m.start, m.end));
                        spans.extend(
                            textsearch::overlay(segments, r.offset, &ranges, current)
                                .into_iter()
                                .map(|(text, tok, hit)| {
                                    let base = tok.map_or_else(Style::new, tok_style);
                                    Span::styled(text, base.patch(hit_style(hit)))
                                }),
                        );
                        Line::from(spans)
                    }
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

fn home_help(width: usize, section: Section, editing: bool) -> &'static str {
    if editing {
        return pick_help(
            width,
            &["Entrée valider · Échap annuler", "↵ · Échap", "Échap"],
        );
    }
    if section == Section::Explore {
        return pick_help(
            width,
            &[
                "/ chercher un dépôt · ←/→ section · ↵ ouvrir · j/k naviguer · q quitter",
                "/ chercher · ←/→ section · ↵ · q quitter",
                "/ · ←/→ · ↵ · q",
                "q",
            ],
        );
    }
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ←/→ section · ↵ ouvrir · t onglet · / filtrer · r actualiser · q quitter",
            "j/k · ←/→ section · ↵ ouvrir · / filtrer · q quitter",
            "←/→ · ↵ · / · q quitter",
            "q",
        ],
    )
}

fn repo_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ choisir · ↵/→ ouvrir · t onglet · ←/Échap retour · r actualiser · q retour",
            "j/k · ↵ ouvrir · ← retour · r actualiser",
            "↵ · ← · r",
            "Échap",
        ],
    )
}

fn runs_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ↵/→ logs · / filtrer · s statut · t onglet · ←/Échap retour · r actualiser",
            "j/k · ↵ logs · / filtrer · s statut · ← retour",
            "j/k · ↵ · / · s · ←",
            "↵ · ← · r",
            "Échap",
        ],
    )
}

fn logs_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · ←/→ section · Espace plier · a tout · / chercher · n/N · w retour à la ligne · r rafraîchir · Esc retour",
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · ←/→ section · Espace plier · a tout · w retour à la ligne · r rafraîchir · Esc retour",
            "j/k défiler · ←/→ section · Espace plier · / chercher · w retour à la ligne · Esc retour",
            "j/k défiler · ←/→ section · Espace plier · a tout · w repli · Esc retour",
            "←/→ · Espace plier · / chercher · w repli · Esc retour",
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
            "j/k ↑/↓ naviguer · →/↵ ouvrir · ←/⌫ remonter · / filtrer · B branche · t onglet · r actualiser · Esc retour",
            "j/k · →/↵ ouvrir · ← remonter · / filtrer · B branche · Esc retour",
            "→ ouvrir · ← haut · / filtrer · B branche · Esc",
            "→ · ← · B · Esc",
            "Esc",
        ],
    )
}

fn file_help(width: usize, markdown: bool, rendered: bool) -> &'static str {
    if markdown {
        // Le rendu se replie toujours par mots : `w` n'y sert pas.
        return pick_help(
            width,
            if rendered {
                &[
                    "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · / chercher · n/N · m texte source · r actualiser · Esc/← retour",
                    "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · m texte source · r actualiser · Esc/← retour",
                    "j/k défiler · g/G · / chercher · m source · Esc retour",
                    "j/k défiler · g/G · m source · Esc retour",
                    "j/k · m · Esc",
                    "m · Esc",
                    "Esc",
                ]
            } else {
                &[
                    "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · / chercher · n/N · w retour à la ligne · m rendu · r actualiser · Esc/← retour",
                    "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · w retour à la ligne · m rendu · r actualiser · Esc/← retour",
                    "j/k défiler · g/G · / chercher · w repli · m rendu · Esc retour",
                    "j/k défiler · g/G · w repli · m rendu · Esc retour",
                    "j/k · w · m · Esc",
                    "m · Esc",
                    "Esc",
                ]
            },
        );
    }
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · / chercher · n/N · w retour à la ligne · r actualiser · Esc/← retour",
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · w retour à la ligne · r actualiser · Esc/← retour",
            "j/k ↑/↓ défiler · g/G début/fin · / chercher · w retour à la ligne · Esc/← retour",
            "j/k défiler · g/G · w retour à la ligne · Esc retour",
            "j/k · / · w repli · Esc retour",
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
fn draw_browser(f: &mut Frame, app: &App, b: &Browser, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let width = area.width as usize;
    // Dépôt et branche sont dans le fil d'Ariane : le cadre montre le chemin.
    if let Mode::File(fv) = &b.mode {
        // Un fichier Markdown dit quelle vue il montre ; `m` bascule.
        let mode = match (fv.rendered.is_some(), fv.is_rendered()) {
            (true, true) => " · rendu",
            (true, false) => " · source",
            _ => "",
        };
        let title = Line::from(format!(" /{}{mode} ", fv.path));
        draw_text_pane(f, app, rows[0], title, fv.view());
        let help = file_help(width, fv.rendered.is_some(), fv.is_rendered());
        let search = search_line(fv.view());
        draw_footer(f, app, rows[1], help, search.as_deref());
        return;
    }
    let filter = if b.filter_active() {
        format!(" · « {} »", b.filter.trim())
    } else {
        String::new()
    };
    draw_dir(f, app, b, rows[0], format!(" /{}{filter} ", b.path));
    let help = if matches!(b.mode, Mode::Branches { .. }) {
        branches_help(width)
    } else {
        dir_help(width)
    };
    let prompt = b
        .editing
        .then(|| format!("filtrer : {}▏   Entrée valider · Échap annuler", b.filter));
    draw_footer(
        f,
        app,
        rows[1],
        help,
        prompt.as_deref().or(b.info.as_deref()),
    );
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
        DirState::Ready(_) if b.visible().is_empty() => draw_message(
            f,
            area,
            block,
            "(aucune entrée ne correspond au filtre · Échap l'efface)".to_owned(),
            Color::DarkGray,
        ),
        DirState::Ready(entries) => {
            let visible = b.visible();
            let table_rows: Vec<Row> = visible
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
                .block(
                    block.title_bottom(
                        Line::from(if b.filter_active() {
                            format!(
                                " {}/{} (sur {}) ",
                                b.selected + 1,
                                visible.len(),
                                entries.len()
                            )
                        } else {
                            format!(" {}/{} ", b.selected + 1, visible.len())
                        })
                        .right_aligned(),
                    ),
                )
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
    draw_branch_modal(
        f,
        &b.branches,
        &b.branches_error,
        &b.reference,
        &b.picker_offset,
        selected,
    );
}

/// La liste des branches par-dessus la page (explorateur et historique) : `●` marque la courante.
fn draw_branch_modal(
    f: &mut Frame,
    branches: &Option<Vec<smithy_core::Branch>>,
    error: &Option<String>,
    current_name: &str,
    offset: &std::cell::Cell<usize>,
    selected: usize,
) {
    let (items, state): (Vec<ListItem>, bool) = match (branches, error) {
        (Some(list), _) => (
            list.iter()
                .map(|br| {
                    let current = if br.name == current_name {
                        "● "
                    } else {
                        "  "
                    };
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
    let mut ls = ListState::default().with_offset(offset.get());
    if state {
        ls.select(Some(selected));
    }
    f.render_stateful_widget(list, area, &mut ls);
    offset.set(ls.offset());
}

fn history_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ↵/→ le commit · / filtrer · B branche · t onglet · r actualiser · ←/Échap retour",
            "j/k · ↵ commit · / filtrer · B branche · ← retour",
            "↵ · / · B · ←",
            "Échap",
        ],
    )
}

fn branch_list_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ↵/→ historique · e code · / filtrer · t onglet · r actualiser · ←/Échap retour",
            "j/k · ↵ historique · e code · / filtrer · ← retour",
            "↵ · e · / · ←",
            "Échap",
        ],
    )
}

fn commit_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · ←/→ fichier · Espace plier · a tout · / chercher · n/N · w repli · r actualiser · Esc retour",
            "j/k défiler · ←/→ fichier · Espace plier · / chercher · w repli · Esc retour",
            "←/→ · Espace · / · w · Esc",
            "Esc",
        ],
    )
}

/// Pied de page d'une liste filtrable : la saisie du filtre, sinon l'erreur ou le message, sinon
/// l'aide.
fn list_footer(
    f: &mut Frame,
    app: &App,
    area: Rect,
    first: Line<'static>,
    editing: Option<&str>,
    help: &'static str,
) {
    let first = match editing {
        Some(text) => Line::from(Span::styled(
            format!("filtrer : {text}▏"),
            Style::new().fg(Color::Yellow),
        )),
        None => first,
    };
    let help = if editing.is_some() {
        "Entrée valider · Échap annuler"
    } else {
        help
    };
    let _ = app;
    let help = Line::from(Span::styled(help, Style::new().fg(Color::DarkGray)));
    f.render_widget(Paragraph::new(vec![first, help]), area);
}

fn draw_commits(f: &mut Frame, app: &App, v: &CommitsView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    let mut title = format!(" Commits · {}", v.branch_label());
    if v.filter.active() {
        title.push_str(&format!(" · « {} »", v.filter.text.trim()));
    }
    title.push(' ');
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rows[0]);
    app.viewport.set((inner.height as usize).saturating_sub(1));
    let visible = v.visible();
    if visible.is_empty() {
        let (text, color) = if let Some(e) = &v.error {
            (format!("erreur : {e}"), Color::Red)
        } else if v.loading {
            ("chargement…".to_owned(), Color::Cyan)
        } else if v.commits.is_empty() {
            ("(aucun commit)".to_owned(), Color::DarkGray)
        } else {
            (
                "(aucun commit ne correspond au filtre · Échap l'efface)".to_owned(),
                Color::DarkGray,
            )
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color)))).block(block),
            rows[0],
        );
    } else {
        let now = timeago::now_epoch();
        let table_rows: Vec<Row> = visible
            .iter()
            .map(|c| {
                Row::new(vec![
                    Cell::from(Span::styled(
                        c.short_sha().to_owned(),
                        Style::new().fg(Color::Yellow),
                    )),
                    Cell::from(
                        c.date
                            .as_deref()
                            .map(|d| timeago::ago_rfc3339(now, d))
                            .unwrap_or_default(),
                    ),
                    Cell::from(c.author.clone()),
                    Cell::from(c.summary().to_owned()),
                ])
            })
            .collect();
        let more = if v.has_more { "+" } else { "" };
        let counter = if v.filter.active() {
            format!(
                " {}/{} (sur {}{more} chargés) ",
                v.selected + 1,
                visible.len(),
                v.commits.len()
            )
        } else {
            format!(" {}/{}{more} ", v.selected + 1, visible.len())
        };
        let table = Table::new(
            table_rows,
            [
                Constraint::Length(8),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Min(10),
            ],
        )
        .header(Row::new(["Commit", "Date", "Auteur", "Message"]).style(Style::new().bold()))
        .block(block.title_bottom(Line::from(counter).right_aligned()))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut ts = TableState::default().with_offset(v.list_offset.get());
        ts.select(Some(v.selected));
        f.render_stateful_widget(table, rows[0], &mut ts);
        v.list_offset.set(ts.offset());
    }
    let first = if let Some(err) = app.error.as_ref().or(v.error.as_ref()) {
        Line::from(Span::styled(
            format!("erreur : {err}"),
            Style::new().fg(Color::Red),
        ))
    } else if v.loading && !v.commits.is_empty() {
        Line::from(Span::styled(
            "chargement de la suite…",
            Style::new().fg(Color::Cyan),
        ))
    } else if let Some(url) = v.selected_commit().and_then(|c| c.url.clone()) {
        Line::from(format!("commit : {url}"))
    } else {
        Line::from("")
    };
    list_footer(
        f,
        app,
        rows[1],
        first,
        v.filter.editing.then_some(v.filter.text.as_str()),
        history_help(area.width as usize),
    );
    if let Some(selected) = v.picker {
        draw_branch_modal(
            f,
            &v.branches,
            &None,
            if v.reference.is_empty() {
                v.default_branch.as_deref().unwrap_or("")
            } else {
                &v.reference
            },
            &v.picker_offset,
            selected,
        );
    }
}

fn draw_branch_list(f: &mut Frame, app: &App, v: &BranchesView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    let mut title = String::from(" Branches");
    if v.filter.active() {
        title.push_str(&format!(" · « {} »", v.filter.text.trim()));
    }
    title.push(' ');
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rows[0]);
    app.viewport.set((inner.height as usize).saturating_sub(1));
    let visible = v.visible();
    if visible.is_empty() {
        let (text, color) = match (&v.branches, &v.error) {
            (_, Some(e)) if v.branches.is_none() => (format!("erreur : {e}"), Color::Red),
            (None, _) => ("chargement…".to_owned(), Color::Cyan),
            (Some(b), _) if b.is_empty() => ("(aucune branche)".to_owned(), Color::DarkGray),
            _ => (
                "(aucune branche ne correspond au filtre · Échap l'efface)".to_owned(),
                Color::DarkGray,
            ),
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color)))).block(block),
            rows[0],
        );
    } else {
        let now = timeago::now_epoch();
        let table_rows: Vec<Row> = visible
            .iter()
            .map(|b| {
                let mut name = vec![
                    Span::raw(if b.default { "● " } else { "  " }),
                    Span::raw(b.name.clone()),
                ];
                if b.protected {
                    name.push(Span::styled(
                        " [protégée]",
                        Style::new().fg(Color::DarkGray),
                    ));
                }
                let head = b.head.as_ref();
                Row::new(vec![
                    Cell::from(Line::from(name)),
                    Cell::from(Span::styled(
                        head.map_or(String::new(), |h| h.sha.chars().take(7).collect()),
                        Style::new().fg(Color::Yellow),
                    )),
                    Cell::from(
                        head.and_then(|h| h.date.as_deref())
                            .map(|d| timeago::ago_rfc3339(now, d))
                            .unwrap_or_default(),
                    ),
                    Cell::from(head.map_or(String::new(), |h| h.author.clone())),
                    Cell::from(head.map_or(String::new(), |h| {
                        h.message.lines().next().unwrap_or("").trim().to_owned()
                    })),
                ])
            })
            .collect();
        let total = v.branches.as_ref().map_or(0, Vec::len);
        let counter = if v.filter.active() {
            format!(" {}/{} (sur {total}) ", v.selected + 1, visible.len())
        } else {
            format!(" {}/{} ", v.selected + 1, visible.len())
        };
        let table = Table::new(
            table_rows,
            [
                Constraint::Length(28),
                Constraint::Length(8),
                Constraint::Length(14),
                Constraint::Length(14),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(["Branche", "Commit", "Date", "Auteur", "Message"]).style(Style::new().bold()),
        )
        .block(block.title_bottom(Line::from(counter).right_aligned()))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut ts = TableState::default().with_offset(v.list_offset.get());
        ts.select(Some(v.selected));
        f.render_stateful_widget(table, rows[0], &mut ts);
        v.list_offset.set(ts.offset());
    }
    let first = if let Some(err) = app.error.as_ref().or(v.error.as_ref()) {
        Line::from(Span::styled(
            format!("erreur : {err}"),
            Style::new().fg(Color::Red),
        ))
    } else {
        Line::from("")
    };
    list_footer(
        f,
        app,
        rows[1],
        first,
        v.filter.editing.then_some(v.filter.text.as_str()),
        branch_list_help(area.width as usize),
    );
}

fn draw_commit(f: &mut Frame, app: &App, v: &CommitView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let title = Line::from(format!(" {} ", v.summary));
    draw_text_pane(f, app, rows[0], title, &v.text);
    let search = search_line(&v.text);
    draw_footer(
        f,
        app,
        rows[1],
        commit_help(area.width as usize),
        search.as_deref(),
    );
}

fn pulls_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ naviguer · ↵/→ la PR · s état · / filtrer · e code · t onglet · r actualiser · ←/Échap retour",
            "j/k · ↵ PR · s état · / filtrer · e code · ← retour",
            "↵ · s · / · e · ←",
            "Échap",
        ],
    )
}

fn pull_help(width: usize) -> &'static str {
    pick_help(
        width,
        &[
            "j/k ↑/↓ défiler · f/PgDn page · g/G début/fin · ←/→ section · Espace plier · a tout · / chercher · n/N · e code · w repli · r actualiser · Esc retour",
            "j/k défiler · ←/→ section · Espace plier · / chercher · e code · w repli · Esc retour",
            "←/→ · Espace · / · e · w · Esc",
            "Esc",
        ],
    )
}

fn draw_pulls(f: &mut Frame, app: &App, v: &PullsView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    let mut title = format!(" Pull requests · {}", pulls::filter_label(v.state));
    if v.filter.active() {
        title.push_str(&format!(" · « {} »", v.filter.text.trim()));
    }
    title.push(' ');
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rows[0]);
    app.viewport.set((inner.height as usize).saturating_sub(1));
    let visible = v.visible();
    if visible.is_empty() {
        let (text, color) = if let Some(e) = &v.error {
            (format!("erreur : {e}"), Color::Red)
        } else if v.loading {
            ("chargement…".to_owned(), Color::Cyan)
        } else if v.pulls.is_empty() {
            (
                match v.state {
                    PullFilter::Open => "(aucune pull request ouverte · s change d'état)",
                    PullFilter::Closed => "(aucune pull request fermée · s change d'état)",
                    PullFilter::All => "(aucune pull request)",
                }
                .to_owned(),
                Color::DarkGray,
            )
        } else {
            (
                "(aucune pull request ne correspond au filtre · Échap l'efface)".to_owned(),
                Color::DarkGray,
            )
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color)))).block(block),
            rows[0],
        );
    } else {
        let now = timeago::now_epoch();
        let dim = Style::new().fg(Color::DarkGray);
        let table_rows: Vec<Row> = visible
            .iter()
            .map(|p| {
                let (_, color) = status_style(pulls::state_status(p));
                let mut title = vec![Span::raw(p.title.clone())];
                for l in &p.labels {
                    title.push(tag(l));
                }
                if p.mergeable == Some(false) {
                    title.push(Span::styled(" [conflits]", Style::new().fg(Color::Red)));
                }
                let when = p
                    .closed
                    .as_deref()
                    .or(p.updated.as_deref())
                    .or(p.created.as_deref())
                    .map(|d| timeago::ago_rfc3339(now, d))
                    .unwrap_or_default();
                Row::new(vec![
                    Cell::from(Span::styled(
                        format!("#{}", p.number),
                        Style::new().fg(Color::Yellow),
                    )),
                    Cell::from(Span::styled(pulls::state_label(p), Style::new().fg(color))),
                    Cell::from(Line::from(title)),
                    Cell::from(Span::styled(format!("{} → {}", p.head, p.base), dim)),
                    Cell::from(p.author.clone()),
                    Cell::from(when),
                ])
            })
            .collect();
        let more = if v.has_more { "+" } else { "" };
        let counter = if v.filter.active() {
            format!(
                " {}/{} (sur {}{more} chargées) ",
                v.selected + 1,
                visible.len(),
                v.pulls.len()
            )
        } else {
            format!(" {}/{}{more} ", v.selected + 1, visible.len())
        };
        let table = Table::new(
            table_rows,
            [
                Constraint::Length(6),
                Constraint::Length(10),
                Constraint::Min(20),
                Constraint::Max(30),
                Constraint::Length(12),
                Constraint::Length(14),
            ],
        )
        .header(
            Row::new(["PR", "État", "Titre", "Branches", "Auteur", "Activité"])
                .style(Style::new().bold()),
        )
        .block(block.title_bottom(Line::from(counter).right_aligned()))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut ts = TableState::default().with_offset(v.list_offset.get());
        ts.select(Some(v.selected));
        f.render_stateful_widget(table, rows[0], &mut ts);
        v.list_offset.set(ts.offset());
    }
    let first = if let Some(err) = app.error.as_ref().or(v.error.as_ref()) {
        Line::from(Span::styled(
            format!("erreur : {err}"),
            Style::new().fg(Color::Red),
        ))
    } else if v.loading && !v.pulls.is_empty() {
        Line::from(Span::styled(
            "chargement de la suite…",
            Style::new().fg(Color::Cyan),
        ))
    } else if let Some(p) = v.selected_pull() {
        // Sans compteurs (GitHub ne les donne qu'avec le détail), seulement l'adresse.
        let mut parts: Vec<String> = Vec::new();
        if let Some(st) = p.stats {
            parts.push(format!(
                "{} · +{} −{} · {} commentaire{}",
                if st.changed_files > 1 {
                    format!("{} fichiers", st.changed_files)
                } else {
                    format!("{} fichier", st.changed_files)
                },
                st.additions,
                st.deletions,
                st.comments,
                if st.comments > 1 { "s" } else { "" }
            ));
        }
        if let Some(url) = &p.url {
            parts.push(url.clone());
        }
        Line::from(parts.join(" · "))
    } else {
        Line::from("")
    };
    list_footer(
        f,
        app,
        rows[1],
        first,
        v.filter.editing.then_some(v.filter.text.as_str()),
        pulls_help(area.width as usize),
    );
}

fn draw_pull(f: &mut Frame, app: &App, v: &PullView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let title = if v.title.is_empty() {
        Line::from(format!(" #{} ", v.number))
    } else {
        Line::from(format!(" #{} {} ", v.number, v.title))
    };
    draw_text_pane(f, app, rows[0], title, &v.text);
    let search = search_line(&v.text);
    draw_footer(
        f,
        app,
        rows[1],
        pull_help(area.width as usize),
        search.as_deref(),
    );
}

fn tag(text: &str) -> Span<'static> {
    Span::styled(format!(" [{text}]"), Style::new().fg(Color::DarkGray))
}

/// Marques d'un dépôt à la suite de son nom : privé, archivé, fork, vide.
fn repo_tags(info: &RepoInfo) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (on, label) in [
        (info.private, "privé"),
        (info.archived, "archivé"),
        (info.fork, "fork"),
        (info.empty, "vide"),
    ] {
        if on {
            out.push(tag(label));
        }
    }
    out
}

fn draw_home(f: &mut Frame, app: &App, h: &HomeView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    // Les sections sont le titre du cadre, la courante en vidéo inverse.
    let mut title: Vec<Span> = Vec::new();
    for s in Section::ALL {
        let style = if s == h.section {
            Style::new()
                .fg(Color::Yellow)
                .bold()
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        title.push(Span::styled(format!(" {} ", s.label()), style));
    }
    let data = h.data();
    let list = app.home_rows();
    let selected = h.selected(list.len());
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title))
        .border_style(Style::new().fg(Color::Yellow));
    if h.editing || !data.filter.is_empty() {
        let verb = if h.section == Section::Explore {
            "chercher"
        } else {
            "filtrer"
        };
        let cursor = if h.editing { "▏" } else { "" };
        block = block.title_bottom(Line::from(Span::styled(
            format!(" {verb} : {}{cursor} ", data.filter),
            Style::new().fg(Color::Yellow),
        )));
    }
    let inner = block.inner(rows[0]);
    app.viewport.set((inner.height as usize).saturating_sub(1));

    let message = |text: String, color: Color| {
        Paragraph::new(Line::from(Span::styled(text, Style::new().fg(color)))).block(block.clone())
    };
    let empty = match h.section {
        Section::Recent => "(aucun dépôt ouvert récemment)".to_owned(),
        Section::Explore => format!("(aucun résultat pour « {} »)", h.submitted()),
        _ if !data.filter.is_empty() => {
            format!("(aucun dépôt ne correspond à « {} »)", data.filter)
        }
        _ => "(aucun dépôt)".to_owned(),
    };
    let widget_message = match (&data.load, h.section) {
        (_, Section::Recent) if list.is_empty() => Some(message(empty.clone(), Color::DarkGray)),
        (Load::Idle, Section::Explore) => Some(message(
            "Tapez / puis le nom d'un dépôt à chercher sur tout le serveur.".to_owned(),
            Color::DarkGray,
        )),
        (Load::Idle | Load::Loading, _) if h.section != Section::Recent => {
            Some(message("chargement…".to_owned(), Color::Cyan))
        }
        (Load::Failed(e), _) => Some(message(format!("erreur : {e}"), Color::Red)),
        _ if list.is_empty() => Some(message(empty, Color::DarkGray)),
        _ => None,
    };
    match widget_message {
        Some(w) => f.render_widget(w, rows[0]),
        None => {
            let now = timeago::now_epoch();
            let recent = h.section == Section::Recent;
            let table_rows: Vec<Row> = list
                .iter()
                .map(|r| {
                    let mut name = vec![Span::raw(r.repo.full_name())];
                    if let Some(i) = &r.info {
                        name.extend(repo_tags(i));
                    }
                    let mut cells = vec![Cell::from(Line::from(name))];
                    if recent {
                        let when = r.opened.map(|t| timeago::ago(now, t)).unwrap_or_default();
                        cells.push(Cell::from(when));
                    } else if let Some(i) = &r.info {
                        let prs = if i.open_prs > 0 {
                            format!("{} PR", i.open_prs)
                        } else {
                            String::new()
                        };
                        let stars = if i.stars > 0 {
                            format!("★ {}", i.stars)
                        } else {
                            String::new()
                        };
                        let when = i
                            .updated
                            .as_deref()
                            .map(|u| timeago::ago_rfc3339(now, u))
                            .unwrap_or_default();
                        cells.extend([Cell::from(prs), Cell::from(stars), Cell::from(when)]);
                    }
                    Row::new(cells)
                })
                .collect();
            let (widths, header): (Vec<Constraint>, Vec<&str>) = if recent {
                (
                    vec![Constraint::Min(10), Constraint::Length(14)],
                    vec!["Dépôt", "Ouvert"],
                )
            } else {
                (
                    vec![
                        Constraint::Min(10),
                        Constraint::Length(6),
                        Constraint::Length(6),
                        Constraint::Length(14),
                    ],
                    vec!["Dépôt", "PR", "★", "Mis à jour"],
                )
            };
            let table = Table::new(table_rows, widths)
                .header(Row::new(header).style(Style::new().bold()))
                .block(block.title_bottom(
                    Line::from(format!(" {}/{} ", selected + 1, list.len())).right_aligned(),
                ))
                .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            let mut ts = TableState::default().with_offset(h.list_offset.get());
            ts.select(Some(selected));
            f.render_stateful_widget(table, rows[0], &mut ts);
            h.list_offset.set(ts.offset());
        }
    }

    // Pied de page : la description du dépôt sélectionné (ou l'erreur), puis l'aide.
    let description = list
        .get(selected)
        .and_then(|r| r.info.as_ref())
        .map(|i| i.description.clone())
        .unwrap_or_default();
    let first = match &app.error {
        Some(e) => Line::from(Span::styled(
            format!("erreur : {e}"),
            Style::new().fg(Color::Red),
        )),
        None => Line::from(Span::styled(description, Style::new().fg(Color::DarkGray))),
    };
    let help = Line::from(Span::styled(
        home_help(area.width as usize, h.section, h.editing),
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(vec![first, help]), rows[1]);
}

fn draw_repo(f: &mut Frame, app: &App, r: &RepoView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).split(area);
    let now = timeago::now_epoch();
    let i = &r.info;
    let mut title = vec![Span::raw(format!(" {} ", i.repo.full_name()))];
    title.extend(repo_tags(i));
    title.push(Span::raw(" "));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title))
        .border_style(Style::new().fg(Color::Yellow));

    let dim = Style::new().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(if i.description.is_empty() {
        Line::from(Span::styled("(pas de description)", dim))
    } else {
        Line::from(i.description.clone())
    });
    // Ce qu'on sait déjà du dépôt, sans requête de plus.
    let mut facts: Vec<String> = Vec::new();
    if !i.default_branch.is_empty() {
        facts.push(format!("branche {}", i.default_branch));
    }
    if i.stars > 0 {
        facts.push(format!("★ {}", i.stars));
    }
    if i.open_prs > 0 {
        facts.push(format!("{} PR ouvertes", i.open_prs));
    }
    if i.open_issues > 0 {
        facts.push(format!("{} tickets", i.open_issues));
    }
    if let Some(u) = i
        .updated
        .as_deref()
        .map(|u| timeago::ago_rfc3339(now, u))
        .filter(|u| !u.is_empty())
    {
        facts.push(format!("mis à jour {u}"));
    }
    lines.push(Line::from(Span::styled(facts.join(" · "), dim)));
    lines.push(Line::from(""));
    for (n, name) in RepoView::ITEMS.iter().enumerate() {
        let summary: Vec<Span> = if n == 0 {
            match &r.last_run {
                LastRun::Loading => vec![Span::styled("…", dim)],
                LastRun::None => vec![Span::styled("aucun run", dim)],
                LastRun::Failed(e) => vec![Span::styled(format!("indisponible ({e})"), dim)],
                LastRun::Ready(p) => {
                    let (label, color) = status_style(p.status);
                    vec![
                        Span::styled(label, Style::new().fg(color)),
                        Span::styled(format!("  {} · {}", p.workflow, p.branch), dim),
                    ]
                }
            }
        } else if i.empty {
            vec![Span::styled("dépôt vide", dim)]
        } else if n == 2 {
            vec![Span::styled(
                format!("historique de {}", i.default_branch),
                dim,
            )]
        } else if n == 3 {
            vec![Span::styled(
                "toutes les branches, avec leur dernier commit",
                dim,
            )]
        } else if n == 4 {
            vec![Span::styled(
                match i.open_prs {
                    0 => "aucune ouverte".to_owned(),
                    1 => "1 ouverte".to_owned(),
                    n => format!("{n} ouvertes"),
                },
                dim,
            )]
        } else {
            vec![Span::styled(format!("branche {}", i.default_branch), dim)]
        };
        let marker = if n == r.selected { "▸ " } else { "  " };
        let style = if n == r.selected {
            Style::new().add_modifier(Modifier::REVERSED).bold()
        } else {
            Style::new().bold()
        };
        let mut spans = vec![
            Span::styled(format!("{marker}{name:<13}"), style),
            Span::raw("  "),
        ];
        spans.extend(summary);
        lines.push(Line::from(spans));
    }
    if let Some(e) = &r.info_error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("erreur : {e}"),
            Style::new().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines).block(block), rows[0]);
    draw_footer(f, app, rows[1], repo_help(area.width as usize), None);
}

fn draw_runs(f: &mut Frame, app: &App, r: &RunsView, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).split(area);
    // Le titre rappelle les filtres actifs.
    let mut title = String::from(" Runs");
    if let Some(label) = r.status.label() {
        title.push_str(&format!(" · {label}"));
    }
    if !r.filter.trim().is_empty() {
        title.push_str(&format!(" · « {} »", r.filter.trim()));
    }
    title.push(' ');
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::new().fg(Color::Yellow));
    let inner = block.inner(rows[0]);
    app.viewport.set((inner.height as usize).saturating_sub(1));
    let visible = r.visible();
    if visible.is_empty() {
        let (text, color) = if r.loading {
            ("chargement…", Color::Cyan)
        } else if r.pipelines.is_empty() {
            ("(aucun run)", Color::DarkGray)
        } else {
            (
                "(aucun run ne correspond au filtre · Échap l'efface)",
                Color::DarkGray,
            )
        };
        let line = Line::from(Span::styled(text, Style::new().fg(color)));
        f.render_widget(Paragraph::new(line).block(block), rows[0]);
    } else {
        // Les jobs d'un même run se suivent : le run n'est écrit qu'une fois, ses jobs reliés.
        let positions = run_positions(&visible);
        let dim = Style::new().fg(Color::DarkGray);
        let table_rows: Vec<Row> = visible
            .iter()
            .zip(&positions)
            .map(|(p, pos)| {
                let (label, color) = status_style(p.status);
                let branch = match pos {
                    RunPos::Single => "",
                    RunPos::First => "┌ ",
                    RunPos::Middle => "├ ",
                    RunPos::Last => "└ ",
                };
                let job = Cell::from(Line::from(vec![
                    Span::styled(branch, dim),
                    Span::raw(p.job.clone()),
                ]));
                let status = Cell::from(Span::styled(label, Style::new().fg(color)));
                if matches!(pos, RunPos::Middle | RunPos::Last) {
                    return Row::new(vec![
                        status,
                        Cell::from(""),
                        Cell::from(""),
                        job,
                        Cell::from(""),
                        Cell::from(""),
                        Cell::from(""),
                    ]);
                }
                Row::new(vec![
                    status,
                    Cell::from(Span::styled(format!("#{}", p.number), dim)),
                    Cell::from(p.workflow.clone()),
                    job,
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
                Constraint::Length(6),
                Constraint::Length(14),
                Constraint::Max(20),
                Constraint::Length(14),
                Constraint::Length(8),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new([
                "Statut", "Run", "Workflow", "Job", "Branche", "Commit", "Titre",
            ])
            .style(Style::new().bold()),
        )
        .block(
            block.title_bottom(
                Line::from(if r.filter_active() {
                    format!(
                        " {}/{} (sur {}) ",
                        r.selected + 1,
                        visible.len(),
                        r.pipelines.len()
                    )
                } else {
                    format!(" {}/{} ", r.selected + 1, visible.len())
                })
                .right_aligned(),
            ),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        let mut ts = TableState::default().with_offset(r.list_offset.get());
        ts.select(Some(r.selected));
        f.render_stateful_widget(table, rows[0], &mut ts);
        r.list_offset.set(ts.offset());
    }

    let first = if r.editing {
        Line::from(Span::styled(
            format!("filtrer : {}▏", r.filter),
            Style::new().fg(Color::Yellow),
        ))
    } else if let Some(err) = app.error.as_ref().or(r.error.as_ref()) {
        Line::from(Span::styled(
            format!("erreur : {err}"),
            Style::new().fg(Color::Red),
        ))
    } else if let Some(url) = r.selected_pipeline().and_then(|p| p.url.as_deref()) {
        Line::from(format!("run : {url}"))
    } else {
        Line::from("")
    };
    let help = Line::from(Span::styled(
        if r.editing {
            "Entrée valider · Échap annuler · le filtre cherche dans le workflow, le job, la branche, le commit et le titre"
        } else {
            runs_help(area.width as usize)
        },
        Style::new().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(vec![first, help]), rows[1]);
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

fn draw_login(f: &mut Frame, form: &LoginForm, quits: bool) {
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
        if quits {
            "Entrée valider · Tab changer de champ · Esc quitter"
        } else {
            "Entrée valider · Tab changer de champ · Esc annuler"
        },
        Style::new().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Connexion · {} ", form.label))
        .border_style(Style::new().fg(Color::Yellow));
    f.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Msg, Page};
    use crate::logview::{LineKind, LogLine, LogState};
    use ratatui::{Terminal, backend::TestBackend};
    use smithy_core::{Pipeline, Repo, RepoSource};

    fn info_of(repo: &Repo) -> RepoInfo {
        RepoView::placeholder(repo.clone()).info
    }

    fn pipe(id: u64, workflow: &str, status: Status) -> Pipeline {
        Pipeline {
            id,
            number: id,
            job: "build".into(),
            workflow: workflow.into(),
            title: format!("titre {id}"),
            branch: "main".into(),
            commit: "2f9ec647244c".into(),
            status,
            url: Some(format!("http://x/runs/{id}")),
        }
    }

    /// App sur la page des runs de `smithy/demo` : accueil → dépôt → CI, comme à l'usage.
    fn app_on_runs(pipelines: Vec<Pipeline>) -> App {
        let mut app = App::new();
        app.set_caps("", true, true);
        app.take_requests();
        let repo = Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        };
        app.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info_of(&repo)]),
        });
        app.home_enter();
        app.repo_enter();
        app.apply(Msg::Pipelines {
            repo,
            result: Ok(pipelines),
        });
        app
    }

    #[test]
    fn page_des_runs() {
        let app = app_on_runs(vec![Pipeline {
            number: 2,
            job: "build".into(),
            ..pipe(1, "fail.yml", Status::Failure)
        }]);
        let text = render(&app, 100, 12);
        for needle in [
            "CI",
            "Runs",
            "fail.yml",
            "failure",
            "2f9ec64",
            "titre 1",
            "run : http://x/runs/1",
            "1/1",
        ] {
            assert!(text.contains(needle), "absent du rendu : {needle}\n{text}");
        }
        assert!(!text.contains("Mes dépôts"), "l'accueil n'est plus affiché");
    }

    #[test]
    fn les_jobs_d_un_run_sont_regroupes() {
        let job = |id: u64, name: &str| Pipeline {
            number: 7,
            job: name.into(),
            ..pipe(id, "build.yml", Status::Success)
        };
        // Comme l'API : ids décroissants ; un autre run d'un seul job en dessous
        let app = app_on_runs(vec![
            job(14, "deploy"),
            job(13, "test"),
            job(12, "build"),
            Pipeline {
                number: 6,
                job: "lint".into(),
                ..pipe(11, "build.yml", Status::Failure)
            },
        ]);
        let width = 130;
        let text = render(&app, width, 14);
        let lines: Vec<String> = text
            .chars()
            .collect::<Vec<_>>()
            .chunks(width as usize)
            .map(|c| c.iter().collect::<String>())
            .collect();
        let row = |needle: &str| {
            lines
                .iter()
                .find(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} absent :\n{}", lines.join("\n")))
                .clone()
        };
        assert!(row("Job").contains("Run"));
        // Première ligne du run : tout ; les suivantes : le statut et le job seulement
        let first = row("┌ build");
        assert!(first.contains("#7") && first.contains("build.yml") && first.contains("titre 12"));
        let middle = row("├ test");
        assert!(
            !middle.contains("build.yml") && !middle.contains("#7") && !middle.contains("titre")
        );
        let last = row("└ deploy");
        assert!(!last.contains("build.yml"));
        // Un run d'un seul job : pas de lien
        let single = row(" lint");
        assert!(single.contains("#6") && !single.contains('┌') && single.contains("failure"));
    }

    #[test]
    fn le_pied_de_page_suit_le_run_selectionne() {
        let mut app = app_on_runs(vec![
            pipe(3, "ci.yml", Status::Success),
            pipe(2, "ci.yml", Status::Success),
        ]);
        app.list_move(1);
        let text = render(&app, 100, 12);
        assert!(text.contains("run : http://x/runs/2"));
        assert!(!text.contains("run : http://x/runs/3"));
    }

    #[test]
    fn page_des_runs_etats() {
        let mut app = app_on_runs(vec![]);
        assert!(render(&app, 80, 10).contains("(aucun run)"));
        // Premier chargement : la liste est encore vide
        let loading = app_on_runs(vec![]);
        let mut loading = loading;
        if let Some(Page::Runs(r)) = loading.stack.last_mut() {
            r.loading = true;
        }
        assert!(render(&loading, 80, 10).contains("chargement"));
        // Erreur de rafraîchissement : la liste reste, l'erreur apparaît en pied de page
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            result: Ok(vec![pipe(1, "ok.yml", Status::Success)]),
        });
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            result: Err("coupé".into()),
        });
        let t = render(&app, 100, 10);
        assert!(t.contains("ok.yml") && t.contains("erreur : coupé"), "{t}");
        for (w, h) in [(1, 1), (10, 3), (30, 5)] {
            render(&app, w, h);
        }
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
        let mut app = app_on_runs(vec![Pipeline {
            number: 3,
            job: "test".into(),
            ..pipe(1, "fail.yml", Status::Failure)
        }]);
        app.open_logs();
        app.logs_mut().unwrap().state = state;
        app
    }

    fn line(kind: LineKind, text: &str) -> LogLine {
        LogLine {
            kind,
            text: text.into(),
            step: None,
            runs: Vec::new(),
            hang: String::new(),
            words: false,
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
        // Le rendu publie la hauteur utile : 12 - 1 fil d'Ariane - 2 bordures - 1 pied.
        let _ = render(&app, 80, 12);
        assert_eq!(app.viewport.get(), 8);
        app.text_scroll(20);
        let text = render(&app, 80, 12);
        assert!(text.contains("ligne-21") && text.contains("ligne-28"));
        assert!(!text.contains("ligne-20") && !text.contains("ligne-29"));
        assert!(text.contains("21-28/50"));
    }

    fn app_with_job(steps: Vec<smithy_core::StepLog>) -> App {
        let mut app = app_with_logs(LogState::Loading);
        let v = app.logs_mut().unwrap();
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
        app.set_caps("", app.logs_supported, true);
        app.take_requests();
        app.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(vec![info_of(&tree_repo())]),
        });
        app.home_enter();
        app.list_move(1); // « Code »
        app.repo_enter();
        app
    }

    fn feed_dir(app: &mut App, path: &str, entries: Vec<Entry>) {
        let b = app.browser_mut().unwrap();
        b.on_branches(
            &tree_repo(),
            Ok(vec![
                Branch::new("main", true),
                Branch::new("feature/x", false),
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
        app.browser().unwrap()
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
            "smithy/tree › Code › main",
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
            render(&app, 100, 10).contains("Code › …"),
            "branche inconnue pendant le chargement"
        );
        feed_dir(&mut app, "", vec![]);
        assert!(render(&app, 100, 10).contains("(dossier vide)"));
        app.browser_mut().unwrap().empty_repo = true;
        assert!(render(&app, 100, 10).contains("dépôt vide"));
        let b = app.browser_mut().unwrap();
        b.on_dir(
            &tree_repo(),
            "main",
            "",
            Err(smithy_core::Error::NotFound("x".into())),
        );
        assert!(render(&app, 100, 10).contains("erreur"));
        // Message d'information (lien symbolique) : en pied de page à la place de l'aide
        app.browser_mut().unwrap().info = Some("lien : lien symbolique, non ouvert".into());
        let t = render(&app, 100, 10);
        assert!(t.contains("non ouvert") && !t.contains("B branche"));
    }

    #[test]
    fn explorateur_affiche_le_dossier_courant() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("src", EntryKind::Dir, 0)]);
        {
            let b = app.browser_mut().unwrap();
            b.path = "src/deep".into();
            b.on_dir(
                &tree_repo(),
                "main",
                "src/deep",
                Ok(vec![e("x.rs", EntryKind::File, 3)]),
            );
        }
        let t = render(&app, 100, 10);
        assert!(t.contains("Code › main › src › deep"), "{t}");
        assert!(t.contains("/src/deep"), "le cadre montre le chemin");
    }

    #[test]
    fn explorateur_selecteur_de_branche() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("a.txt", EntryKind::File, 1)]);
        app.browser_mut().unwrap().open_branches();
        let t = render(&app, 100, 14);
        assert!(
            t.contains("Branche") && t.contains("● main (défaut)") && t.contains("feature/x"),
            "{t}"
        );
        assert!(t.contains("j/k choisir"));
        // Liste pas encore chargée, puis en erreur
        app.browser_mut().unwrap().branches = None;
        assert!(render(&app, 100, 14).contains("chargement"));
        app.browser_mut().unwrap().branches_error = Some("coupé".into());
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
            app.browser_mut().unwrap().move_selection(1);
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
        app.browser_mut().unwrap().move_selection(-1);
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
            let b = app.browser_mut().unwrap();
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
            let b = app.browser_mut().unwrap();
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

    // --- accueil

    fn recent_ts(seconds_ago: i64) -> String {
        // RFC 3339 en UTC, reconstruit à partir de l'heure courante.
        let t = timeago::now_epoch() - seconds_ago;
        let days = t.div_euclid(86_400);
        let rem = t.rem_euclid(86_400);
        // jours depuis 1970 -> date civile (algorithme de H. Hinnant, sens inverse)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    fn info_full(name: &str) -> RepoInfo {
        let mut i = info_of(&Repo {
            owner: "smithy".into(),
            name: name.into(),
        });
        i.description = format!("description de {name}");
        i.default_branch = "main".into();
        i
    }

    fn home_app(infos: Vec<RepoInfo>) -> App {
        let mut app = App::new();
        app.take_requests();
        app.server_name = "maison".into();
        app.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Ok(infos),
        });
        app
    }

    #[test]
    fn accueil_liste_les_depots_avec_marques_et_dates() {
        let mut demo = info_full("demo");
        demo.private = true;
        demo.open_prs = 2;
        demo.stars = 3;
        demo.updated = Some(recent_ts(2 * 3600));
        let mut vieux = info_full("vieux");
        vieux.archived = true;
        vieux.fork = true;
        vieux.empty = true;
        let app = home_app(vec![demo, vieux]);
        let t = render(&app, 110, 12);
        for needle in [
            "maison › Accueil",
            "Mes dépôts",
            "Suivis",
            "Étoilés",
            "Explorer",
            "Récents",
            "Dépôt",
            "PR",
            "Mis à jour",
            "smithy/demo",
            "[privé]",
            "2 PR",
            "★ 3",
            "il y a 2 h",
            "smithy/vieux",
            "[archivé]",
            "[fork]",
            "[vide]",
            "1/2",
            "description de demo",
        ] {
            assert!(t.contains(needle), "absent : {needle}\n{t}");
        }
        assert!(
            !t.contains("description de vieux"),
            "seule la description de la sélection est montrée"
        );
        let mut moved = home_app(vec![info_full("a"), info_full("b")]);
        moved.list_move(1);
        let t = render(&moved, 110, 12);
        assert!(t.contains("description de b") && t.contains("2/2"));
    }

    #[test]
    fn accueil_sections_en_video_inverse() {
        let app = home_app(vec![info_full("a")]);
        let mut term = Terminal::new(TestBackend::new(100, 8)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        // Ligne 1 : le cadre et ses sections. Seule la section courante est inversée.
        let row =
            |y: u16| -> Vec<&ratatui::buffer::Cell> { (0..100).map(|x| &buf[(x, y)]).collect() };
        let reversed: String = row(1)
            .iter()
            .filter(|c| c.modifier.contains(Modifier::REVERSED))
            .map(|c| c.symbol())
            .collect();
        assert_eq!(reversed.trim(), "Mes dépôts", "{reversed:?}");
    }

    #[test]
    fn accueil_etats_vides_et_erreurs() {
        // Chargement initial
        let app = App::new();
        assert!(render(&app, 80, 10).contains("chargement"));
        // Échec
        let mut app = App::new();
        app.apply(Msg::Repos {
            source: RepoSource::Mine,
            result: Err(smithy_core::Error::Network("coupé".into())),
        });
        assert!(render(&app, 80, 10).contains("erreur : "));
        // Liste vide
        let app = home_app(vec![]);
        assert!(render(&app, 80, 10).contains("(aucun dépôt)"));
        // Filtre sans résultat
        let mut app = home_app(vec![info_full("a")]);
        app.home_mut().unwrap().start_edit();
        "zzz"
            .chars()
            .for_each(|c| app.home_mut().unwrap().type_char(c));
        let t = render(&app, 90, 10);
        assert!(
            t.contains("aucun dépôt ne correspond à « zzz »") && t.contains("filtrer : zzz▏"),
            "{t}"
        );
        assert!(
            t.contains("Entrée valider"),
            "l'aide change pendant la saisie"
        );
        // « Explorer » : invitation à chercher, puis résultat vide
        let mut app = home_app(vec![]);
        app.home_switch(3);
        assert!(render(&app, 90, 10).contains("Tapez /"));
        app.home_mut().unwrap().start_edit();
        "rien"
            .chars()
            .for_each(|c| app.home_mut().unwrap().type_char(c));
        assert!(render(&app, 90, 10).contains("chercher : rien▏"));
        app.home_commit();
        app.apply(Msg::Repos {
            source: RepoSource::Search("rien".into()),
            result: Ok(vec![]),
        });
        assert!(render(&app, 90, 10).contains("(aucun résultat pour « rien »)"));
        // « Récents » vide
        let mut app = home_app(vec![]);
        app.home_switch(4);
        assert!(render(&app, 90, 10).contains("(aucun dépôt ouvert récemment)"));
    }

    #[test]
    fn accueil_recents_avec_date_d_ouverture() {
        let mut app = home_app(vec![]);
        app.recents = vec![crate::recent::RecentEntry {
            server: "maison".into(),
            owner: "smithy".into(),
            name: "tree".into(),
            opened: timeago::now_epoch() - 3 * 86_400,
        }];
        app.home_switch(4);
        let t = render(&app, 90, 10);
        assert!(
            t.contains("Ouvert") && t.contains("smithy/tree") && t.contains("il y a 3 j"),
            "{t}"
        );
        assert!(!t.contains("Mis à jour"), "colonnes propres aux récents");
    }

    #[test]
    fn accueil_long_defile_sans_sauter() {
        let many: Vec<_> = (0..300).map(|i| info_full(&format!("r{i:03}"))).collect();
        let mut app = home_app(many);
        for _ in 0..200 {
            app.list_move(1);
            render(&app, 90, 14);
        }
        let t = render(&app, 90, 14);
        assert!(
            t.contains("r200") && !t.contains("r000") && t.contains("201/300"),
            "{t}"
        );
        let before = app.home().unwrap().list_offset.get();
        app.list_move(-1);
        render(&app, 90, 14);
        assert_eq!(
            app.home().unwrap().list_offset.get(),
            before,
            "remonter d'une ligne ne fait pas sauter la liste"
        );
    }

    #[test]
    fn accueil_et_depot_tres_petits_terminaux() {
        let mut app = home_app(vec![info_full("a")]);
        for (w, h) in [(1, 1), (5, 2), (12, 4), (30, 6)] {
            render(&app, w, h);
        }
        app.home_enter();
        for (w, h) in [(1, 1), (5, 2), (12, 4), (30, 6)] {
            render(&app, w, h);
        }
    }

    // --- page d'un dépôt

    fn repo_app(mut info: RepoInfo) -> App {
        info.description = "Outil de CI".into();
        let mut app = home_app(vec![info]);
        app.home_enter();
        app.take_requests();
        app
    }

    #[test]
    fn page_du_depot() {
        let mut i = info_full("demo");
        i.private = true;
        i.stars = 4;
        i.open_prs = 3;
        i.open_issues = 7;
        i.updated = Some(recent_ts(5 * 60));
        let mut app = repo_app(i);
        let t = render(&app, 100, 12);
        for needle in [
            "maison › Accueil › smithy/demo",
            "smithy/demo",
            "[privé]",
            "Outil de CI",
            "branche main",
            "★ 4",
            "3 PR ouvertes",
            "7 tickets",
            "mis à jour il y a 5 min",
            "▸ CI",
            "Code",
        ] {
            assert!(t.contains(needle), "absent : {needle}\n{t}");
        }
        assert!(t.contains("…"), "dernier run en cours de chargement");
        // Le dernier run, une fois reçu
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            result: Ok(vec![pipe(9, "fail.yml", Status::Failure)]),
        });
        let t = render(&app, 100, 12);
        assert!(
            t.contains("✘ failure") && t.contains("fail.yml · main"),
            "{t}"
        );
        // Sélection sur « Code »
        app.list_move(1);
        let t = render(&app, 100, 12);
        assert!(t.contains("▸ Code") && !t.contains("▸ CI"), "{t}");
        assert!(t.contains("branche main"));
    }

    #[test]
    fn page_du_depot_cas_limites() {
        // Sans description ni fiche, aucun run, dépôt vide, erreurs
        let mut app = repo_app(
            RepoView::placeholder(Repo {
                owner: "smithy".into(),
                name: "vide".into(),
            })
            .info,
        );
        app.repo_page_info_mut(|i| {
            i.description.clear();
            i.empty = true;
        });
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "vide".into(),
            },
            result: Ok(vec![]),
        });
        let t = render(&app, 100, 12);
        assert!(
            t.contains("(pas de description)")
                && t.contains("aucun run")
                && t.contains("dépôt vide"),
            "{t}"
        );
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "vide".into(),
            },
            result: Err("coupé".into()),
        });
        assert!(render(&app, 100, 12).contains("indisponible (coupé)"));
        app.apply(Msg::RepoInfo {
            repo: Repo {
                owner: "smithy".into(),
                name: "vide".into(),
            },
            result: Err(smithy_core::Error::NotFound("vide".into())),
        });
        assert!(render(&app, 100, 14).contains("erreur : "));
    }

    #[test]
    fn aides_de_la_page_du_depot_et_des_runs() {
        for w in [120, 80, 50, 30, 12, 5, 0] {
            for h in [repo_help(w), runs_help(w)] {
                assert!(h.chars().count() <= w.max(5), "{h:?} dépasse {w}");
                assert!(
                    h.contains("Échap") || h.contains('←'),
                    "le retour reste indiqué : {h:?}"
                );
            }
        }
        assert!(repo_help(120).contains("↵/→ ouvrir") && runs_help(120).contains("logs"));
    }

    // --- onglets

    fn tt(text: &str, marker: Option<TabMarker>, active: bool) -> TabTitle {
        TabTitle {
            server: String::new(),
            text: text.into(),
            marker,
            active,
        }
    }

    fn many_titles(n: usize, active: usize) -> Vec<TabTitle> {
        (0..n)
            .map(|i| tt(&format!("onglet-{i}"), None, i == active))
            .collect()
    }

    fn bar_width(bar: &TabBar) -> usize {
        let cells: usize =
            bar.cells.iter().map(cell_width).sum::<usize>() + bar.cells.len().saturating_sub(1);
        cells + 2 * usize::from(bar.more_left) + 2 * usize::from(bar.more_right)
    }

    #[test]
    fn titres_d_onglets_tronques() {
        assert_eq!(truncate_end("court", 20), "court");
        assert_eq!(
            truncate_end("exactement-vingt-cars", 21),
            "exactement-vingt-cars"
        );
        let t = truncate_end("un-titre-beaucoup-trop-long-pour-la-barre", 20);
        assert!(t.ends_with('…') && text_width(&t) <= 20, "{t}");
        let cjk = truncate_end("日本語日本語日本語日本語日本語", 9);
        assert!(
            text_width(&cjk) <= 9 && cjk.ends_with('…'),
            "{cjk} = {} colonnes",
            text_width(&cjk)
        );
        assert_eq!(truncate_end("", 5), "");
    }

    #[test]
    fn barre_d_onglets_qui_tient() {
        let bar = fit_tabs(&many_titles(3, 1), 80);
        assert_eq!(bar.cells.len(), 3);
        assert!(!bar.more_left && !bar.more_right);
        assert!(bar.cells[1].active && !bar.cells[0].active);
    }

    #[test]
    fn barre_d_onglets_autour_de_l_onglet_affiche() {
        // 10 onglets de 10 colonnes ne tiennent pas dans 45 : on garde ceux qui entourent l'actif.
        for active in [0usize, 4, 9] {
            let bar = fit_tabs(&many_titles(10, active), 45);
            assert!(
                bar.cells.iter().any(|c| c.active),
                "l'onglet affiché est toujours visible (actif {active})"
            );
            assert!(
                bar_width(&bar) <= 45,
                "actif {active} : {} colonnes",
                bar_width(&bar)
            );
            assert!(bar.cells.len() >= 3 && bar.cells.len() < 10);
            assert_eq!(
                bar.more_left,
                active > 0 && bar.cells[0].label != "onglet-0"
            );
            assert_eq!(
                bar.more_right,
                active < 9 && bar.cells.last().unwrap().label != "onglet-9"
            );
        }
        let first = fit_tabs(&many_titles(10, 0), 45);
        assert!(!first.more_left && first.more_right);
        let last = fit_tabs(&many_titles(10, 9), 45);
        assert!(last.more_left && !last.more_right);
        let middle = fit_tabs(&many_titles(10, 5), 45);
        assert!(middle.more_left && middle.more_right);
    }

    #[test]
    fn barre_d_onglets_cas_limites() {
        // Largeur minuscule : au moins l'onglet affiché
        let bar = fit_tabs(&many_titles(5, 2), 3);
        assert_eq!(bar.cells.len(), 1);
        assert!(bar.cells[0].active);
        // Les marqueurs prennent 2 colonnes de plus
        let with = fit_tabs(&[tt("a", Some(TabMarker::Running), true)], 80);
        let without = fit_tabs(&[tt("a", None, true)], 80);
        assert_eq!(
            cell_width(&with.cells[0]),
            cell_width(&without.cells[0]) + 2
        );
        // Aucun onglet affiché (ne devrait pas arriver) ou liste vide : rien, sans panique
        assert!(fit_tabs(&[], 80).cells.is_empty());
        assert!(fit_tabs(&[tt("x", None, false)], 80).cells.is_empty());
    }

    #[test]
    fn barre_d_onglets_a_l_ecran() {
        // Un seul onglet : pas de barre, le fil d'Ariane est en première ligne
        let app = home_app(vec![info_full("a")]);
        let one = render(&app, 90, 8);
        assert!(one.starts_with("maison › Accueil"), "{one}");
        // Deux onglets : la barre en haut, le fil d'Ariane dessous
        let mut app = home_app(vec![info_full("a")]);
        app.open_in_new_tab();
        let two = render(&app, 90, 8);
        assert!(two.trim_start().starts_with("Accueil"), "{two}");
        assert!(
            two.contains(" a ") && two.contains("maison › Accueil › smithy/a"),
            "{two}"
        );
        assert!(two.contains('│'), "séparateur entre onglets");
        // Des marqueurs d'état dans les onglets
        let mut app = app_on_runs(vec![pipe(1, "ok.yml", Status::Running)]);
        app.open_logs();
        app.open_in_new_tab();
        let t = render(&app, 100, 10);
        assert!(t.contains("● "), "job en cours : {t}");
        app.apply(Msg::Pipelines {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            result: Ok(vec![pipe(1, "ok.yml", Status::Failure)]),
        });
        assert!(render(&app, 100, 10).contains("✘ "), "job terminé en échec");
        // Très petits terminaux
        for (w, h) in [(1, 1), (6, 3), (20, 5)] {
            render(&app, w, h);
        }
    }

    #[test]
    fn la_barre_marque_l_onglet_affiche_en_video_inverse() {
        let mut app = home_app(vec![info_full("a")]);
        app.open_in_new_tab();
        let mut term = Terminal::new(TestBackend::new(60, 6)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        let reversed: String = (0..60)
            .map(|x| &buf[(x, 0)])
            .filter(|c| c.modifier.contains(Modifier::REVERSED))
            .map(|c| c.symbol())
            .collect();
        assert_eq!(
            reversed.trim(),
            "a",
            "seul l'onglet affiché est inversé : {reversed:?}"
        );
    }

    #[test]
    fn liste_des_onglets_a_l_ecran() {
        let mut app = app_on_runs(vec![pipe(1, "ok.yml", Status::Running)]);
        app.open_logs();
        app.open_in_new_tab();
        app.open_tab_picker();
        let t = render(&app, 100, 14);
        for needle in ["Onglets", "1. ", "2. ", "(affiché)", "↵ aller", "x fermer"] {
            assert!(t.contains(needle), "absent : {needle}\n{t}");
        }
        assert!(t.contains("demo · build"), "{t}");
        assert!(
            t.contains("●"),
            "le marqueur du job en cours est aussi dans la liste"
        );
        for (w, h) in [(1, 1), (10, 4), (30, 6)] {
            render(&app, w, h);
        }
    }

    #[test]
    fn aide_de_l_accueil_adaptee_a_la_largeur() {
        // 80 colonnes : « ↵ ouvrir », « / filtrer » et « q quitter » doivent rester visibles.
        let h = home_help(80, Section::Mine, false);
        assert!(
            h.contains("↵ ouvrir") && h.contains("/ filtrer") && h.contains("q quitter"),
            "{h}"
        );
        assert!(home_help(120, Section::Mine, false).contains("r actualiser"));
        assert!(home_help(90, Section::Explore, false).contains("/ chercher"));
        assert!(home_help(90, Section::Mine, true).contains("Entrée valider"));
        for section in Section::ALL {
            for editing in [false, true] {
                for w in [100, 80, 60, 40, 20, 8, 3, 0] {
                    let h = home_help(w, section, editing);
                    assert!(h.chars().count() <= w.max(8), "{h:?} dépasse {w}");
                    assert!(
                        h.contains('q') || h.contains("Échap"),
                        "une sortie reste indiquée : {h:?}"
                    );
                }
            }
        }
        // Rendu réel à 80 colonnes
        let app = App::new();
        assert!(render(&app, 80, 10).contains("q quitter"));
    }

    #[test]
    fn explorateur_fichier() {
        let mut app = explorer_app();
        feed_dir(&mut app, "", vec![e("a.txt", EntryKind::File, 30)]);
        app.browser_mut().unwrap().enter();
        assert!(render(&app, 100, 10).contains("chargement"));
        app.browser_mut().unwrap().on_file(
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
        app.browser_mut().unwrap().on_file(
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
        app.browser_mut().unwrap().open_branches();
        for (w, h) in [(1, 1), (8, 3), (20, 6)] {
            render(&app, w, h);
        }
    }

    #[test]
    fn aides_de_l_explorateur_adaptees_a_la_largeur() {
        for w in [120, 80, 50, 30, 12, 5, 0] {
            for h in [
                dir_help(w),
                file_help(w, false, false),
                file_help(w, true, true),
                file_help(w, true, false),
                branches_help(w),
            ] {
                assert!(h.chars().count() <= w.max(3), "{h:?} dépasse {w}");
                assert!(h.contains("Esc"));
            }
        }
        assert!(dir_help(120).contains("B branche") && dir_help(120).contains("remonter"));
        assert!(file_help(120, false, false).contains("w retour à la ligne"));
        assert!(!file_help(120, true, true).contains("w retour"));
        assert!(file_help(120, true, true).contains("m texte source"));
        assert!(file_help(120, true, false).contains("m rendu"));
    }

    // --- fil d'Ariane

    fn crumbs(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    fn shown(parts: &[&str], width: usize) -> String {
        fit_breadcrumb(&crumbs(parts), width).join(CRUMB_SEP)
    }

    #[test]
    fn fil_d_ariane_tient_dans_la_largeur() {
        let path = [
            "maison",
            "Accueil",
            "smithy/tree",
            "Code",
            "main",
            "src",
            "deep",
            "main.rs",
        ];
        // Assez large : tel quel
        assert_eq!(
            shown(&path, 200),
            "maison › Accueil › smithy/tree › Code › main › src › deep › main.rs"
        );
        // Trop long : le serveur reste au début, la page à la fin, le plus ancien du milieu part d'abord
        let s = shown(&path, 50);
        assert!(
            s.starts_with("maison › … › ") && s.ends_with("main.rs"),
            "{s}"
        );
        assert!(text_width(&s) <= 50, "{s}");
        // Plus étroit encore : serveur … page
        assert_eq!(shown(&path, 22), "maison › … › main.rs");
        // Même ainsi trop long : on ne garde que la fin, la partie la plus précise
        let s = shown(&path, 12);
        assert!(
            s.starts_with('…') && s.ends_with("main.rs") && text_width(&s) <= 12,
            "{s}"
        );
        // Cas limites
        assert_eq!(shown(&[], 10), "");
        assert_eq!(shown(&["seul"], 10), "seul");
        assert!(shown(&["maison", "Accueil"], 3).chars().count() <= 3);
        assert_eq!(shown(&path, 0), "");
        assert!(fit_breadcrumb(&crumbs(&path), 1).join("").chars().count() <= 1);
    }

    #[test]
    fn fil_d_ariane_largeur_en_colonnes_pas_en_caracteres() {
        // Chaque idéogramme occupe 2 colonnes.
        let s = shown(&["maison", "日本語日本語日本語", "fin"], 14);
        assert!(text_width(&s) <= 14, "{s} = {} colonnes", text_width(&s));
        assert!(s.ends_with("fin"), "{s}");
    }

    #[test]
    fn fil_d_ariane_affiche_a_l_ecran() {
        let mut app = App::new();
        app.server_name = "maison".into();
        let t = render(&app, 80, 10);
        assert!(t.contains("maison › Accueil"), "{t}");
        // Dans les logs : dépôt, CI, workflow, job
        let mut app = app_with_logs(LogState::Loading);
        app.server_name = "maison".into();
        let t = render(&app, 100, 10);
        assert!(
            t.contains("maison › Accueil › smithy/demo › CI › fail.yml › test"),
            "{t}"
        );
        // Dans l'explorateur : dépôt, Code, branche, dossiers
        let mut app = explorer_app();
        app.server_name = "maison".into();
        feed_dir(&mut app, "", vec![e("src", EntryKind::Dir, 0)]);
        {
            let b = app.browser_mut().unwrap();
            b.move_selection(0);
            b.enter();
            b.on_dir(
                &tree_repo(),
                "main",
                "src",
                Ok(vec![e("main.rs", EntryKind::File, 3)]),
            );
        }
        let t = render(&app, 100, 10);
        assert!(
            t.contains("maison › Accueil › smithy/tree › Code › main › src"),
            "{t}"
        );
        // Très petits terminaux : pas de panique
        for (w, h) in [(1, 1), (5, 2), (12, 3)] {
            render(&app, w, h);
        }
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
        app.logs_mut().unwrap().notice = Some("Forgejo 14.0.0 : version non vérifiée".into());
        let text = render(&app, 100, 10);
        assert!(text.contains("⚠") && text.contains("non vérifiée"));
        assert!(!render(&app_with_logs(LogState::Loading), 100, 10).contains("⚠"));
    }

    #[test]
    fn formulaire_par_dessus_les_logs() {
        let mut app = app_with_logs(LogState::Failed("session expirée".into()));
        app.open_login("", "thomas", false);
        let text = render(&app, 100, 20);
        assert!(text.contains("Connexion") && text.contains("CI"));
    }

    #[test]
    fn formulaire_de_connexion_masque_le_mot_de_passe() {
        let mut app = App::new();
        app.set_servers(vec![crate::app::ServerEntry {
            name: "maison".into(),
            url: "http://forge".into(),
            kind: Default::default(),
        }]);
        app.open_login("maison", "thomas", false);
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
        app.open_login("", "", false);
        render(&app, 10, 3);
    }

    #[test]
    fn rendu_sur_petit_terminal_et_etat_vide() {
        let mut term = Terminal::new(TestBackend::new(20, 4)).unwrap();
        term.draw(|f| draw(f, &App::new())).unwrap();
    }

    fn two_servers() -> App {
        let mut app = App::new();
        app.set_servers(
            ["A", "B"]
                .map(|n| crate::app::ServerEntry {
                    name: n.into(),
                    url: format!("http://{n}.example"),
                    kind: Default::default(),
                })
                .to_vec(),
        );
        app.set_initial_server("A");
        app.set_caps("A", true, true);
        app
    }

    #[test]
    fn titres_d_onglets_prefixes_du_serveur_seulement_avec_plusieurs_serveurs() {
        let mut app = two_servers();
        app.open_in_new_tab();
        app.switch_server("B");
        let text = render(&app, 100, 10);
        assert!(
            text.contains("A · Accueil") && text.contains("B · Accueil"),
            "{text}"
        );
        // Un seul serveur configuré : titres inchangés
        let mut solo = App::new();
        solo.open_in_new_tab();
        let text = render(&solo, 100, 10);
        assert!(
            text.contains("Accueil") && !text.contains(" · Accueil"),
            "{text}"
        );
    }

    #[test]
    fn liste_des_serveurs() {
        let mut app = two_servers();
        app.open_server_picker();
        let text = render(&app, 100, 14);
        assert!(text.contains("Serveurs"), "{text}");
        assert!(
            text.contains("● A") && text.contains("session ouverte"),
            "{text}"
        );
        assert!(
            text.contains("B [forgejo] (http://B.example)") && text.contains("pas de session"),
            "{text}"
        );
        // Réseau : vérification en cours, puis résultats distincts de la session
        assert!(text.contains("vérification…"), "{text}");
        let reach = |app: &mut App, n: &str, r: Result<Option<String>, String>| {
            let url = format!("http://{n}.example");
            app.apply_for(
                n,
                Msg::Reach {
                    server: n.into(),
                    url,
                    result: r,
                },
            );
        };
        reach(&mut app, "A", Ok(Some("15.0".into())));
        reach(&mut app, "B", Err("délai dépassé".into()));
        let text = render(&app, 120, 14);
        assert!(text.contains("joignable · session ouverte"), "{text}");
        assert!(
            text.contains("injoignable (délai dépassé) · pas de session"),
            "{text}"
        );
        // Une réponse pour une ancienne URL est ignorée
        app.apply_for(
            "B",
            Msg::Reach {
                server: "B".into(),
                url: "http://ancienne".into(),
                result: Ok(None),
            },
        );
        assert!(render(&app, 120, 14).contains("injoignable"));
        assert!(text.contains("↵ choisir"));
    }

    #[test]
    fn le_formulaire_dit_quitter_ou_annuler() {
        let mut app = two_servers();
        app.open_login("B", "", true);
        let text = render(&app, 100, 20);
        assert!(text.contains("Connexion · B (http://B.example)"), "{text}");
        assert!(text.contains("Esc annuler") && !text.contains("Esc quitter"));
        // Un renouvellement de session alors que A est connecté : Échap annule
        app.open_login("B", "", false);
        assert!(render(&app, 100, 20).contains("Esc annuler"));
        // Connexion de démarrage, aucun serveur connecté : Échap quitte
        app.forget_caps("A");
        assert!(render(&app, 100, 20).contains("Esc quitter"));
    }

    fn pull(n: u64) -> smithy_core::PullRequest {
        smithy_core::PullRequest {
            number: n,
            title: format!("titre n°{n}"),
            body: "Une **description**.".into(),
            author: "ana".into(),
            state: smithy_core::PullState::Open,
            draft: false,
            head: "feature/garde".into(),
            base: "main".into(),
            head_sha: "abc".into(),
            labels: vec![],
            stats: Some(smithy_core::PullStats {
                additions: 8,
                deletions: 2,
                changed_files: 4,
                comments: 2,
            }),
            mergeable: Some(true),
            created: None,
            updated: None,
            closed: None,
            url: None,
        }
    }

    #[test]
    fn liste_et_detail_des_pull_requests() {
        let repo = Repo {
            owner: "smithy".into(),
            name: "revue".into(),
        };
        let mut app = App::new();
        app.set_caps("", true, true);
        let (mut v, _) = PullsView::open(repo.clone(), PullFilter::Open);
        let mut p1 = pull(1);
        p1.labels = vec!["docs".into()];
        let mut p4 = pull(4);
        p4.mergeable = Some(false);
        let mut p5 = pull(5);
        p5.draft = true;
        p5.mergeable = None;
        v.apply_page(
            &repo,
            PullFilter::Open,
            1,
            Ok(smithy_core::PullPage {
                pulls: vec![p5, p4, p1],
                has_more: false,
            }),
        );
        app.stack.push(Page::Pulls(v));
        let text = render(&app, 150, 12);
        for want in [
            "Pull requests · ouvertes",
            "#5",
            "brouillon",
            "#4",
            "[conflits]",
            "titre n°1 [docs]",
            "feature/garde → main",
            "4 fichiers · +8 −2 · 2 commentaires",
            "s état",
            " 1/3 ",
        ] {
            assert!(text.contains(want), "{want:?} absent de {text}");
        }
        // Liste vide
        let (mut empty, _) = PullsView::open(repo.clone(), PullFilter::Closed);
        empty.apply_page(
            &repo,
            PullFilter::Closed,
            1,
            Ok(smithy_core::PullPage {
                pulls: vec![],
                has_more: false,
            }),
        );
        app.stack.push(Page::Pulls(empty));
        assert!(render(&app, 150, 12).contains("aucune pull request fermée"));
        app.stack.pop();

        // Détail : titre, en-tête, sections
        let (mut d, _) = PullView::open(repo.clone(), 1, "titre n°1".into());
        d.apply_detail(
            &repo,
            1,
            Ok(smithy_core::PullDetail {
                pull: pull(1),
                commits: vec![],
                commits_truncated: false,
                reviews: vec![smithy_core::Review {
                    author: "relecteur".into(),
                    state: smithy_core::ReviewState::Approved,
                    body: "Ça me va.".into(),
                    date: None,
                    dismissed: false,
                    stale: false,
                    comments: vec![],
                }],
                comments: None,
                checks: Some(Status::Failure),
                statuses: vec![],
                patch: "diff --git a/a.txt b/a.txt\n+x\n".into(),
                patch_truncated: false,
            }),
        );
        app.stack.push(Page::Pull(d));
        let text = render(&app, 150, 30);
        for want in [
            " #1 titre n°1 ",
            "État     : ouverte",
            "Branches : feature/garde → main",
            "CI       : ✘ échec",
            "Une description.",
            "Revues (1)",
            "✔ relecteur a approuvé",
            "Conversation (2, non affichée)",
            "a.txt  +1 −0",
            "e code",
        ] {
            assert!(text.contains(want), "{want:?} absent de {text}");
        }
    }

    #[test]
    fn premier_lancement_formulaire_impose() {
        let mut app = App::new();
        app.open_first_server_form();
        let text = render(&app, 100, 20);
        assert!(
            text.contains("premier serveur")
                && text.contains("Aucun serveur configuré")
                && text.contains("Esc quitter")
                && text.contains("https://"),
            "{text}"
        );
    }

    #[test]
    fn formulaire_et_confirmation_des_serveurs() {
        let mut app = two_servers();
        app.open_server_picker();
        let text = render(&app, 110, 20);
        assert!(
            text.contains("a ajouter")
                && text.contains("e modifier")
                && text.contains("d supprimer"),
            "{text}"
        );
        app.server_form_add();
        app.server_form_move(1); // Type -> Nom
        "dix".chars().for_each(|c| app.server_form_type(c));
        let text = render(&app, 110, 20);
        assert!(
            text.contains("Nouveau serveur") && text.contains("dix") && text.contains("https://"),
            "{text}"
        );
        app.server_form_fail("nom invalide".into());
        assert!(render(&app, 110, 20).contains("nom invalide"));
        app.server_form_escape();
        app.server_picker_move(1);
        app.server_form_edit();
        let text = render(&app, 110, 20);
        assert!(
            text.contains("Modifier le serveur · B") && text.contains("inchangé"),
            "{text}"
        );
        app.server_form_escape();
        app.server_delete_ask();
        assert!(render(&app, 110, 20).contains("Supprimer « B » ?"));
        app.server_delete_cancel();
        app.server_picker_move(-1);
        app.server_delete_ask();
        assert!(render(&app, 110, 20).contains("utilisé par un onglet"));
    }

    #[test]
    fn les_jetons_sont_peints_aux_couleurs_ansi() {
        use crate::highlight::{Run, Tok};
        let run = |start, end, tok| Run { start, end, tok };
        let app = app_with_logs(LogState::Ready(vec![LogLine {
            kind: LineKind::Text,
            text: "QQ x = \"ZZ\"; // WW".into(),
            step: None,
            runs: vec![
                run(0, 2, Tok::Keyword),
                run(7, 11, Tok::Str),
                run(13, 18, Tok::Comment),
            ],
            hang: String::new(),
            words: false,
        }]));
        app.viewport.set(5);
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let fg_of = |sym: &str| {
            buf.content()
                .iter()
                .find(|c| c.symbol() == sym)
                .map(|c| c.fg)
        };
        assert_eq!(fg_of("Q"), Some(Color::Magenta), "mot-clé");
        assert_eq!(fg_of("Z"), Some(Color::Green), "chaîne");
        assert_eq!(fg_of("W"), Some(Color::DarkGray), "commentaire");
        // Le texte sans région garde la couleur par défaut
        assert_eq!(fg_of("x"), Some(Color::Reset));
    }

    #[test]
    fn markdown_replie_par_mots_avec_retrait_et_barre() {
        use crate::markdown;
        let src = "- un deux trois quatre cinq six sept huit neuf dix onze douze\n\n> citation tres longue qui doit se replier correctement sur la ligne suivante\n";
        let lines: Vec<_> = markdown::render(src, false)
            .into_iter()
            .map(markdown::MdLine::into_log_line)
            .collect();
        let mut app = app_with_logs(LogState::Ready(lines));
        if let Some(v) = app.logs_mut() {
            v.wrap = true;
        }
        let text = render(&app, 40, 14);
        // Aucun mot n'est coupé : chaque mot du texte reste entier à l'écran
        for w in ["quatre", "douze", "citation", "replier", "suivante"] {
            assert!(text.contains(w), "{w} coupé : {text}");
        }
        // La puce n'est que sur la première ligne ; la suite est retirée (les 2 colonnes de la
        // puce sont des espaces) et la citation garde sa barre
        assert_eq!(text.matches('•').count(), 1, "{text}");
        assert!(
            text.matches('▎').count() >= 2,
            "la barre suit la citation : {text}"
        );
    }

    #[test]
    fn occurrences_surlignees_et_courante_en_jaune() {
        let mut app = app_with_logs(LogState::Ready(vec![LogLine {
            kind: LineKind::Text,
            text: "aaa ZQX bbb ZQX".into(),
            step: None,
            runs: Vec::new(),
            hang: String::new(),
            words: false,
        }]));
        app.viewport.set(5);
        app.viewport_width.set(40);
        app.text_search_begin();
        "ZQX".chars().for_each(|c| app.text_prompt_type(c));
        // La saisie est affichée dans le pied de page
        assert!(render(&app, 100, 10).contains("/ZQX▏"));
        app.text_prompt_commit();
        let mut term = Terminal::new(TestBackend::new(100, 10)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // Seulement le volet de texte (les premières lignes), pas le pied de page qui répète la requête
        let width = buf.area.width as usize;
        let cells: Vec<_> = buf
            .content()
            .iter()
            .enumerate()
            .filter(|(i, c)| c.symbol() == "Z" && (1..4).contains(&(i / width)))
            .map(|(_, c)| c)
            .collect();
        assert_eq!(cells.len(), 2, "deux occurrences à l'écran");
        // La première est la courante (jaune), l'autre en vidéo inverse
        assert_eq!(cells[0].bg, Color::Yellow);
        assert!(cells[1].modifier.contains(Modifier::REVERSED) && cells[1].bg != Color::Yellow);
        let b = buf.content().iter().find(|c| c.symbol() == "b").unwrap();
        assert!(
            !b.modifier.contains(Modifier::REVERSED),
            "hors occurrence : texte normal"
        );
        // Bilan dans le pied de page ; n passe à la suivante
        assert!(render(&app, 120, 10).contains("1/2"));
        app.text_search_step(1);
        let text = render(&app, 120, 10);
        assert!(
            text.contains("2/2") && text.contains("Échap efface"),
            "{text}"
        );
        app.text_escape();
        assert!(!render(&app, 120, 10).contains("2/2"));
    }

    #[test]
    fn liste_des_runs_filtree() {
        let mut app = app_on_runs(vec![
            pipe(1, "ci.yml", Status::Failure),
            pipe(2, "deploy.yml", Status::Success),
            pipe(3, "ci.yml", Status::Success),
        ]);
        let r = app.runs_mut().unwrap();
        r.start_edit();
        "deploy".chars().for_each(|c| r.type_char(c));
        // Saisie : le filtre est visible dans le pied de page et le titre
        let text = render(&app, 120, 14);
        assert!(
            text.contains("filtrer : deploy▏") && text.contains("« deploy »"),
            "{text}"
        );
        assert!(
            text.contains("deploy.yml") && !text.contains("ci.yml"),
            "{text}"
        );
        assert!(text.contains("1/1 (sur 3)"), "{text}");
        app.runs_mut().unwrap().commit();
        let r = app.runs_mut().unwrap();
        r.cycle_status();
        let text = render(&app, 120, 14);
        assert!(text.contains("échecs"), "{text}");
        assert!(text.contains("aucun run ne correspond"), "{text}");
        // Les filtres retirés, tout revient
        app.runs_clear_filters();
        let text = render(&app, 120, 14);
        assert!(
            text.contains("ci.yml") && text.contains("deploy.yml") && text.contains("1/3"),
            "{text}"
        );
    }

    #[test]
    fn dossier_filtre_a_l_ecran() {
        let mut app = App::new();
        app.set_caps("", true, true);
        let repo = Repo {
            owner: "o".into(),
            name: "r".into(),
        };
        let mut b = Browser::open(repo.clone());
        b.on_branches(&repo, Ok(vec![smithy_core::Branch::new("main", true)]));
        let e = |n: &str, k| smithy_core::Entry {
            name: n.into(),
            path: n.into(),
            kind: k,
            size: 5,
        };
        b.on_dir(
            &repo,
            "main",
            "",
            Ok(vec![
                e("alpha", EntryKind::Dir),
                e("notes.md", EntryKind::File),
                e("main.rs", EntryKind::File),
            ]),
        );
        app.stack.push(crate::app::Page::Browser(b));
        {
            let b = app.browser_mut().unwrap();
            b.start_filter();
            "RS".chars().for_each(|c| b.filter_type(c));
        }
        let text = render(&app, 110, 12);
        assert!(text.contains("filtrer : RS▏"), "{text}");
        assert!(
            text.contains("« RS »") && text.contains("1/1 (sur 3)"),
            "{text}"
        );
        assert!(
            text.contains("main.rs") && !text.contains("notes.md"),
            "{text}"
        );
        assert!(app.is_typing());
        app.browser_mut().unwrap().filter_commit();
        assert!(!app.is_typing());
        assert!(render(&app, 110, 12).contains("« RS »"));
        // Aucune entrée ne correspond
        let b = app.browser_mut().unwrap();
        b.start_filter();
        "zzz".chars().for_each(|c| b.filter_type(c));
        b.filter_commit();
        assert!(render(&app, 110, 12).contains("aucune entrée ne correspond"));
        app.browser_mut().unwrap().clear_filter();
        let text = render(&app, 110, 12);
        assert!(text.contains("notes.md") && text.contains("1/3"), "{text}");
    }

    // --- historique

    fn commit_n(n: usize, msg: &str) -> smithy_core::Commit {
        smithy_core::Commit {
            sha: format!("{n:07x}{}", "a".repeat(33)),
            message: msg.into(),
            author: "ana".into(),
            date: Some("2026-10-14T10:00:00Z".into()),
            url: Some(format!("http://x/commit/{n}")),
        }
    }

    fn on_repo_page() -> App {
        let mut app = app_on_runs(vec![]);
        app.pop(); // retour à la page du dépôt
        app
    }

    #[test]
    fn la_page_du_depot_propose_commits_et_branches() {
        let app = on_repo_page();
        let text = render(&app, 110, 16);
        for needle in [
            "CI",
            "Code",
            "Commits",
            "Branches",
            "historique de",
            "toutes les branches",
        ] {
            assert!(text.contains(needle), "absent : {needle}\n{text}");
        }
    }

    #[test]
    fn liste_des_commits_a_l_ecran() {
        let mut app = on_repo_page();
        app.list_move(2);
        app.repo_enter();
        app.apply(Msg::Branches {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            result: Ok(vec![
                smithy_core::Branch::new("main", true),
                smithy_core::Branch::new("feature/x", false),
            ]),
        });
        let text = render(&app, 120, 14);
        assert!(
            text.contains("Commits · main") && text.contains("chargement…"),
            "{text}"
        );
        app.apply(Msg::Commits {
            repo: Repo {
                owner: "smithy".into(),
                name: "demo".into(),
            },
            reference: String::new(),
            page: 1,
            result: Ok(smithy_core::CommitPage {
                commits: vec![
                    commit_n(1, "feat: une chose\n\ncorps"),
                    commit_n(2, "fix: autre chose"),
                ],
                has_more: true,
            }),
        });
        let text = render(&app, 120, 14);
        for needle in [
            "Commit",
            "Date",
            "Auteur",
            "Message",
            "0000001",
            "feat: une chose",
            "fix: autre chose",
            "ana",
            "1/2+",
        ] {
            assert!(text.contains(needle), "absent : {needle}\n{text}");
        }
        assert!(
            !text.contains("corps"),
            "seule la première ligne du message est listée"
        );
        // Liste courte avec une suite possible : la page suivante est déjà demandée
        assert!(text.contains("chargement de la suite…"), "{text}");
        // Filtre : saisie, titre, compteur
        {
            let v = app.commits_mut().unwrap();
            v.filter.start();
            "autre".chars().for_each(|c| {
                v.filter.type_char(c);
            });
        }
        let text = render(&app, 120, 14);
        assert!(
            text.contains("filtrer : autre▏") && text.contains("« autre »"),
            "{text}"
        );
        assert!(
            text.contains("1/1 (sur 2+ chargés)") && !text.contains("feat: une chose"),
            "{text}"
        );
        app.commits_mut().unwrap().filter.commit();
        // Sélecteur de branche par-dessus
        app.commits_mut().unwrap().filter.clear();
        app.commits_mut().unwrap().open_picker();
        let text = render(&app, 120, 18);
        assert!(
            text.contains("● main") && text.contains("(défaut)") && text.contains("feature/x"),
            "{text}"
        );
    }

    #[test]
    fn liste_des_branches_a_l_ecran() {
        let mut app = on_repo_page();
        app.list_move(3);
        app.repo_enter();
        let repo = Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        };
        assert!(render(&app, 120, 12).contains("chargement…"));
        let head = |sha: &str, msg: &str| {
            Some(smithy_core::BranchHead {
                sha: sha.into(),
                message: msg.into(),
                author: "bob".into(),
                date: Some("2026-10-14T10:00:00Z".into()),
            })
        };
        app.apply(Msg::Branches {
            repo,
            result: Ok(vec![
                smithy_core::Branch {
                    head: head("abcdef0123", "fix: le bug\n\ndétail"),
                    ..smithy_core::Branch::new("main", true)
                },
                smithy_core::Branch {
                    head: head("1234567890", "feat: nouveau"),
                    protected: true,
                    ..smithy_core::Branch::new("release", false)
                },
            ]),
        });
        let text = render(&app, 130, 12);
        for needle in [
            "Branche",
            "● main",
            "release",
            "[protégée]",
            "abcdef0",
            "fix: le bug",
            "bob",
            "1/2",
        ] {
            assert!(text.contains(needle), "absent : {needle}\n{text}");
        }
        assert!(!text.contains("détail"));
        // Filtre sur le message du dernier commit
        let v = app.branch_list_mut().unwrap();
        v.filter.start();
        "nouveau".chars().for_each(|c| {
            v.filter.type_char(c);
        });
        v.filter_changed();
        let text = render(&app, 130, 12);
        assert!(
            text.contains("1/1 (sur 2)") && !text.contains("● main"),
            "{text}"
        );
    }

    #[test]
    fn le_correctif_d_un_commit_est_colore() {
        use crate::highlight::Tok as T;
        let _ = T::Added;
        let mut app = on_repo_page();
        app.list_move(2);
        app.repo_enter();
        let repo = Repo {
            owner: "smithy".into(),
            name: "demo".into(),
        };
        let c = commit_n(1, "fix: titre\n\nCorps du commit");
        app.apply(Msg::Commits {
            repo: repo.clone(),
            reference: String::new(),
            page: 1,
            result: Ok(smithy_core::CommitPage {
                commits: vec![c.clone()],
                has_more: false,
            }),
        });
        app.commits_enter();
        assert!(render(&app, 100, 12).contains("chargement…"));
        app.apply(Msg::CommitDetail {
            repo,
            sha: c.sha.clone(),
            result: Ok(smithy_core::CommitDetail {
                commit: c,
                parents: vec!["abcdef0123".into()],
                files: vec![smithy_core::FileChange { path: "f.txt".into(), kind: smithy_core::ChangeKind::Modified }],
                additions: 1,
                deletions: 1,
                patch: "diff --git a/f.txt b/f.txt\nindex 1..2 100644\n--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-VIEUXQ\n+NEUFQ\n".into(),
                patch_truncated: true,
            }),
        });
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        for needle in [
            "fix: titre",
            "Parent : abcdef0",
            "Corps du commit",
            "1 fichier modifié",
            "f.txt  +1 −1",
            "@@ -1 +1 @@",
            "correctif tronqué",
        ] {
            assert!(text.contains(needle), "absent : {needle}\n{text}");
        }
        assert!(!text.contains("index 1..2"), "la ligne index est masquée");
        let fg = |sym: &str| {
            buf.content()
                .iter()
                .find(|c| c.symbol() == sym)
                .map(|c| c.fg)
        };
        assert_eq!(fg("V"), Some(Color::Red), "ligne retirée");
        assert_eq!(fg("N"), Some(Color::Green), "ligne ajoutée");
        assert_eq!(fg("@"), Some(Color::Cyan), "en-tête de hunk");
    }
}
