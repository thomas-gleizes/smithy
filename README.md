# smithy

A terminal UI (TUI) to browse the CI and the code of your git forges, written in Rust with [ratatui](https://ratatui.rs).

Built for machines without a desktop (e.g. a server behind a VPN): a single static-ish binary, `rustls` only (no OpenSSL), read-only access.

> **Status:** early development. Only Forgejo is implemented. The UI and its messages are in French.

## Features

- List the repositories you can access and the latest CI runs of the selected one.
- Read the steps and output of any job, in the terminal (`Enter` on a run): steps are foldable sections like in the web UI (successful ones start folded, failing or running ones open), ANSI colours and progress bars are cleaned, a running job is refreshed and followed, and the view opens on the first failing step.
- Explore the code of a repository (`e`): branches, folders, files, without leaving the terminal. Files are read-only, capped at 1 MiB, with binary files detected and control characters made visible.
- Sign in from the TUI (username and password, entered once and never stored).
- A breadcrumb on the first line shows where you are (`server › Home › repo › CI › workflow › job`); pages stack like in a web browser and `Esc` goes back to the previous one exactly as you left it.
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

Requires Rust 1.85+.

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

The first server is used by default; pick another one with `SMITHY_SERVER=work`. The former single-server format (`url` and `token` at the top level) is still read, as a server named `default`.

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

| Key               | Action                    |
|-------------------|---------------------------|
| `j` / `k`, `↓` / `↑` | Move selection         |
| `Tab`             | Switch panel              |
| `Enter`           | Open the logs of the selected run (on the repository list: go to its runs) |
| `e`               | Explore the code of the selected repository |
| `r`               | Refresh                   |
| `q`, `Ctrl+C`     | Quit (`q` only on the home screen; elsewhere it goes back) |
| `Esc`             | Go back one page (never quits) |

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

A file opens in the same viewer as the logs: `j`/`k`, `f`/`b`, `g`/`G` and `w` (wrap) work the same way. Symbolic links and submodules are listed but not opened. The explorer only needs the read-only API token (no session cookie) and works on every Forgejo version from 10 to 15 (the content API is the same).

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
- [ ] Home page (my repositories, organisations, watched, explore, recent) and a repository page
- [ ] Independent tabs, each with its own page stack and server
- [ ] Pull requests, notifications and runners
- [ ] Filters and search (status, branch, workflow, repository)
- [x] Job steps and logs for Forgejo
- [x] Code explorer (branches, folders, files)
- [ ] Syntax highlighting in the explorer
- [ ] File search by name in the explorer, tags and commits as references
- [ ] Open the run URL from the TUI
- [ ] GitHub backend (jobs and logs)
- [x] Multiple instances in the config file (selection from the TUI still to come)
- [ ] GitLab, Bitbucket, Azure DevOps backends

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
