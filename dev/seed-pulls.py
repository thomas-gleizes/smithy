#!/usr/bin/env python3
"""Fixtures des pull requests : dépôt privé `revue` et un second utilisateur `relecteur`.

    seed-pulls.py <url> <login> <mot de passe>

Création par l'API uniquement (pas de `git push`), idempotent : le dépôt est complet quand
l'étiquette `fixtures-ok` existe ; sinon il est supprimé et recréé (instance jetable).
Aucun workflow dans ce dépôt (les statuts de CI sont posés à la main par l'API des statuts).
Dépôt à part plutôt que `tree` : de nouvelles branches y changeraient l'ordre des branches
dont dépendent d'anciens scénarios.

Contenu :
  #1  ouverte   : 2 commits, ajout + modif + suppression + renommage, description Markdown,
                  étiquettes, commentaires des deux utilisateurs, revues (changements demandés
                  avec un commentaire de ligne, puis approbation), statuts de CI (succès + en cours)
  #2  fusionnée
  #3  fermée sans fusion
  #4  ouverte en conflit avec main (non fusionnable), statut de CI en échec
  #5  brouillon (titre « WIP: »)
  #6… 32 petites PR ouvertes (`lot/NN`) pour dépasser une page de liste
"""
import base64
import json
import sys
import urllib.error
import urllib.request

REPO = "revue"
REVIEWER = "relecteur"
REVIEWER_PASS = "relecteur-dev-password"


