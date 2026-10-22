#!/usr/bin/env python3
"""Sonde l'API REST v4 de GitLab pour un futur backend smithy et affiche un JSON.

    GITLAB_TOKEN=glpat-… dev/probe-gitlab.py <projet avec CI> [<projet avec MR>]

Les projets se donnent par chemin (`groupe/sous-groupe/projet`). `GITLAB_URL` (défaut
https://gitlab.com) permet de viser une instance auto-hébergée avec le même script.

Lecture seule (GET uniquement). Mesure ce que smithy consomme, comme les sondes GitHub et
Forgejo : authentification (en-têtes, droits du token), listes de projets et pagination,
pipelines/jobs/étapes, trace d'un job (marqueurs de sections, ANSI), branches, arbre, fichier
brut, commits et diff, merge requests (revues, discussions, pipelines), quotas. Aucun contenu
n'est recopié : seulement des formes (clés, codes, comptes, premières lignes).
"""
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE = os.environ.get("GITLAB_URL", "https://gitlab.com").rstrip("/")
API = f"{BASE}/api/v4"
TOKEN = os.environ.get("GITLAB_TOKEN", "")
UA = {"User-Agent": "smithy-probe"}
SECTION = re.compile(r"section_(start|end):\d+:[\w.\-]+")
ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


def get(path, query=None, auth="private", follow=True, raw=False, base=API):
    """auth : 'private' (PRIVATE-TOKEN), 'bearer' (Authorization: Bearer), None."""
    url = path if path.startswith("http") else f"{base}{path}"
    if query:
        url += ("&" if "?" in url else "?") + urllib.parse.urlencode(query)
    hdr = dict(UA)
    if auth == "private" and TOKEN and os.environ.get("GITLAB_AUTH") == "bearer":
        auth = "bearer"  # jeton OAuth : seul `Authorization: Bearer` est accepté
    if auth == "private" and TOKEN:
        hdr["PRIVATE-TOKEN"] = TOKEN
    elif auth == "bearer" and TOKEN:
        hdr["Authorization"] = f"Bearer {TOKEN}"
    opener = urllib.request.build_opener() if follow else urllib.request.build_opener(NoRedirect)
    try:
        r = opener.open(urllib.request.Request(url, headers=hdr), timeout=60)
    except urllib.error.HTTPError as e:
        r = e
    body = r.read()
    if raw:
        return r.status, body, r.headers
    try:
        return r.status, json.loads(body), r.headers
    except Exception:
        return r.status, body.decode("utf8", "replace")[:200], r.headers


def keys(x):
    """Forme d'une réponse : clés d'un objet, ou clés du premier élément d'une liste."""
    if isinstance(x, list):
        return {"len": len(x), "item_keys": sorted(x[0].keys()) if x and isinstance(x[0], dict) else None}
    if isinstance(x, dict):
        if "error_description" in x:
            return {"keys": sorted(x.keys()), "refus": x["error_description"]}
        return {"keys": sorted(x.keys())}
    return {"body": x}


def heads(h, *names):
    return {n: h.get(n) for n in names if h.get(n) is not None}


PAGE = ("x-total", "x-total-pages", "x-next-page", "x-per-page", "x-page", "link")
RATE = ("ratelimit-limit", "ratelimit-remaining", "ratelimit-reset", "ratelimit-name",
        "retry-after", "ratelimit-observed")


def enc(path):
    return urllib.parse.quote(path, safe="")


def trace_shape(body):
    text = body.decode("utf8", "replace")
    lines = text.split("\n")
    return {
        "bytes": len(body),
        "lines": len(lines),
        "bom": text.startswith("﻿"),
        "ansi": bool(ANSI.search(text)),
        "cr": "\r" in text,
        "timestamps_prefix": bool(re.match(r"\d{4}-\d\d-\d\dT", text)),
        "sections": [m.group(0) for m in SECTION.finditer(text)][:12],
        "n_sections": len(SECTION.findall(text)),
        "first_lines": [ANSI.sub("", l)[:100] for l in lines[:6]],
        "last_lines": [ANSI.sub("", l)[:100] for l in lines[-4:]],
    }


