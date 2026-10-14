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

    # Idempotence : le dépôt est complet quand le dernier fichier de la liste est là.
    if call("GET", f"/repos/{user}/tree/contents/big.txt?ref=main", ok=(200, 404))[0] == 200:
        star()
        print("dépôt tree déjà présent")
        return
    if not exists("tree"):
        call("POST", "/user/repos", {"name": "tree", "private": True, "auto_init": True, "default_branch": "main"})

    b64 = lambda b: base64.b64encode(b).decode()
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


main()
