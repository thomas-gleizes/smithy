# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Projet

`smithy` : TUI Rust (ratatui) pour consulter la CI de forges git. Forgejo seul est implémenté ; GitHub, GitLab, Bitbucket et Azure DevOps sont prévus, d'où le trait `Provider`. Usage visé : machine sans bureau derrière un VPN. Outil personnel, publié sur GitHub (licence `MIT OR Apache-2.0`). L'UI et les messages sont en français.

## Commandes

Tout passe par le `Makefile` (qui ajoute `~/.cargo/bin` au `PATH`) ; `make` affiche l'aide.

- `make check` : `fmt-check` + `lint` (clippy `-D warnings`) + `test`. À lancer avant de conclure.
- `make test` ; un seul test : `cargo test -p smithy-forgejo pagine_les_depots`
- `make dev-run` : démarre le Forgejo local si besoin, puis lance l'app dessus. `make run` utilise `SMITHY_URL`/`SMITHY_TOKEN` ou `~/.config/smithy/config.toml`.
- Couverture : `cargo llvm-cov --workspace` (non encadré par le Makefile). `main.rs` est à 0 %, le reste autour de 80–100 %.

## Architecture

Workspace de 3 crates, dépendances à sens unique : `smithy` → `smithy-forgejo` → `smithy-core`.

- **`smithy-core`** : modèles communs (`Repo`, `Pipeline`, `Status`), `Error`, `Capabilities`, trait `Provider` (méthodes `impl Future + Send`, pas `async_trait` : donc générique `P: Provider`, pas de `dyn`). Un nouveau backend (GitHub, GitLab…) = un nouveau crate qui implémente ce trait. `Capabilities` expose ce qu'un backend sait faire (ex. `logs`) pour que l'UI s'adapte.
- **`smithy-forgejo`** : client REST. Les champs serde sont tous `#[serde(default)]` sauf `id`, pour tolérer les écarts de version. Il traduit les statuts Forgejo (`waiting`, `blocked`, `success`, …) vers `Status`, et les codes HTTP vers `Error`.
- **`smithy`** (binaire) : `app.rs` est l'état pur (aucune dépendance terminal, testable seul), `ui.rs` le rendu (testé avec `TestBackend`), `config.rs` la config, `main.rs` la boucle d'événements. Flux : les requêtes tournent dans des `tokio::spawn` qui renvoient un `Msg` par canal ; `App::apply` les intègre et renvoie `true` s'il faut recharger les pipelines. Les réponses périmées (dépôt changé entre-temps) sont ignorées dans `apply`.

## Particularités de l'API Forgejo (validées sur Forgejo 15, swagger de l'instance)

- Les runs viennent de `GET /repos/{o}/{r}/actions/tasks` (clé `workflow_runs`, un run par entrée). Il n'existe **aucun endpoint REST de jobs ni de logs** : `capabilities.logs` est `false` et la TUI affiche l'URL du run. Ne pas supposer qu'un endpoint de logs existe sans le vérifier dans `<instance>/swagger.v1.json`.
- Dans cette réponse, `name` est le nom du job et `workflow_id` le fichier de workflow ; `Pipeline.workflow` vient de `workflow_id`.
- Auth : en-tête `Authorization: token <t>`. Le token de dev est en lecture seule.

## Environnement de dev (`dev/`)

`dev/dev.sh` pilote un Forgejo jetable (SQLite) + un `forgejo-runner` dans Docker Compose : `up`, `seed` (pousse un workflow vert et un rouge, idempotent), `reset` (`down -v`), `env`. Le token est écrit dans `dev/.env.local` (gitignoré). Forgejo écoute sur `127.0.0.1:3300` ; la version est surchargeable avec `FORGEJO_VERSION`.

- Le runner tourne en root avec le socket Docker de l'hôte : acceptable pour ce dev jetable uniquement.
- Les workflows de démo n'ont pas de `checkout` : les conteneurs de job ne joignent pas `localhost:3300`.
- Un `docker compose up` sans token de runner fait crasher le service `runner` ; `dev.sh` démarre donc `forgejo` seul d'abord, puis le runner avec le token.

## Conventions

- Lints workspace : `unsafe_code = "forbid"`, clippy en `warn` (`-D warnings` via `make lint`).
- `rustls` uniquement (pas d'OpenSSL) pour que le binaire tourne sur une machine sans bureau.
- Le `README.md` (anglais) est à tenir à jour à chaque évolution visible : fonctionnalités, raccourcis, config, backends supportés, roadmap.
