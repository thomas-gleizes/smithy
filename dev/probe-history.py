#!/usr/bin/env python3
"""Sonde l'API d'historique d'une instance Forgejo de dev (dépôt `tree`, branche `history`) et affiche un JSON.

    probe-history.py <url> <token>

Sert à savoir, version par version, ce qui change dans ce que smithy-forgejo consomme pour les
commits et les branches : pagination (en-têtes, taille de page, plafond de `limit`), options
`stat`/`files`/`verification`, forme d'un commit et de son détail, diff, branche avec un slash,
dépôt vide. À relancer sur toute nouvelle version. Les dépôts viennent de `seed-tree.py`.
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

    def get(path, query=None, raw=False):
        url = f"{base}/api/v1{path}"
        if query:
            url += "?" + urllib.parse.urlencode(query)
        try:
            r = urllib.request.urlopen(urllib.request.Request(url, headers=hdr), timeout=60)
        except urllib.error.HTTPError as e:
            r = e
        body = r.read()
        if raw:
            return r.status, body.decode("utf8", "replace"), r.headers
        try:
            return r.status, json.loads(body), r.headers
        except Exception:
            return r.status, body.decode("utf8", "replace")[:100], r.headers

    quiet = {"stat": "false", "verification": "false", "files": "false"}
    s, v, _ = get("/version")
    out["version"] = v["version"]

    # --- liste des commits : taille de page, plafond, en-têtes, ordre
    s, d, h = get("/repos/smithy/tree/commits", {"sha": "history"})
    out["list_default_page"] = len(d)
    out["list_headers"] = {k: h.get(k) for k in ("x-total-count", "x-hasmore", "x-page", "x-perpage", "x-pagecount", "link") if h.get(k)}
    out["list_has_link_next"] = 'rel="next"' in (h.get("link") or "")
    for lim in (30, 50, 100):
        s, d, _ = get("/repos/smithy/tree/commits", {"sha": "history", "limit": lim, **quiet})
        out[f"list_limit_{lim}"] = (s, len(d) if isinstance(d, list) else d)
    s, d, h = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 50, "page": 2, **quiet})
    out["list_limit50_page2"] = (s, len(d), h.get("x-hasmore"))
    s, d, h = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 50, "page": 3, **quiet})
    out["list_limit50_page3"] = (s, d if not isinstance(d, list) else len(d))

    # --- options stat/files/verification honorées ?
    s, d, _ = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 1})
    out["list_item_keys_default"] = sorted(d[0].keys())
    s, d, _ = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 1, **quiet})
    out["list_item_keys_quiet"] = sorted(d[0].keys())
    newest = d[0]
    out["item_commit_keys"] = sorted(newest["commit"].keys())
    out["item_created_field"] = newest.get("created")
    out["item_author_date"] = newest["commit"]["author"].get("date")
    out["item_message_last_char"] = repr(newest["commit"]["message"][-1:])

    # --- ordre et messages particuliers
    s, d, _ = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 50, "page": 2, **quiet})
    s1, d1, _ = get("/repos/smithy/tree/commits", {"sha": "history", "limit": 50, **quiet})
    allc = d1 + d
    msgs = [c["commit"]["message"] for c in allc]
    out["newest_first"] = msgs[0].startswith("feat: ajoute") and "premier historique" in "".join(msgs[-4:])
    out["multiline_message"] = next((m for m in msgs if "puce deux" in m), None)
    out["unicode_message"] = next((m for m in msgs if "日本語" in m), None)
    out["page_boundary_overlap"] = len({c["sha"] for c in allc}) == len(allc)

    # --- détail d'un commit (ajout + modification + suppression) et binaire
    refactor = next(c for c in allc if c["commit"]["message"].startswith("refactor"))
    s, det, _ = get(f"/repos/smithy/tree/git/commits/{refactor['sha']}")
    out["detail_status"] = s
    out["detail_keys"] = sorted(det.keys()) if isinstance(det, dict) else det
    out["detail_files"] = det.get("files") if isinstance(det, dict) else None
    out["detail_stats"] = det.get("stats") if isinstance(det, dict) else None
    out["detail_parents"] = [p.get("sha", "")[:7] for p in (det.get("parents") or [])] if isinstance(det, dict) else None
    binary = next(c for c in allc if c["commit"]["message"].startswith("fix: modifie le binaire"))
    s, bd, _ = get(f"/repos/smithy/tree/git/commits/{binary['sha']}")
    out["binary_detail_files"] = bd.get("files") if isinstance(bd, dict) else bd
    out["binary_detail_stats"] = bd.get("stats") if isinstance(bd, dict) else None

    # --- diff : en-tête, binaire, racine, inconnu
    s, text, h = get(f"/repos/smithy/tree/git/commits/{refactor['sha']}.diff", raw=True)
    out["diff_status"] = s
    out["diff_content_type"] = h.get("content-type")
    out["diff_first_line"] = text.splitlines()[0] if text else None
    out["diff_has_header_before_diff_git"] = not text.startswith("diff --git")
    out["diff_files"] = [l for l in text.splitlines() if l.startswith("diff --git")]
    out["diff_deleted_marker"] = "deleted file mode" in text
    out["diff_new_marker"] = "new file mode" in text
    s, text, _ = get(f"/repos/smithy/tree/git/commits/{binary['sha']}.diff", raw=True)
    out["binary_diff"] = [l for l in text.splitlines() if "inary" in l or l.startswith("diff --git")][:3]
    root = allc[-1]
    s, text, _ = get(f"/repos/smithy/tree/git/commits/{root['sha']}.diff", raw=True)
    out["root_commit_diff_first_line"] = text.splitlines()[0] if text else None
    s, text, _ = get("/repos/smithy/tree/git/commits/0000000000000000000000000000000000000000.diff", raw=True)
    out["unknown_sha_diff"] = s
    out["patch_status"] = get(f"/repos/smithy/tree/git/commits/{refactor['sha']}.patch", raw=True)[0]

    # --- branche avec slash, inconnue, dépôt vide
    s, d, _ = get("/repos/smithy/tree/commits", {"sha": "feature/x", "limit": 2, **quiet})
    out["slash_branch"] = (s, len(d) if isinstance(d, list) else d)
    s, d, _ = get("/repos/smithy/tree/commits", {"sha": "branche-inconnue", "limit": 2, **quiet})
    out["unknown_branch"] = (s, d if not isinstance(d, list) else len(d))
    s, d, h = get("/repos/smithy/vide/commits", quiet)
    out["empty_repo"] = (s, d if not isinstance(d, list) else len(d))
    s, d, _ = get("/repos/smithy/tree/commits", {"limit": 1, **quiet})
    out["no_sha_param_default_branch"] = (s, len(d) if isinstance(d, list) else d)

    # --- branches : détail du commit de tête
    s, d, h = get("/repos/smithy/tree/branches", {"limit": 50})
    b = next(x for x in d if x["name"] == "history")
    out["branch_item_keys"] = sorted(b.keys())
    out["branch_commit_keys"] = sorted(b["commit"].keys())
    out["branch_commit_ts"] = b["commit"].get("timestamp")
    out["branch_protected"] = b.get("protected")
    print(json.dumps(out, indent=1, ensure_ascii=False, sort_keys=True))


main()