def main():
    url, user, password = sys.argv[1:4]
    url = url.rstrip("/")

    def auth_of(login, pw):
        return "Basic " + base64.b64encode(f"{login}:{pw}".encode()).decode()

    me = auth_of(user, password)
    them = auth_of(REVIEWER, REVIEWER_PASS)

    def call(method, path, body=None, ok=(200, 201, 204), auth=me):
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
            raise SystemExit(f"{method} {path} -> HTTP {code}: {text[:300]}")
        return code, (json.loads(text) if text.strip().startswith(("{", "[")) else text)

    b64 = lambda s: base64.b64encode(s.encode() if isinstance(s, str) else s).decode()
    repo = f"/repos/{user}/{REPO}"

    if call("GET", f"{repo}/labels?limit=50", ok=(200, 404))[0] == 200:
        labels = call("GET", f"{repo}/labels?limit=50")[1] or []
        if any(l["name"] == "fixtures-ok" for l in labels):
            print("dépôt revue déjà présent")
            return
        call("DELETE", repo)

    # --- second utilisateur, pour des revues et des commentaires qui ne sont pas de l'auteur
    if call("GET", f"/users/{REVIEWER}", ok=(200, 404))[0] == 404:
        call("POST", "/admin/users", {
            "username": REVIEWER, "password": REVIEWER_PASS, "email": f"{REVIEWER}@example.test",
            "must_change_password": False,
        })

    call("POST", "/user/repos", {"name": REPO, "private": True, "auto_init": True, "default_branch": "main"})
    call("PUT", f"{repo}/collaborators/{REVIEWER}", {"permission": "write"})

    def blob_sha(path, ref):
        return call("GET", f"{repo}/contents/{path}?ref={ref}")[1]["sha"]

    def commit(branch, message, files, new_branch=None):
        body = {"message": message, "files": files}
        if new_branch:
            body.update({"branch": branch, "new_branch": new_branch})
        else:
            body["branch"] = branch
        return call("POST", f"{repo}/contents", body)[1]

    def pull(head, title, body="", base="main"):
        return call("POST", f"{repo}/pulls", {"head": head, "base": base, "title": title, "body": body})[1]

    commit("main", "init: fichiers de départ", [
        {"operation": "create", "path": "src/lib.rs", "content": b64("pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n")},
        {"operation": "create", "path": "docs/guide.md", "content": b64("# Guide\n\nPremière version.\n")},
        {"operation": "create", "path": "obsolete.txt", "content": b64("à supprimer\n")},
        {"operation": "create", "path": "calcul.txt", "content": b64("1 + 1 = 3\n")},
        {"operation": "create", "path": "titre.txt", "content": b64("Titre d'origine\n")},
    ])

    # --- #1 ouverte, riche
    commit("main", "feat: page de garde", [
        {"operation": "create", "path": "garde.md", "content": b64("# Page de garde\n\nBienvenue.\n")},
        {"operation": "update", "path": "src/lib.rs", "sha": blob_sha("src/lib.rs", "main"),
         "content": b64("pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn sub(a: i32, b: i32) -> i32 {\n    a - b\n}\n")},
    ], new_branch="feature/garde")
    commit("feature/garde", "refactor: renomme le guide, retire l'obsolète", [
        {"operation": "update", "path": "docs/manuel.md", "from_path": "docs/guide.md",
         "sha": blob_sha("docs/guide.md", "feature/garde"), "content": b64("# Manuel\n\nPremière version.\n")},
        {"operation": "delete", "path": "obsolete.txt", "sha": blob_sha("obsolete.txt", "feature/garde")},
    ])
    body1 = ("Ajoute une **page de garde** et range la documentation.\n\n"
             "- nouvelle fonction `sub`\n- `docs/guide.md` devient `docs/manuel.md`\n\n"
             "```rust\nassert_eq!(sub(3, 1), 2);\n```\n\nVoir aussi le [manuel](docs/manuel.md).\n")
    p1 = pull("feature/garde", "feat: page de garde et manuel", body1)
    for name, color in (("enhancement", "#84b6eb"), ("docs", "#0075ca")):
        call("POST", f"{repo}/labels", {"name": name, "color": color})
    ids = {l["name"]: l["id"] for l in call("GET", f"{repo}/labels?limit=50")[1]}
    call("POST", f"{repo}/issues/{p1['number']}/labels", {"labels": [ids["enhancement"], ids["docs"]]})
    call("POST", f"{repo}/issues/{p1['number']}/comments", {"body": "Première remarque de l'auteur."})
    call("POST", f"{repo}/issues/{p1['number']}/comments",
         {"body": "Un avis du relecteur, sur\nplusieurs lignes, avec des accents é à ü et 日本語."}, auth=them)
    head1 = call("GET", f"{repo}/pulls/{p1['number']}")[1]["head"]["sha"]
    call("POST", f"{repo}/pulls/{p1['number']}/reviews", {
        "event": "REQUEST_CHANGES", "body": "Il manque un test pour `sub`.", "commit_id": head1,
        "comments": [{"path": "src/lib.rs", "body": "Et le dépassement ?", "new_position": 6}],
    }, auth=them)
    call("POST", f"{repo}/pulls/{p1['number']}/reviews",
         {"event": "APPROVED", "body": "Ça me va finalement.", "commit_id": head1}, auth=them)
    call("POST", f"{repo}/statuses/{head1}", {"state": "success", "context": "ci/build", "description": "compilation"})
    call("POST", f"{repo}/statuses/{head1}", {"state": "pending", "context": "ci/test", "description": "tests en cours"})

    # --- #2 fusionnée
    commit("main", "fix: corrige le calcul", [
        {"operation": "update", "path": "calcul.txt", "sha": blob_sha("calcul.txt", "main"), "content": b64("1 + 1 = 2\n")},
    ], new_branch="fix/calcul")
    p2 = pull("fix/calcul", "fix: corrige le calcul", "Le résultat était faux.")
    call("POST", f"{repo}/pulls/{p2['number']}/merge", {"Do": "merge"})

    # --- #3 fermée sans fusion
    commit("main", "essai abandonné", [
        {"operation": "create", "path": "essai.txt", "content": b64("essai\n")},
    ], new_branch="essai")
    p3 = pull("essai", "essai: idée abandonnée", "")
    call("PATCH", f"{repo}/pulls/{p3['number']}", {"state": "closed"})

    # --- #4 en conflit : la branche et main changent la même ligne
    commit("main", "feat: nouveau titre", [
        {"operation": "update", "path": "titre.txt", "sha": blob_sha("titre.txt", "main"), "content": b64("Titre de la branche\n")},
    ], new_branch="conflit")
    p4 = pull("conflit", "feat: titre en conflit", "Cette PR ne peut pas être fusionnée.")
    commit("main", "chore: titre changé sur main", [
        {"operation": "update", "path": "titre.txt", "sha": blob_sha("titre.txt", "main"), "content": b64("Titre de main\n")},
    ])
    head4 = call("GET", f"{repo}/pulls/{p4['number']}")[1]["head"]["sha"]
    call("POST", f"{repo}/statuses/{head4}", {"state": "failure", "context": "ci/build", "description": "échec"})

    # --- #5 brouillon
    commit("main", "wip", [{"operation": "create", "path": "brouillon.txt", "content": b64("à finir\n")}],
           new_branch="brouillon")
    pull("brouillon", "WIP: brouillon à finir", "Pas encore prêt.")

    # --- 32 petites PR pour la pagination de la liste
    for i in range(32):
        commit("main", f"lot {i:02d}", [{"operation": "create", "path": f"lot/{i:02d}.txt", "content": b64(f"{i}\n")}],
               new_branch=f"lot/{i:02d}")
        pull(f"lot/{i:02d}", f"chore: lot {i:02d}", "")
    # Marqueur de fin : sans lui, un dépôt à moitié créé est refait au prochain lancement.
    call("POST", f"{repo}/labels", {"name": "fixtures-ok", "color": "#cccccc"})
    print("dépôt revue créé (37 PR)")


main()
