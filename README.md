# smithy

A terminal UI (TUI) to browse the CI of your git forges, written in Rust with [ratatui](https://ratatui.rs).

Built for machines without a desktop (e.g. a server behind a VPN): a single static-ish binary, `rustls` only (no OpenSSL), read-only access.

> **Status:** early development. Only Forgejo is implemented. The UI and its messages are in French.

## Features

- List the repositories you can access and the latest CI runs of the selected one.
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

Environment variables take precedence over the config file.

| Variable       | Description                          |
|----------------|--------------------------------------|
| `SMITHY_URL`   | Base URL of the forge instance       |
| `SMITHY_TOKEN` | API token (read-only is enough)      |

Or a TOML file at `~/.config/smithy/config.toml` (platform config dir):

```toml
url = "https://forge.example.com"
token = "<your token>"
```

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
- [ ] Multiple instances in the config file
- [ ] GitLab, Bitbucket, Azure DevOps backends

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
