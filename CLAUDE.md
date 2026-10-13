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

- **`smithy-core`** : modèles communs (`Repo`, `Pipeline`, `Status`, et pour l'explorateur `Branch`, `Entry`, `EntryKind`, `FileContent`), `Error`, `Capabilities`, trait `Provider` (méthodes `impl Future + Send`, pas `async_trait` : donc générique `P: Provider`, pas de `dyn`). Un nouveau backend (GitHub, GitLab…) = un nouveau crate qui implémente ce trait. `Capabilities` expose ce qu'un backend sait faire (ex. `logs`) pour que l'UI s'adapte. `Provider::job_log` a une implémentation par défaut `Unsupported` ; `clean_log_line` (ANSI, `\r`, tabulations) est dans le core pour que tous les backends rendent des lignes affichables ; `clean_text_line` sert aux fichiers (ne garde **pas** « ce qui suit le dernier `\r` » comme `clean_log_line`, qui ferait disparaître du contenu : les contrôles deviennent des images Unicode `␛`, `␍`…). `list_branches`, `read_dir` et `read_file` (plafond d'octets passé par l'appelant) ont une implémentation par défaut `Unsupported`, `Capabilities.browse` les annonce.
- **`smithy-forgejo`** : client REST. Les champs serde sont tous `#[serde(default)]` sauf `id`, pour tolérer les écarts de version. Il traduit les statuts Forgejo (`waiting`, `blocked`, `success`, …) vers `Status`, et les codes HTTP vers `Error`.
- **`smithy`** (binaire) : `app.rs` est l'état pur (aucune dépendance terminal, testable seul), `ui.rs` le rendu (testé avec `TestBackend`), `config.rs` la config (serveurs multiples, `kind` + `url` + `token` optionnel, aucun secret hors token), `browser.rs` l'explorateur de code (machine à états pure : dossier, sélecteur de branche, fichier ; elle dépose des `Request` dans une file que `main.rs` exécute, et ignore les réponses périmées par dépôt/branche/chemin), `logview.rs` le volet de texte `TextView` (partagé entre les logs et un fichier ouvert dans l'explorateur ; `LogView` = `TextView` + dépôt + pipeline, via `Deref`) avec ses sections pliables par étape, défilement en **lignes d'écran** et retour à la ligne, avec un index des lignes d'écran mis en cache par largeur ; pur, la taille utile lui est passée) et `wrap.rs` le découpage au caractère (`unicode-width`), `session.rs` le cookie/token issus du login (un fichier par serveur dans `~/.local/state/smithy/sessions`, 0600 ; le mot de passe n'est jamais écrit), `main.rs` la boucle d'événements. Flux : les requêtes tournent dans des `tokio::spawn` qui renvoient un `Msg` par canal ; `App::apply` les intègre et renvoie `true` s'il faut recharger les pipelines. Les réponses périmées (dépôt changé, vue des logs fermée ou ouverte sur un autre job) sont ignorées dans `apply`. La vue des logs (`App.logs` ; `w` retour à la ligne conservé par `App.logs_wrap` ; `Espace` plie la section courante, `→`/`←` la changent, `a` tout plier/déplier) s'ouvre sur la première étape en échec ; un job en cours est rafraîchi toutes les 5 s et sa fin est suivie. Les étapes réussies ou ignorées démarrent repliées, comme sur le web ; l'état de pliage est gardé par indice d'étape, et une étape vue ouverte (elle tournait) le reste. La section courante est la sélectionnée avec les flèches droite/gauche, sinon celle de la ligne du haut : sans sélection explicite, un job tout replié qui tient à l'écran ne permettrait jamais d'atteindre la 2e étape (le défilement est impossible). Une session expirée (`Error::SessionExpired`) rouvre le formulaire de login, qui ne renouvelle alors que le cookie : créer un token à chaque expiration les ferait s'empiler sur le compte.

## Versions de Forgejo : un adaptateur par famille

Les routes web des logs varient selon la version, donc `smithy-forgejo/src/version.rs` associe la version (`/api/v1/version`, mise en cache dans `Forgejo::decision`) à un adaptateur `Flavor` et à un niveau de support (`Support::{Full, Partial, Unsupported}`) :

| Version | Adaptateur | Logs | CSRF (cookie `_csrf` + en-tête) | Segment `attempt` | Login |
|---|---|---|---|---|---|
| 10, 11, 12 | `Legacy` | complet (sondé) | exigé | absent (404) | champ `_csrf`, échec en 303 vers `/user/login`, cookies `i_like_gitea` + `gitea_incredible` |
| 13 | `Transitional` | complet (sondé) | exigé | obligatoire (500 sans) | idem |
| 14 | `Modern` | complet (sondé) | plus exigé | obligatoire | pas de champ `_csrf`, échec en 303, mêmes cookies |
| 15 | `Modern` | complet (sondé) | non | obligatoire (réponse vide sans) | pas de champ `_csrf`, échec en **200**, cookies `session` + `persistent` |
| ≥ 16 | `Modern` supposé | partiel | | | |
| ≤ 9, Gitea | — | non supporté (`Error::UnsupportedVersion`, aucune requête web) | | | |

Deux interrupteurs indépendants expliquent tout : le CSRF (retiré en 14) et `attempt` (apparu en 13). Autres faits mesurés : `currentJob` s'appelle `detail` en 10–13 et `details` en 14–15 (`allAttempts` apparaît en 13) ; dès la 13 la page du run redirige (307) vers `…/jobs/0/attempt/1` (on la suit pour lire le jeton) ; **sans `attempt`, les 13 et 14 répondent 500** : le premier appel part donc de la tentative 1 pour toutes les versions qui utilisent les tentatives ; le cookie `gitea_incredible` (10–14) dure 31 jours ; une étape hors bornes donne 500 (HTML) partout. Le login ne dépend pas de la version : il lit le formulaire (avec ou sans `_csrf`) et considère comme un échec toute réponse qui n'est pas une redirection hors de `/user/login`.

Une version partielle est utilisée et `JobLog.notice` la signale (⚠ dans le cadre de la vue des logs). Pour valider ou ajouter une version : `FORGEJO_VERSION=N dev/dev.sh probe` (instance isolée, voir plus bas), lire le JSON, puis ajuster `decide` et, si le comportement est nouveau, ajouter une variante de `Flavor`. Les 10, 11 et 12 se comportent à l'identique ; la 13 puis la 14 changent chacune une chose. Ne jamais déduire d'un test sur la 15 que la 10 se comporte pareil : c'est l'instance réelle de Thomas (v10, migration prévue).

## Explorateur de code (API REST, identique de la 10 à la 15)

Contrairement aux logs, tout passe par l'API documentée avec le **token** (pas de cookie, pas d'adaptateur par version) : `GET /repos/{o}/{r}` (`default_branch`), `/branches` (page de 30 par défaut, **plafonnée à 50** ; total dans `x-total-count`), `/contents[/{chemin}]?ref=` (liste d'un dossier : tout en une réponse, 130 entrées vues, pas de plafond), `/raw/{chemin}?ref=` (octets, avec `Content-Length`). Mesuré avec `dev/dev.sh probe-browse` sur 10, 11, 12, 13, 14 et 15 : seuls des champs facultatifs d'une entrée varient. Points à retenir :
- **Dépôt sans commit** : `/branches` répond **`null`** (pas `[]`) et `/contents` une liste vide. `null` doit être décodé comme une liste vide (`Option<Vec<_>>`), sinon l'explorateur affiche « réponse inattendue » : bug trouvé par le test réel, pas par les tests unitaires.
- **Fichiers** : lire par `raw` en s'arrêtant au plafond (1 Mio, `FILE_LIMIT_BYTES`) ; `contents` renvoie tout le fichier en base64 sans plafond. Binaire = octet nul dans les 8 premiers Kio. Au plus 50 000 lignes affichées.
- **Chemins** : encoder chaque segment (`encode_path`) ; espaces, `#`, `?`, `%`, accents vérifiés. Une branche avec un `/` (`feature/x`) passe telle quelle en paramètre `ref`.
- Lien symbolique et sous-module : listés, jamais ouverts (non créables par l'API de dev, donc non testés en réel : seulement par tests unitaires).
- `ref` vide n'est jamais envoyé : on connaît d'abord la branche par défaut (liste des branches), puis on demande la racine.
- Touches : `e` depuis la liste des dépôts ; `B` ouvre le sélecteur de branche (pas `b`, déjà « page précédente » dans le volet de texte).

## Particularités de l'API Forgejo (décrites ici pour la 15 ; swagger de l'instance)

- Les runs viennent de `GET /repos/{o}/{r}/actions/tasks` (clé `workflow_runs`, un run par entrée = un job ; `run_number` est le numéro du run dans l'URL, distinct de `id`). Il n'existe **aucun endpoint REST de jobs ni de logs** dans le swagger.
- Les logs ne sont servis que par les **routes web internes** (non documentées, donc susceptibles de changer) : `POST /{o}/{r}/actions/runs/{run_number}/jobs/{index}/attempt/{n}` avec `{"logCursors":[{"step":0,"cursor":null,"expanded":true}]}`. Sans le segment `attempt/{n}`, `steps` reste vide sans erreur. `index` est la position du job dans `state.run.jobs`, pas son id. Les lignes contiennent des séquences ANSI et des `\r`. Une étape hors bornes dans `logCursors` fait répondre une page HTML d'erreur : lire d'abord la liste des étapes (appel sans curseur). Pas de pagination observée (6000 lignes en une réponse) ; un curseur `N` renvoie les lignes d'index > N. Procédure dans `smithy-forgejo/src/logs.rs` : liste des jobs → index → dernière tentative (`allAttempts`) → étapes → sortie.
- **Ces routes n'acceptent pas le token** : sur un dépôt privé, token et basic auth donnent 404 ; seul le cookie `session` passe (sur un dépôt public, tout passe, y compris sans auth : ne pas s'y fier pour tester l'auth). L'API `/api/v1` fait l'inverse (cookie seul = 401).
- Login (`smithy-forgejo/src/login.rs`) : `POST /user/login` (formulaire `user_name`, `password`, `remember=on`, sans CSRF sur Forgejo 15) → 303 si succès, 200 si échec ; le dernier `Set-Cookie: session` l'emporte. Le token en lecture seule est créé ensuite par `POST /api/v1/users/{login}/tokens` en basic auth. Le cookie `persistent` seul ne restaure pas la session : à l'expiration, il faut se reconnecter. Durée de vie de la session non mesurée. 2FA/SSO non gérés.
- Dans cette réponse, `name` est le nom du job et `workflow_id` le fichier de workflow ; `Pipeline.workflow` vient de `workflow_id`.
- Auth : en-tête `Authorization: token <t>`. Le token de dev est en lecture seule.

## Environnement de dev (`dev/`)

`dev/dev.sh` pilote un Forgejo jetable (SQLite) + un `forgejo-runner` dans Docker Compose : `up`, `seed` (pousse un workflow vert et un rouge sur `smithy/demo`, et un workflow riche — 2 jobs, ANSI, 250 lignes — sur le dépôt **privé** `smithy/secret`, plus les dépôts **privés sans workflow** `smithy/tree` — 144 fichiers de test, 62 branches dont `feature/x`, un fichier de 1,5 Mio, un binaire, des noms avec espaces/`#`/`?`/accents — et `smithy/vide` sans commit, via `dev/seed-tree.py` ; idempotent), `reset` (`down -v`), `env`, `wait` (attend la fin des runs), `probe` (seed + `dev/probe-logs.py`, JSON des différences entre versions des routes de logs), `probe-browse` (idem pour l'API de contenu, `dev/probe-browse.py`). `FORGEJO_VERSION=N` lance une autre version **isolée** de la 15 (projet Compose `smithy-dev-vN`, port 3300+N, `dev/.env.vN.local`). Le token est écrit dans `dev/.env.local` (gitignoré). Forgejo écoute sur `127.0.0.1:3300` (15 par défaut).

- Le runner tourne en root avec le socket Docker de l'hôte : acceptable pour ce dev jetable uniquement.
- Les workflows de démo n'ont pas de `checkout` : les conteneurs de job ne joignent pas `localhost:3300`.
- **Fixtures de l'explorateur : jamais dans `secret`** (ses workflows `on: [push]` se déclencheraient à chaque ajout et fausseraient `wait`), jamais par `git push`, et pas par `put_file_in` pour du gros ou du binaire (argument limité à 128 Kio, octets nuls perdus) : `seed-tree.py` utilise l'API de contenu multi-fichiers.
- Ne pas lancer de `docker pull` interrompu/répété pendant qu'un autre tourne : le démon peut rester bloqué (observé une fois : `docker ps` ne répondait plus alors que les conteneurs tournaient ; je n'ai pas vérifié ce qui le débloque).
- Un `docker compose up` sans token de runner fait crasher le service `runner` ; `dev.sh` démarre donc `forgejo` seul d'abord, puis le runner avec le token.

## Conventions

- Lints workspace : `unsafe_code = "forbid"`, clippy en `warn` (`-D warnings` via `make lint`).
- `rustls` uniquement (pas d'OpenSSL) pour que le binaire tourne sur une machine sans bureau.
- Le `README.md` (anglais) est à tenir à jour à chaque évolution visible : fonctionnalités, raccourcis, config, backends supportés, roadmap.
