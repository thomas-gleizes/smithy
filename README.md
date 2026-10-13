# smithy

A terminal UI (TUI) to browse the CI of your git forges, written in Rust with [ratatui](https://ratatui.rs).

Built for machines without a desktop (e.g. a server behind a VPN): a single static-ish binary, `rustls` only (no OpenSSL), read-only access.

> **Status:** early development. Only Forgejo is implemented. The UI and its messages are in French.

## Features

- List the repositories you can access and the latest CI runs of the selected one.
- Sign in from the TUI (username and password, entered once and never stored).
- Run status translated to a common model (`Status`), whatever the forge.
- Automatic refresh every 5 seconds, manual refresh with `r`.
- Backends are pluggable through the `Provider` trait; the UI adapts to what a backend supports (`Capabilities`).

### Supported forges

| Forge        | Status                                              |
|--------------|-----------------------------------------------------|
| Forgejo      | Implemented (no job list or logs: not in the REST API, the run URL is shown instead) |
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

Both are kept per server in `~/.local/state/smithy/sessions/<server>.json` (directory `0700`, file `0600`). **The password is never written to disk.** A `token` set in the config file or in `SMITHY_TOKEN` takes precedence over the one created at login. Two-factor authentication and SSO are not supported yet.

## Key bindings

| Key               | Action                    |
|-------------------|---------------------------|
| `j` / `k`, `↓` / `↑` | Move selection         |
| `Tab`             | Switch panel              |
| `r`               | Refresh                   |
| `q`, `Esc`, `Ctrl+C` | Quit                   |

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
- [ ] Filters and search (status, branch, workflow, repository)
- [ ] Open the run URL from the TUI
- [ ] GitHub backend (jobs and logs)
- [x] Multiple instances in the config file (selection from the TUI still to come)
- [ ] GitLab, Bitbucket, Azure DevOps backends

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
