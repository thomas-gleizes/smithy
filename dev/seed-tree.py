#!/usr/bin/env python3
"""Fixtures de l'explorateur de code : dépôts privés `tree` (contenu varié) et `vide` (sans commit).

    seed-tree.py <url> <login> <mot de passe>

Création par l'API uniquement (pas de `git push`), idempotent. Aucun workflow dans ces dépôts :
un push sur `secret` déclencherait ses runs et fausserait les vérifications de la CI.
Instance jetable seulement : le mot de passe passe en argument.
"""
import base64
import json
import os
import sys
import urllib.error
import urllib.request


def main():
    url, user, password = sys.argv[1:4]
    url = url.rstrip("/")
    auth = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()

    def call(method, path, body=None, ok=(200, 201, 204)):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(
            f"{url}/api/v1{path}", data=data, method=method,
            headers={"Authorization": auth, "Content-Type": "application/json"},
        )
        try:
            r = urllib.request.urlopen(req, timeout=60)
            code, text = r.status, r.read().decode("utf8", "replace")
        except urllib.error.HTTPError as e:
            code, text = e.code, e.read().decode("utf8", "replace")
        if code not in ok:
            raise SystemExit(f"{method} {path} -> HTTP {code}: {text[:200]}")
        return code, (json.loads(text) if text.strip().startswith(("{", "[")) else text)

    def exists(repo):
        return call("GET", f"/repos/{user}/{repo}", ok=(200, 404))[0] == 200

    # --- dépôt vide : ni commit ni branche
    if not exists("vide"):
        call("POST", "/user/repos", {"name": "vide", "private": True, "auto_init": False})
        print("dépôt vide créé")

    # Un favori, pour que la liste « Étoilés » de l'accueil ne soit pas vide (idempotent).
    def star():
        if exists("tree"):
            call("PUT", f"/user/starred/{user}/tree", ok=(204,))

    # Fichiers pour la coloration syntaxique et l'affichage Markdown (idempotent, ajoutés après
    # coup : un `tree` déjà créé les reçoit aussi). Jamais dans `secret` (ses workflows se
    # déclencheraient) et jamais par `git push`.
    def seed_code():
        if call("GET", f"/repos/{user}/tree/contents/code/sample.rs?ref=main", ok=(200, 404))[0] == 200:
            return
        big_rs = "".join(
            f'pub fn fonction_{i:05d}(x: u64) -> u64 {{ let y = x * {i} + 1; y % 97 }} // ligne {i}\n'
            for i in range(17000)
        )
        code = {
            "code/sample.rs": (
                "// Un commentaire\nuse std::collections::HashMap;\n\n"
                "/// Documentation\npub struct Point { x: i32, y: i32 }\n\n"
                "fn main() {\n    let n: u32 = 42;\n    let s = \"chaîne avec accents é\";\n"
                "    let mut m = HashMap::new();\n    m.insert(1, s);\n    println!(\"{n} {:?}\", m);\n}\n"
            ),
            "code/Cargo.toml": '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nserde = { version = "1", features = ["derive"] }\n',
            "code/app.ts": "interface User { id: number; name: string }\nexport async function load(id: number): Promise<User> {\n  const r = await fetch(`/api/${id}`);\n  return r.json() as Promise<User>; // fin\n}\n",
            "code/data.json": '{"nom": "smithy", "versions": [1, 2, 3], "actif": true, "x": null}\n',
            "code/script.py": "#!/usr/bin/env python\nimport os\n\ndef main(n: int = 3) -> None:\n    \"\"\"Doc.\"\"\"\n    for i in range(n):\n        print(f'valeur {i}')  # commentaire\n",
            "code/Makefile": "all: build\n\nbuild:\n\tcargo build --release\n",
            "code/Dockerfile": "FROM rust:1.89\nWORKDIR /app\nCOPY . .\nRUN cargo build --release\nCMD [\"./target/release/demo\"]\n",
            "code/style.css": "body { color: #333; margin: 0 auto; }\n.a > .b:hover { background: url(x.png); }\n",
            "code/page.html": '<!doctype html>\n<html lang="fr">\n  <body class="x"><h1>Titre</h1><!-- note --></body>\n</html>\n',
            "code/big.rs": big_rs,
            "code/sans-couleur.xyz": "format inconnu\n",
            "docs/rich.md": (
                "# Titre principal\n\nUn paragraphe avec du **gras**, de l'*italique*, du `code en ligne`, "
                "un [lien](https://example.com/page) et du ~~barré~~. Ce paragraphe est assez long pour "
                "devoir être replié à la largeur du terminal sans couper les mots au milieu, y compris "
                "quand le terminal est étroit.\n\n## Listes\n\n- un\n- deux avec un texte plutôt long qui "
                "dépasse la largeur de l'écran et doit se replier proprement\n  - sous-élément\n  - autre\n"
                "- trois\n\n1. premier\n2. deuxième\n\n- [x] tâche faite\n- [ ] tâche à faire\n\n"
                "> Une citation\n> sur deux lignes\n\n### Code\n\n```rust\nfn main() {\n    println!(\"salut\");\n}\n```\n\n"
                "```\nbloc sans langage\n```\n\n#### Tableau\n\n| Nom | Valeur |\n|-----|-------:|\n| a | 1 |\n| bb | 22 |\n\n"
                "---\n\n![image](logo.png)\n\n<div>du HTML brut</div>\n"
            ),
        }
        call("POST", f"/repos/{user}/tree/contents", {
            "message": "fixtures de coloration et de markdown", "branch": "main",
            "files": [{"operation": "create", "path": p, "content": b64(c.encode())} for p, c in code.items()],
        })
        print(f"{len(code)} fichiers de code/markdown créés dans tree")

    # --- historique : une branche `history` avec 60 commits (au-delà d'une page de 50), un message
    # sur plusieurs lignes, de l'unicode, un commit qui ajoute + modifie + supprime, un fichier
    # binaire. Sur une branche à part : la racine de `main` ne bouge pas. Les fusions ne sont pas
    # créables par l'API : non couvertes. Idempotent.
    def seed_history():
        if call("GET", f"/repos/{user}/tree/branches/history", ok=(200, 404))[0] == 200:
            return
        call("POST", f"/repos/{user}/tree/branches", {"new_branch_name": "history", "old_branch_name": "main"})
        path = f"/repos/{user}/tree/contents"

        def blob_sha(p):
            return call("GET", f"{path}/{p}?ref=history")[1]["sha"]

        def commit(message, files):
            call("POST", path, {"message": message, "branch": "history", "files": files})

        commit("docs: premier historique\n\nCorps du message sur plusieurs lignes,\navec une seconde ligne.\n\n- puce un\n- puce deux",
               [{"operation": "create", "path": "history/log.txt", "content": b64(b"ligne 0\n")},
                {"operation": "create", "path": "history/old.txt", "content": b64(b"a supprimer\n")}])
        for i in range(1, 56):
            msg = f"chore: mise à jour {i:02d}" if i != 7 else "fix: accents é à ü, 日本語 et ✔ dans un message"
            body = "".join(f"ligne {k}\n" for k in range(i + 1)).encode()
            commit(msg, [{"operation": "update", "path": "history/log.txt", "sha": blob_sha("history/log.txt"), "content": b64(body)}])
        # ajoute + modifie + supprime en un seul commit
        commit("refactor: ajoute, modifie et supprime",
               [{"operation": "create", "path": "history/new.txt", "content": b64(b"nouveau fichier\n")},
                {"operation": "update", "path": "history/log.txt", "sha": blob_sha("history/log.txt"), "content": b64(b"remplace\n")},
                {"operation": "delete", "path": "history/old.txt", "sha": blob_sha("history/old.txt")}])
        # fichier binaire, puis sa modification
        commit("feat: ajoute un binaire", [{"operation": "create", "path": "history/data.bin", "content": b64(bytes(range(256)))}])
        commit("fix: modifie le binaire", [{"operation": "update", "path": "history/data.bin", "sha": blob_sha("history/data.bin"), "content": b64(bytes(range(255, -1, -1)))}])
        commit("feat: ajoute\nun message dont la première ligne est courte", [{"operation": "create", "path": "history/last.txt", "content": b64(b"fin\n")}])
        print("branche history créée (60 commits)")

    b64 = lambda b: base64.b64encode(b).decode()
    # Idempotence : le dépôt est complet quand le dernier fichier de la liste est là.
    if call("GET", f"/repos/{user}/tree/contents/big.txt?ref=main", ok=(200, 404))[0] == 200:
        star()
        seed_code()
        seed_history()
        print("dépôt tree déjà présent")
        return
    if not exists("tree"):
        call("POST", "/user/repos", {"name": "tree", "private": True, "auto_init": True, "default_branch": "main"})

    files = {
        "src/main.rs": b'fn main() {\n\tprintln!("bonjour");\n}\n',
        "src/lib.rs": b"pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        "docs/guide/intro.md": b"# Intro\n",
        "dossier avec espaces/fichier a b.txt": b"espaces dans le chemin\n",
        "dossier avec espaces/fichier #1 ?.txt": b"# et ? dans le nom\n",
        "été/résumé.md": "unicode dans le chemin\n".encode(),
        "empty.txt": b"",
        "crlf.txt": b"un\r\ndeux\r\ntrois\r\n",
        "cr-isole.txt": b"avant\rapres\nligne suivante\n",
        "tabs.txt": b"a\tb\tc\n\tindente\n",
        "esc.txt": b"\x1b[31mrouge\x1b[0m dans un fichier\n",
        "long-line.json": ("{\"data\": [" + ",".join(str(i) for i in range(1500)) + "]}\n").encode(),
        "bin/data.bin": bytes(range(256)) * 8 + b"\x00\x01\x02",
        "big.txt": b"".join(f"ligne {i:07d} du gros fichier pour depasser la limite de taille\n".encode() for i in range(24000)),
    }
    # 130 fichiers dans un dossier : un listing plus long que la pagination par défaut (50).
    for i in range(130):
        files[f"many/f{i:03d}.txt"] = f"{i}\n".encode()
    call("POST", f"/repos/{user}/tree/contents", {
        "message": "fixtures",
        "branch": "main",
        "files": [{"operation": "create", "path": p, "content": b64(c)} for p, c in files.items()],
    })
    print(f"{len(files)} fichiers créés dans tree")

    # --- branches : une avec un slash et un fichier propre, puis de quoi dépasser une page (50).
    call("POST", f"/repos/{user}/tree/branches", {"new_branch_name": "feature/x", "old_branch_name": "main"})
    call("POST", f"/repos/{user}/tree/contents", {
        "message": "propre à feature/x", "branch": "feature/x",
        "files": [{"operation": "create", "path": "seulement-sur-feature.txt", "content": b64(b"branche feature/x\n")}],
    })
    for i in range(60):
        call("POST", f"/repos/{user}/tree/branches", {"new_branch_name": f"b{i:02d}", "old_branch_name": "main"})
    print("branches créées : main, feature/x, b00..b59")
    star()
    seed_code()
    seed_history()


main()