def main():
    ci = sys.argv[1]
    mr = sys.argv[2] if len(sys.argv) > 2 else ci
    out = {"base": BASE, "token": bool(TOKEN)}

    # --- Authentification -------------------------------------------------------------
    a = {}
    for name, mode in [("private_token", "private"), ("bearer", "bearer"), ("none", None)]:
        s, b, h = get("/user", auth=mode)
        a[name] = {"status": s, **(keys(b) if s == 200 else {"body": b})}
    s, b, h = get("/personal_access_tokens/self")
    a["token_self"] = {"status": s, "scopes": b.get("scopes") if isinstance(b, dict) else b,
                       "expires_at": b.get("expires_at") if isinstance(b, dict) else None}
    s, b, h = get("/version")
    a["version"] = {"status": s, "body": b if s == 200 else None}
    s, b, h = get("/metadata", auth=None)
    a["metadata_sans_token"] = {"status": s, "body": b if s == 200 else None}
    out["auth"] = a

    # --- Quotas -----------------------------------------------------------------------
    s, b, h = get("/user")
    out["rate_headers"] = heads(h, *RATE)

    # --- Projets ----------------------------------------------------------------------
    p = {}
    for name, q in [("membre", {"membership": "true"}), ("etoiles", {"starred": "true"}),
                    ("proprietaire", {"owned": "true"})]:
        s, b, h = get("/projects", {**q, "per_page": 100, "simple": "true"})
        p[name] = {"status": s, **keys(b), **heads(h, *PAGE)}
    s, b, h = get("/projects", {"membership": "true", "per_page": 101})
    p["per_page_101"] = {"status": s, "len": len(b) if isinstance(b, list) else b}
    s, b, h = get("/projects", {"search": "smithy", "per_page": 5})
    p["recherche_avec_token"] = {"status": s, **keys(b), **heads(h, *PAGE)}
    s, b, h = get("/projects", {"search": "rust", "per_page": 5}, auth=None)
    p["recherche_sans_token"] = {"status": s, **keys(b)}
    s, b, h = get("/projects", {"membership": "true", "pagination": "keyset", "per_page": 5,
                                "order_by": "id", "sort": "asc"})
    p["keyset"] = {"status": s, "link": h.get("link"), "len": len(b) if isinstance(b, list) else b}
    s, b, h = get(f"/projects/{enc(ci)}")
    p["fiche_par_chemin"] = {"status": s, **keys(b)}
    proj = b if s == 200 else {}
    p["fiche_valeurs"] = {k: proj.get(k) for k in (
        "id", "path_with_namespace", "default_branch", "visibility", "archived", "empty_repo",
        "open_issues_count", "star_count", "forks_count", "last_activity_at") if k in proj}
    p["namespace"] = {k: (proj.get("namespace") or {}).get(k) for k in ("kind", "full_path")}
    s2, b2, _ = get(f"/projects/{proj.get('id')}") if proj.get("id") else (None, None, None)
    p["fiche_par_id"] = {"status": s2}
    s, b, h = get(f"/projects/{enc(ci)}", auth=None)
    p["fiche_sans_token"] = {"status": s}
    out["projets"] = p
    pid = proj.get("id")
    default = proj.get("default_branch") or "main"
    if not pid:
        print(json.dumps(out, indent=1, ensure_ascii=False))
        return

    # --- CI : pipelines, jobs, trace --------------------------------------------------
    ci_out = {}
    s, b, h = get(f"/projects/{pid}/pipelines", {"per_page": 100})
    ci_out["pipelines"] = {"status": s, **keys(b), **heads(h, *PAGE)}
    pipes = b if isinstance(b, list) else []
    s, b, h = get(f"/projects/{pid}/pipelines", {"per_page": 101})
    ci_out["pipelines_per_page_101"] = len(b) if isinstance(b, list) else b
    ci_out["pipeline_statuses"] = sorted({x.get("status") for x in pipes})
    ci_out["pipeline_sources"] = sorted({str(x.get("source")) for x in pipes})
    if pipes:
        s, b, h = get(f"/projects/{pid}/pipelines/{pipes[0]['id']}")
        ci_out["pipeline_detail"] = {"status": s, **keys(b)}
    # Un pipeline avec jobs, de préférence en échec.
    job_pipe = next((x for x in pipes if x.get("status") == "failed"), pipes[0] if pipes else None)
    jobs = []
    if job_pipe:
        s, b, h = get(f"/projects/{pid}/pipelines/{job_pipe['id']}/jobs", {"per_page": 100})
        ci_out["jobs_du_pipeline"] = {"status": s, **keys(b), **heads(h, *PAGE)}
        jobs = b if isinstance(b, list) else []
        s, b, h = get(f"/projects/{pid}/pipelines/{job_pipe['id']}/jobs",
                      {"per_page": 100, "include_retried": "true"})
        ci_out["jobs_avec_relances"] = len(b) if isinstance(b, list) else b
        s, b, h = get(f"/projects/{pid}/pipelines/{job_pipe['id']}/bridges")
        ci_out["bridges"] = {"status": s, "len": len(b) if isinstance(b, list) else b}
    ci_out["job_statuses"] = sorted({x.get("status") for x in jobs})
    ci_out["job_stages"] = [x.get("stage") for x in jobs][:12]
    ci_out["job_ordre_ids"] = [x.get("id") for x in jobs][:8]
    s, b, h = get(f"/projects/{pid}/jobs", {"per_page": 5})
    ci_out["jobs_du_projet"] = {"status": s, **keys(b)}
    traced = [j for j in jobs if j.get("status") in ("failed", "success")][:2]
    ci_out["traces"] = []
    for j in traced:
        s, b, h = get(f"/projects/{pid}/jobs/{j['id']}/trace", raw=True)
        ci_out["traces"].append({
            "job_status": j["status"], "status": s,
            "content_type": h.get("content-type"), **(trace_shape(b) if s == 200 else {"body": b[:120].decode("utf8", "replace")})})
    for j in jobs:
        if j.get("status") in ("running", "pending"):
            s, b, h = get(f"/projects/{pid}/jobs/{j['id']}/trace", raw=True)
            ci_out["trace_job_en_cours"] = {"status": s, **(trace_shape(b) if s == 200 else {})}
            break
    if traced:
        j = traced[0]
        s, b, h = get(f"/projects/{pid}/jobs/{j['id']}/trace", raw=True, auth=None)
        ci_out["trace_sans_token"] = {"status": s}
        s, b, h = get(f"/projects/{pid}/jobs/{j['id']}/trace", raw=True, auth="bearer")
        ci_out["trace_bearer"] = {"status": s}
        # Étapes via l'API : le job n'en a pas ; sections seulement dans la trace.
        ci_out["job_keys"] = sorted(j.keys())
    out["ci"] = ci_out

    # --- Code -------------------------------------------------------------------------
    c = {}
    s, b, h = get(f"/projects/{pid}/repository/branches", {"per_page": 100})
    c["branches"] = {"status": s, **keys(b), **heads(h, *PAGE)}
    if isinstance(b, list) and b:
        c["branche_commit_keys"] = sorted((b[0].get("commit") or {}).keys())
    s, b, h = get(f"/projects/{pid}/repository/tree", {"ref": default, "per_page": 100})
    c["arbre_racine"] = {"status": s, **keys(b), **heads(h, *PAGE)}
    tree = b if isinstance(b, list) else []
    c["arbre_types"] = sorted({x.get("type") for x in tree})
    s, b, h = get(f"/projects/{pid}/repository/tree", {"ref": default, "per_page": 101})
    c["arbre_per_page_101"] = len(b) if isinstance(b, list) else b
    s, b, h = get(f"/projects/{pid}/repository/tree", {"ref": default, "recursive": "true", "per_page": 100})
    c["arbre_recursif"] = {"status": s, "len": len(b) if isinstance(b, list) else b, **heads(h, *PAGE)}
    s, b, h = get(f"/projects/{pid}/repository/tree", {"ref": "branche-inconnue-xyz"})
    c["arbre_ref_inconnue"] = {"status": s, "body": b}
    blob = next((x for x in tree if x.get("type") == "blob"), None)
    if blob:
        path = blob["path"]
        s, body, h = get(f"/projects/{pid}/repository/files/{enc(path)}/raw", {"ref": default}, raw=True)
        c["fichier_brut"] = {"status": s, "bytes": len(body), "content_type": h.get("content-type"),
                             "content_length": h.get("content-length")}
        s, b, h = get(f"/projects/{pid}/repository/files/{enc(path)}", {"ref": default})
        c["fichier_json"] = {"status": s, **keys(b)}
        s, b, h = get(f"/projects/{pid}/repository/files/{enc(path)}", {"ref": default}, auth=None)
        c["fichier_sans_token"] = {"status": s}
        s, body, h = get(f"/projects/{pid}/repository/files/{enc(path)}/raw", {"ref": default},
                         raw=True, auth="bearer")
        c["fichier_bearer"] = {"status": s}
    s, b, h = get(f"/projects/{pid}/repository/files/{enc('chemin/inconnu.txt')}/raw", {"ref": default})
    c["fichier_inconnu"] = {"status": s}
    out["code"] = c

    # --- Historique -------------------------------------------------------------------
    hi = {}
    s, b, h = get(f"/projects/{pid}/repository/commits", {"ref_name": default, "per_page": 100})
    hi["liste"] = {"status": s, **keys(b), **heads(h, *PAGE)}
    commits = b if isinstance(b, list) else []
    s, b, h = get(f"/projects/{pid}/repository/commits", {"ref_name": default, "per_page": 101})
    hi["per_page_101"] = len(b) if isinstance(b, list) else b
    s, b, h = get(f"/projects/{pid}/repository/commits", {"ref_name": default, "per_page": 3, "with_stats": "true"})
    hi["liste_avec_stats"] = {"status": s, "item_keys": sorted(b[0].keys()) if isinstance(b, list) and b else b}
    s, b, h = get(f"/projects/{pid}/repository/commits", {"ref_name": "branche-inconnue-xyz"})
    hi["ref_inconnue"] = {"status": s, "body": b}
    if commits:
        sha = commits[0]["id"]
        hi["parent_ids_dans_liste"] = "parent_ids" in commits[0]
        s, b, h = get(f"/projects/{pid}/repository/commits/{sha}", {"stats": "true"})
        hi["detail"] = {"status": s, **keys(b), "stats": b.get("stats") if isinstance(b, dict) else None}
        s, b, h = get(f"/projects/{pid}/repository/commits/{sha}/diff", {"per_page": 100})
        hi["diff_json"] = {"status": s, **keys(b), **heads(h, *PAGE)}
        if isinstance(b, list) and b:
            hi["diff_json_exemple"] = {k: (v if k != "diff" else str(v)[:80]) for k, v in b[0].items()}
        s, body, h = get(f"/projects/{pid}/repository/commits/{sha}/diff", raw=True, base=API)
        hi["diff_ctype"] = h.get("content-type")
        # Correctif unifié : routes web (acceptent-elles le token ?).
        for name, auth in [("private", "private"), ("sans", None), ("bearer", "bearer")]:
            s, body, h = get(f"{BASE}/{ci}/-/commit/{sha}.diff", raw=True, auth=auth, follow=False)
            hi[f"diff_web_{name}"] = {"status": s, "location": h.get("location"),
                                      "starts": body[:60].decode("utf8", "replace")}
        s, body, h = get(f"{BASE}/{ci}/-/commit/{sha}.patch", raw=True, follow=False)
        hi["patch_web"] = {"status": s, "starts": body[:60].decode("utf8", "replace")}
        s, b, h = get(f"/projects/{pid}/repository/compare", {"from": sha + "^", "to": sha})
        hi["compare"] = {"status": s, **keys(b)}
        s, b, h = get(f"/projects/{pid}/repository/commits/{sha}/statuses")
        hi["statuses"] = {"status": s, **keys(b)}
        s, b, h = get(f"/projects/{pid}/repository/commits/{sha}/refs")
        hi["refs"] = {"status": s, **keys(b)}
    s, b, h = get(f"/projects/{pid}/repository/commits/{'0' * 40}")
    hi["sha_inconnu"] = {"status": s}
    out["historique"] = hi

    # --- Merge requests ---------------------------------------------------------------
    m = {}
    s, b, h = get(f"/projects/{enc(mr)}")
    mpid = b.get("id") if s == 200 else None
    if mpid:
        for st in ("opened", "closed", "merged", "all"):
            s, b, h = get(f"/projects/{mpid}/merge_requests", {"state": st, "per_page": 100})
            m[f"liste_{st}"] = {"status": s, "len": len(b) if isinstance(b, list) else b,
                                **heads(h, "x-total", "x-total-pages", "x-next-page")}
            if st == "all" and isinstance(b, list) and b:
                m["liste_item_keys"] = sorted(b[0].keys())
                m["etats_vus"] = sorted({x.get("state") for x in b})
                m["merge_status_vus"] = sorted({str(x.get("detailed_merge_status")) for x in b})
                m["draft_vus"] = sorted({str(x.get("draft")) for x in b})
                iid = b[0]["iid"]
                base = f"/projects/{mpid}/merge_requests/{iid}"
                for name, path in [("detail", base), ("commits", base + "/commits"),
                                   ("diffs", base + "/diffs"), ("changes", base + "/changes"),
                                   ("raw_diffs", base + "/raw_diffs"), ("approvals", base + "/approvals"),
                                   ("notes", base + "/notes"), ("discussions", base + "/discussions"),
                                   ("pipelines", base + "/pipelines"),
                                   ("approval_state", base + "/approval_state")]:
                    s, body, h = get(path, {"per_page": 100} if name in ("notes", "discussions", "diffs") else None, raw=True)
                    try:
                        parsed = json.loads(body)
                    except Exception:
                        parsed = body[:80].decode("utf8", "replace")
                    m[name] = {"status": s, **keys(parsed), **heads(h, "content-type")} if name != "raw_diffs" else {
                        "status": s, "ctype": h.get("content-type"), "starts": body[:40].decode("utf8", "replace")}
                    if name == "discussions" and isinstance(parsed, list) and parsed:
                        notes = [n for d in parsed for n in d.get("notes", [])]
                        m["discussion_note_keys"] = sorted(notes[0].keys()) if notes else None
                        m["notes_types"] = sorted({str(n.get("type")) for n in notes})
                        m["notes_systeme"] = sum(1 for n in notes if n.get("system"))
                        pos = [n for n in notes if n.get("position")]
                        m["position_exemple"] = sorted(pos[0]["position"].keys()) if pos else None
                    if name == "detail" and isinstance(parsed, dict):
                        m["detail_valeurs"] = {k: (parsed.get(k) if not isinstance(parsed.get(k), (dict, list)) else "…")
                                               for k in ("state", "draft", "detailed_merge_status", "has_conflicts",
                                                         "changes_count", "user_notes_count", "source_branch",
                                                         "target_branch", "sha", "merge_commit_sha")}
                        m["head_pipeline"] = keys(parsed.get("head_pipeline")) if parsed.get("head_pipeline") else None
                        m["diff_refs"] = parsed.get("diff_refs")
    out["merge_requests"] = m

    # --- Quota final ------------------------------------------------------------------
    s, b, h = get("/user")
    out["rate_headers_fin"] = heads(h, *RATE)
    print(json.dumps(out, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
