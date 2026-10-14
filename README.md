# smithy

A terminal UI (TUI) to browse the CI and the code of your git forges, written in Rust with [ratatui](https://ratatui.rs).

Built for machines without a desktop (e.g. a server behind a VPN): a single static-ish binary, `rustls` only (no OpenSSL), read-only access.

> **Status:** early development. Only Forgejo is implemented. The UI and its messages are in French.

## Features

- A home page like a web dashboard: your repositories, the ones you watch or starred, a server-wide search, and the ones you opened recently. Each row shows its tags (private, archived, fork, empty), open pull requests, stars and last update; type `/` to filter. Open a repository to get its own page: the latest CI run at a glance, then the CI runs list and the code explorer.
- Read the steps and output of any job, in the terminal (`Enter` on a run): steps are foldable sections like in the web UI (successful ones start folded, failing or running ones open), ANSI colours and progress bars are cleaned, a running job is refreshed and followed, and the view opens on the first failing step.
- Explore the code of a repository (`e`): branches, folders, files, without leaving the terminal. Files are read-only, capped at 1 MiB, with binary files detected and control characters made visible, and **syntax highlighting** for the common languages (Rust, TypeScript, Python, TOML, JSON, YAML, Dockerfile, Makefile, HTML/CSS, shell, SQL, Markdown source, …). The colours are the 16 ANSI colours of your terminal, so they follow its theme. **Markdown files** (`.md`) open rendered: headings, emphasis, lists (nested, ordered, task lists), quotes, tables, links (the target is shown), images (alt text), rules, and code blocks coloured by their language; raw HTML is shown dimmed. Press `m` to switch to the highlighted source.
- Sign in from the TUI (username and password, entered once and never stored).
- A breadcrumb on the first line shows where you are (`server › Home › repo › CI › workflow › job`); pages stack like in a web browser and `Esc` goes back to the previous one exactly as you left it. Open several independent tabs with `t`; each tab has its own server, which you can change from the tab (`S`) without touching the other tabs.
- Run status translated to a common model (`Status`), whatever the forge.
- Automatic refresh every 5 seconds, manual refresh with `r`.
- Backends are pluggable through the `Provider` trait; the UI adapts to what a backend supports (`Capabilities`).

### Forgejo versions

The logs come from Forgejo's internal web routes, which change between versions. smithy detects the server version (`/api/v1/version`) and picks the matching adapter. Listing runs and repositories uses the stable REST API and does not depend on this.

| Forgejo        | Logs                   | Notes |
|----------------|------------------------|-------|
| 10, 11, 12     | Supported (verified)   | CSRF token, no `attempt` segment |
| 13             | Supported (verified)   | CSRF token and `attempt` segment |
| 14, 15         | Supported (verified)   | No CSRF, `attempt` segment |
| 16 and later   | Partial (not verified) | smithy tries the 15 behaviour and says so in the log view |
| 9 and earlier  | Not supported          | Not verified before 10: the log view shows why |
| Gitea          | Not supported          | Forgejo only |

Verified means every version from 10 to 15 was run locally (steps, a 6000-line step, ANSI colours, progress bars, wrong password, expired session).

An unverified version is never silently trusted: the log view shows a ⚠ notice, and a failing route is reported as such. To verify a version yourself: `FORGEJO_VERSION=13 ./dev/dev.sh probe` starts an isolated instance and prints what changed.

### Supported forges

| Forge        | Status                                              |
|--------------|-----------------------------------------------------|
| Forgejo      | Implemented, including job steps and logs (through Forgejo's web routes, which need a login: see below) |
| GitHub       | Planned                                             |
| GitLab       | Planned                                             |
| Bitbucket    | Planned                                             |
| Azure DevOps | Planned                                             |

## Install

Requires Rust 1.89+.

```sh
git clone git@github.com:thomas-gleizes/smithy.git
cd smithy
cargo build --release -p smithy   # binary: target/release/smithy
```

## Configuration

Servers are declared in a TOML file at `~/.config/smithy/config.toml` (platform config dir). It holds no secret other than the optional API token:

```toml
[[server]]
name = "home"            # letters, digits, - _ . only
kind = "forgejo"         # default; other forges are planned
url = "https://forge.example.com"
token = "<your token>"   # read-only is enough

[[server]]
name = "work"
url = "https://git.work.example"
```

All the servers are available at once: smithy starts on `SMITHY_SERVER` if set, otherwise on the first server that already has credentials (a `token` or a saved login), otherwise on the first one. Each tab belongs to one server; press `S` to change the server of the current tab (see below). The former single-server format (`url` and `token` at the top level) is still read, as a server named `default`.

Environment variables override the first server (or define one if the file has none):

| Variable        | Description                          |
|-----------------|--------------------------------------|
| `SMITHY_URL`    | Base URL of the forge instance       |
| `SMITHY_TOKEN`  | API token (read-only is enough)      |
| `SMITHY_SERVER` | Name of the server to use            |

### Logging in

`token` is optional. Without one (and without a previous login), smithy shows a login form: enter your username and password once. smithy then

- signs in to obtain a **session cookie** (Forgejo only serves CI logs through its web routes, which do not accept API tokens);
- creates a **read-only API token** named `smithy-<server>-<timestamp>` on your account, so you can revoke it from the Forgejo settings.

Both are kept per server in `~/.local/state/smithy/sessions/<server>.json` (directory `0700`, file `0600`). **The password is never written to disk.** A `token` set in the config file or in `SMITHY_TOKEN` takes precedence over the one created at login. When the session cookie expires, smithy reopens the form (username prefilled) and only renews the cookie: no new token is created. With a `token` in the config but no login yet, the form opens the first time you ask for logs. Two-factor authentication and SSO are not supported yet.

## Key bindings

Everywhere: `Esc` goes back one page (it never quits), `Ctrl+C` quits, `r` refreshes the current page.

Tabs, on every page (except while typing a filter):

| Key                   | Action                          |
|-----------------------|---------------------------------|
| `t`                   | Open the selection in a **new tab**: the current tab is duplicated, page by page, then the selected item is opened in the copy |
| `Tab` / `Shift+Tab`   | Next / previous tab (wraps around) |
| `T`                   | List the tabs (`j`/`k`, `Enter` to go, `x` to close, `Esc`) |
| `x`                   | Close the current tab (the last one becomes a fresh home page) |
| `S`                   | Change the **server of the current tab** (list of the configured servers; `Enter` to choose) |

Tabs are independent: each keeps its own page stack, selection, scroll position **and server**. `S` lists the configured servers (● marks the current one). Each line shows two independent states: the **network** (*joignable*, or *injoignable* with the reason, e.g. a VPN that is down) and the **session** (*session ouverte* or *pas de session*). Reachability is probed (`/api/v1/version`, no authentication) when the list opens and again every 5 seconds while it stays open, so you see a server is unreachable before choosing it. Choosing one keeps the tab at the same rank but restarts it on that server's home page; the other tabs are left alone, even if they were showing the same repository name on another server. A server you are not signed in to opens its login form first (`Esc` cancels and leaves the tab as it was).

**Managing servers from the app**: in the `S` list, `a` adds a server (name, URL, optional token), `e` edits the selected one (URL and token; the name is fixed, and an empty token keeps the current one), `d` deletes it after a `y` confirmation. Changes are written to `config.toml` (rewritten in the `[[server]]` format, comments are not kept, mode `0600`); deleting also removes the saved login of that server. A server used by a tab, or the last one, cannot be deleted. Servers that only come from `SMITHY_URL` are written without their token. The list also opens with a single server, so a second one can be added right away. With several servers configured, tab titles are prefixed by their server (`work · repo · CI`). A running job keeps being followed in a background tab on its own server. A tab showing a job's logs tells you in the tab bar whether the job is still running (`●`) or how it ended (`✔`, `✘`), and keeps following a running job while you work in another tab. Up to 10 tabs.

**Tabs are restored when smithy restarts**: each tab comes back on its server, with its page stack (home section, repository page, runs list, code folder and branch). The data is reloaded, not saved. A job's logs are not restored: that tab returns to its runs list. Tabs whose server no longer exists are dropped. Start with `smithy --fresh` to skip the restoration.

Only **one running instance** manages the saved tabs (a file lock, released by the system even after a crash). A second instance started in parallel opens one empty tab, says so in the top line (*onglets non sauvegardés : autre instance*) and saves nothing, so it never duplicates or overwrites the first one's tabs. The *Recent* history is merged on save, so parallel instances do not erase each other's entries.

On the home page:

| Key                   | Action                          |
|-----------------------|---------------------------------|
| `j` / `k`, `↓` / `↑`  | Move the selection              |
| `PgDn` / `f`, `PgUp` / `b`, `g` / `G` | Page, first, last |
| `←` / `→`             | Previous / next section (My repositories, Watched, Starred, Explore, Recent) |
| `Enter`               | Open the repository page        |
| `/`                   | Filter the list; in *Explore*, search the whole server (`Enter` runs the search) |
| `Esc`                 | Clear the filter (while typing: cancel) |
| `q`                   | Quit                            |

On a repository page and on the list of runs: `j`/`k` move, `Enter` or `→` opens (CI or Code; a run's logs), `←`, `Esc`, `q` or `Backspace` go back, `PgDn`/`PgUp`, `g`/`G` as above.

In the code explorer:

| Key                   | Action                          |
|-----------------------|---------------------------------|
| `j` / `k`, `↓` / `↑`  | Move the selection              |
| `→` / `Enter`         | Open the folder or file         |
| `←` / `Backspace`     | Go up one folder (or close the file) |
| `PgDn` / `f`, `PgUp` / `b` | Page down / up             |
| `g` / `Home`, `G` / `End` | First / last entry          |
| `B`                   | Choose a branch (the default one comes first) |
| `r`                   | Refresh                         |
| `Esc`, `q`            | Close the file or the picker, then the explorer |

A Markdown file opens rendered; `m` toggles between the rendered view and the source (the rendered view always wraps at word boundaries, so `w` only applies to the source). A file opens in the same viewer as the logs (the text shows at once; the colours arrive a moment later, computed in the background; files over 20,000 lines or lines over 1,000 bytes stay uncoloured): `j`/`k`, `f`/`b`, `g`/`G` and `w` (wrap) work the same way. Symbolic links and submodules are listed but not opened. The explorer only needs the read-only API token (no session cookie) and works on every Forgejo version from 10 to 15 (the content API is the same).

In the log view:

| Key                   | Action                          |
|-----------------------|---------------------------------|
| `j` / `k`, `↓` / `↑`  | Scroll one line                 |
| `PgDn` / `f`, `PgUp` / `b` | Scroll one page          |
| `→` / `←`             | Select the next / previous step |
| `Space`               | Fold / unfold the selected step |
| `a`                   | Unfold all steps (or fold all if all are open) |
| `g` / `Home`, `G` / `End` | Top / bottom                |
| `w`                   | Toggle line wrapping (kept for the session) |
| `r`                   | Refresh                         |
| `Esc`, `q`, `Backspace` | Back to the runs              |
| `Ctrl+C`              | Quit                            |

The selected step is the one you moved to with `→` / `←`, or otherwise the one at the top of the screen; its title is shown in reverse video. Folding state survives refreshes, and a step that was open while running stays open when it finishes.

Long lines are clipped by default; `w` wraps them at the exact terminal width (characters are cut, not words, so paths and JSON stay intact). Scrolling counts screen rows when wrapped, so a single very long line can be read to its end. Output is capped to the last 50,000 lines per step.

## Development

Everything goes through the `Makefile` (run `make` for the list of targets).

```sh
make check      # fmt-check + clippy (-D warnings) + tests
make dev-run    # start a throwaway local Forgejo + runner (Docker Compose) and run smithy against it
make dev-reset  # destroy the local environment
```

### Architecture

Cargo workspace of 3 crates, dependencies flow one way: `smithy` → `smithy-forgejo` → `smithy-core`.

- `smithy-core`: common models, `Error`, `Capabilities` and the `Provider` trait.
- `smithy-forgejo`: Forgejo REST client implementing `Provider`.
- `smithy`: the binary (application state, rendering, config, event loop).

A new forge is a new crate implementing `Provider`.

## Roadmap

- [ ] CI workflow running `make check`
- [x] Page stack with a breadcrumb
- [x] Home page (my repositories, watched, starred, explore, recent) and a repository page
- [ ] Organisations on the home page (needs an extra `read:organization` token scope)
- [x] Independent tabs, each with its own page stack
- [x] One server per tab (several servers at once, switched from the tab with `S`)
- [x] Add, edit and delete servers from the app
- [x] Restore the open tabs at startup (one instance owns them, `--fresh` to skip)
- [ ] Pull requests, notifications and runners
- [ ] Filters and search (status, branch, workflow, repository)
- [x] Job steps and logs for Forgejo
- [x] Code explorer (branches, folders, files)
- [x] Syntax highlighting in the explorer
- [x] Markdown rendering in the explorer
- [ ] File search by name in the explorer, tags and commits as references
- [ ] Open the run URL from the TUI
- [ ] GitHub backend (jobs and logs)
- [x] Multiple instances in the config file (selected per tab from the TUI)
- [ ] GitLab, Bitbucket, Azure DevOps backends

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
