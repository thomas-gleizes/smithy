#!/usr/bin/env python3
"""Sonde l'API de contenu d'une instance Forgejo de dev (dépôts `tree` et `vide`) et affiche un JSON.

    probe-browse.py <url> <token>

Sert à savoir, version par version, ce qui change dans ce que smithy-forgejo consomme pour
l'explorateur : pagination des branches, forme des listings, chemins encodés, branche avec un
slash, fichier brut, gros fichier, dépôt vide. À relancer sur toute nouvelle version.
Les dépôts viennent de `seed-tree.py` (dev.sh seed).
"""
import json
import sys
import urllib.error
import urllib.parse
import urllib.request


def main():
    base, token = sys.argv[1].rstrip("/"), sys.argv[2]
    hdr = {"Authorization": f"token {token}"}
    out = {}

    def get(path, query=None, headers=None, limit=None, raw=False):
        url = f"{base}/api/v1{path}"
        if query:
            url += "?" + urllib.parse.urlencode(query)
        req = urllib.request.Request(url, headers={**hdr, **(headers or {})})
        try:
            r = urllib.request.urlopen(req, timeout=30)
        except urllib.error.HTTPError as e:
            r = e
        body = r.read(limit) if limit else r.read()
        h = r.headers
        if raw:
            return r.status, body, h
        try:
            return r.status, json.loads(body), h
        except Exception:
            return r.status, body.decode("utf8", "replace")[:80], h

    def seg(path):  # encode chaque segment, garde les "/"
        return "/".join(urllib.parse.quote(s, safe="") for s in path.split("/"))

    s, v, _ = get("/version")
    out["version"] = v["version"]

    # --- dépôt et branche par défaut
    s, repo, _ = get("/repos/smithy/tree")
    out["default_branch"] = repo.get("default_branch")
    out["repo_keys_have_empty"] = "empty" in repo

    # --- branches : taille de page par défaut, limite, total
    s, d, h = get("/repos/smithy/tree/branches")
    out["branches_default_page"] = len(d)
    out["branches_total_header"] = h.get("x-total-count")
    for lim in (50, 100):
        s, d, h = get("/repos/smithy/tree/branches", {"limit": lim, "page": 1})
        out[f"branches_limit_{lim}_page1"] = (s, len(d))
    s, d, _ = get("/repos/smithy/tree/branches", {"limit": 50, "page": 2})
    out["branches_limit_50_page2"] = (s, len(d))
    names = []
    for page in range(1, 6):
        s, d, _ = get("/repos/smithy/tree/branches", {"limit": 50, "page": page})
        if not isinstance(d, list) or not d:
            break
        names += [b["name"] for b in d]
    out["branches_all"] = len(names)
    out["branches_has_slash_branch"] = "feature/x" in names
    out["branch_entry_keys"] = sorted(d[0]) if isinstance(d, list) and d else None

    # --- listing
    s, root, _ = get("/repos/smithy/tree/contents", {"ref": "main"})
    out["root_status"] = s
    out["root_entry_keys"] = sorted(root[0])
    out["root_types"] = sorted({e["type"] for e in root})
    out["root_names"] = [e["name"] for e in root]
    s, many, _ = get("/repos/smithy/tree/contents/many", {"ref": "main"})
    out["many_listing"] = (s, len(many))  # 130 attendus : plafonné ou non ?

    # --- ref avec un slash
    s, d, _ = get("/repos/smithy/tree/contents", {"ref": "feature/x"})
    out["slash_branch_listing"] = (s, "seulement-sur-feature.txt" in [e["name"] for e in d] if isinstance(d, list) else d)
    s, d, _ = get("/repos/smithy/tree/contents", {"ref": "main"})
    out["main_lacks_feature_file"] = "seulement-sur-feature.txt" not in [e["name"] for e in d]

    # --- chemins délicats (segments encodés)
    for label, path in (
        ("espaces", "dossier avec espaces/fichier a b.txt"),
        ("diese_et_question", "dossier avec espaces/fichier #1 ?.txt"),
        ("unicode", "été/résumé.md"),
    ):
        s, d, _ = get(f"/repos/smithy/tree/contents/{seg(path)}", {"ref": "main"})
        out[f"path_{label}"] = (s, d.get("name") if isinstance(d, dict) else d)
        s, body, _ = get(f"/repos/smithy/tree/raw/{seg(path)}", {"ref": "main"}, raw=True)
        out[f"raw_{label}"] = (s, body[:30].decode("utf8", "replace"))

    # --- fichier : contents (base64) et raw
    s, d, _ = get("/repos/smithy/tree/contents/src/main.rs", {"ref": "main"})
    out["file_keys"] = sorted(d) if isinstance(d, dict) else d
    out["file_encoding"] = d.get("encoding") if isinstance(d, dict) else None
    s, body, h = get("/repos/smithy/tree/raw/src/main.rs", {"ref": "main"}, raw=True)
    out["raw_small"] = (s, h.get("content-type"), h.get("content-length"), body[:20])
    s, body, h = get("/repos/smithy/tree/raw/src/main.rs", {"ref": "feature/x"}, raw=True)
    out["raw_slash_branch"] = s

    # --- gros fichier (≈ 1,5 Mio)
    s, d, _ = get("/repos/smithy/tree/contents/big.txt", {"ref": "main"})
    out["big_contents"] = (s, d.get("size") if isinstance(d, dict) else d, len(d.get("content") or "") if isinstance(d, dict) else None)
    s, body, h = get("/repos/smithy/tree/raw/big.txt", {"ref": "main"}, limit=1000, raw=True)
    out["big_raw"] = (s, h.get("content-length"), h.get("transfer-encoding"), len(body))

    # --- binaire, fichier vide, retours chariot
    s, body, h = get("/repos/smithy/tree/raw/bin/data.bin", {"ref": "main"}, raw=True)
    out["binary_raw"] = (s, len(body), b"\x00" in body[:8192])
    s, body, h = get("/repos/smithy/tree/raw/empty.txt", {"ref": "main"}, raw=True)
    out["empty_raw"] = (s, len(body), h.get("content-length"))
    s, body, _ = get("/repos/smithy/tree/raw/crlf.txt", {"ref": "main"}, raw=True)
    out["crlf_raw"] = (s, body)
    s, d, _ = get("/repos/smithy/tree/contents/empty.txt", {"ref": "main"})
    out["empty_contents"] = (s, d.get("size") if isinstance(d, dict) else d, repr(d.get("content")) if isinstance(d, dict) else None)

    # --- erreurs
    out["unknown_path"] = get("/repos/smithy/tree/contents/nexiste/pas", {"ref": "main"})[0]
    out["unknown_ref"] = get("/repos/smithy/tree/contents", {"ref": "nexiste"})[0]
    out["raw_unknown_path"] = get("/repos/smithy/tree/raw/nexiste", {"ref": "main"}, raw=True)[0]
    out["raw_of_directory"] = get("/repos/smithy/tree/raw/src", {"ref": "main"}, raw=True)[0]
    bad = urllib.request.Request(f"{base}/api/v1/repos/smithy/tree/contents", headers={"Authorization": "token invalide"})
    try:
        out["bad_token"] = urllib.request.urlopen(bad, timeout=10).status
    except urllib.error.HTTPError as e:
        out["bad_token"] = e.code
    nt = urllib.request.Request(f"{base}/api/v1/repos/smithy/tree/contents")
    try:
        out["no_token_private"] = urllib.request.urlopen(nt, timeout=10).status
    except urllib.error.HTTPError as e:
        out["no_token_private"] = e.code

    # --- dépôt vide
    s, repo_vide, _ = get("/repos/smithy/vide")
    out["empty_repo_meta"] = (s, repo_vide.get("empty"), repo_vide.get("default_branch"))
    s, d, _ = get("/repos/smithy/vide/branches")
    out["empty_repo_branches"] = (s, d if not isinstance(d, list) else len(d))
    s, d, _ = get("/repos/smithy/vide/contents")
    out["empty_repo_contents"] = (s, d if not isinstance(d, list) else len(d))

    # --- accueil : listes de dépôts (voir smithy-forgejo/src/home.rs)
    need = ["name", "owner", "description", "private", "fork", "archived", "empty",
            "stars_count", "open_issues_count", "open_pr_counter", "updated_at", "default_branch"]
    for label, path, query in (
        ("mine", "/user/repos", {"limit": 50}),
        ("watched", "/user/subscriptions", {"limit": 50}),
        ("starred", "/user/starred", {"limit": 50}),
    ):
        s, d, h = get(path, query)
        out[f"home_{label}"] = {
            "status": s,
            "is_list": isinstance(d, list),
            "count": len(d) if isinstance(d, list) else d,
            "total_header": h.get("x-total-count"),
            "missing_fields": [k for k in need if isinstance(d, list) and d and k not in d[0]],
        }
    s, d, _ = get("/repos/search", {"q": "tree", "limit": 50, "sort": "updated", "order": "desc"})
    out["home_search"] = {
        "status": s,
        "shape": sorted(d) if isinstance(d, dict) else type(d).__name__,
        "count": len(d["data"]) if isinstance(d, dict) and "data" in d else None,
        "ok_flag": d.get("ok") if isinstance(d, dict) else None,
        "missing_fields": [k for k in need if isinstance(d, dict) and d.get("data") and k not in d["data"][0]],
    }
    s, d, _ = get("/repos/search", {"q": "zzz-aucun-resultat", "limit": 50})
    out["home_search_empty"] = {"status": s, "body": d if not isinstance(d, dict) else {"ok": d.get("ok"), "data": d.get("data")}}
    s, d, _ = get("/repos/search", {"limit": 50, "page": 1})
    out["home_search_no_query_count"] = len(d["data"]) if isinstance(d, dict) and d.get("data") is not None else d
    s, d, _ = get("/repos/smithy/tree")
    out["repo_info_missing_fields"] = [k for k in need if k not in d]
    out["repo_info_types"] = {k: type(d.get(k)).__name__ for k in ("stars_count", "open_pr_counter", "open_issues_count", "updated_at", "empty")}
    s, d, _ = get("/repos/smithy/nexiste")
    out["repo_info_unknown"] = s

    print(json.dumps(out, ensure_ascii=False, indent=1, default=str))


main()
