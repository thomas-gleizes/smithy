#!/usr/bin/env python3
"""Sonde l'API des pull requests d'une instance Forgejo de dev (dépôt `revue`) et affiche un JSON.

    probe-pulls.py <url> <login> <mot de passe>

Crée (puis supprime) un token avec **les droits du token du login de smithy**
(`read:repository`, `read:user`) : ce qui répond 403 avec lui demanderait un droit de plus.
Mesure la liste (taille de page, plafond, états, champs), le détail, les fichiers, le diff,
les commits, les commentaires, les revues et les statuts de CI. À relancer sur toute nouvelle
version. Les fixtures viennent de `seed-pulls.py`.
"""
import base64
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def main():
    base, user, password = sys.argv[1].rstrip("/"), sys.argv[2], sys.argv[3]
    basic = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()

    def request(method, path, query=None, body=None, auth=None, raw=False):
        url = f"{base}/api/v1{path}"
        if query:
            url += "?" + urllib.parse.urlencode(query)
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method,
                                     headers={"Authorization": auth, "Content-Type": "application/json"})
        try:
            r = urllib.request.urlopen(req, timeout=60)
        except urllib.error.HTTPError as e:
            r = e
        text = r.read().decode("utf8", "replace")
        if raw:
            return r.status, text, r.headers
        try:
            return r.status, json.loads(text), r.headers
        except Exception:
            return r.status, text[:120], r.headers

    s, tok, _ = request("POST", f"/users/{user}/tokens", body={
        "name": f"probe-pulls-{time.time_ns()}", "scopes": ["read:repository", "read:user"]}, auth=basic)
    token = tok["sha1"]
    hdr = f"token {token}"
    get = lambda path, query=None, raw=False: request("GET", path, query, auth=hdr, raw=raw)
    out = {}
    repo = f"/repos/{user}/revue"
    keys = lambda d: sorted(d.keys()) if isinstance(d, dict) else d

    out["version"] = get("/version")[1]["version"]

    # --- liste : taille de page, plafond, états, tri
    s, d, h = get(f"{repo}/pulls")
    out["list_default"] = (s, len(d) if isinstance(d, list) else d)
    out["list_default_state_open_only"] = all(p["state"] == "open" for p in d) if isinstance(d, list) else None
    out["list_headers"] = {k: h.get(k) for k in ("x-total-count", "x-hasmore", "link") if h.get(k)}
    for lim in (30, 50, 100):
        s, d, _ = get(f"{repo}/pulls", {"limit": lim, "state": "all"})
        out[f"list_limit_{lim}"] = (s, len(d) if isinstance(d, list) else d)
    for state in ("open", "closed", "all"):
        s, d, h = get(f"{repo}/pulls", {"state": state, "limit": 50})
        out[f"state_{state}"] = (s, len(d) if isinstance(d, list) else d, h.get("x-total-count"))
    s, d, _ = get(f"{repo}/pulls", {"state": "all", "limit": 50, "page": 2})
    out["state_all_page2"] = (s, len(d) if isinstance(d, list) else d)
    s, d, _ = get(f"{repo}/pulls", {"state": "all", "limit": 3})
    out["list_default_order_numbers"] = [p["number"] for p in d]
    for sort in ("recentupdate", "newest", "oldest", "leastupdate", "mostcomment", "priority"):
        s, d, _ = get(f"{repo}/pulls", {"state": "all", "limit": 3, "sort": sort})
        out[f"sort_{sort}"] = (s, [p["number"] for p in d] if isinstance(d, list) else d)
    s, d, _ = get(f"{repo}/pulls", {"state": "all", "limit": 50})
    allp = {p["number"]: p for p in d}
    s, d2, _ = get(f"{repo}/pulls", {"state": "all", "limit": 50, "page": 2})
    allp.update({p["number"]: p for p in (d2 or [])})
    out["list_item_keys"] = keys(allp[1])
    for k in ("additions", "deletions", "changed_files", "comments", "review_comments", "draft",
              "mergeable", "merged", "merged_at", "closed_at", "is_locked", "requested_reviewers",
              "assignees", "milestone", "html_url", "diff_url"):
        out[f"list_item_{k}"] = allp[1].get(k, "<absent>")
    out["head_keys"] = keys(allp[1]["head"])
    out["head"] = {k: allp[1]["head"].get(k) for k in ("ref", "label", "sha")}
    out["head_repo_keys"] = keys(allp[1]["head"].get("repo"))
    out["base"] = {k: allp[1]["base"].get(k) for k in ("ref", "label")}
    out["user_keys"] = keys(allp[1]["user"])
    out["labels"] = [(l["name"], l["color"]) for l in allp[1]["labels"]]
    out["merged_pr"] = {k: allp[2].get(k) for k in ("state", "merged", "merged_at", "merged_by", "closed_at", "merge_commit_sha", "mergeable")}
    out["merged_by_keys"] = keys(allp[2].get("merged_by"))
    out["closed_pr"] = {k: allp[3].get(k) for k in ("state", "merged", "merged_at", "closed_at", "mergeable")}
    out["conflict_pr"] = {k: allp[4].get(k) for k in ("state", "mergeable")}
    out["draft_pr"] = {k: allp[5].get(k) for k in ("title", "draft")}

    # --- détail
    s, p1, _ = get(f"{repo}/pulls/1")
    out["detail_status"] = s
    out["detail_same_keys_as_list"] = keys(p1) == out["list_item_keys"]
    out["detail_extra"] = {k: p1.get(k, "<absent>") for k in ("additions", "deletions", "changed_files", "comments", "review_comments", "mergeable")}
    out["detail_body"] = p1.get("body")
    out["unknown_pr"] = get(f"{repo}/pulls/9999")[0]

    # --- fichiers
    s, files, h = get(f"{repo}/pulls/1/files")
    out["files_status"] = s
    out["files_headers"] = {k: h.get(k) for k in ("x-total-count", "x-hasmore") if h.get(k)}
    out["files"] = [{k: f.get(k) for k in ("filename", "status", "additions", "deletions", "changes", "previous_filename")} for f in files] if isinstance(files, list) else files
    out["file_keys"] = keys(files[0]) if isinstance(files, list) and files else None

    # --- diff
    s, text, h = get(f"{repo}/pulls/1.diff", raw=True)
    out["diff_status"] = s
    out["diff_content_type"] = h.get("content-type")
    out["diff_first_line"] = text.splitlines()[0] if text else None
    out["diff_files"] = [l for l in text.splitlines() if l.startswith("diff --git")]
    out["diff_rename_markers"] = [l for l in text.splitlines() if l.startswith(("rename from", "rename to", "similarity"))]
    out["diff_merged_pr"] = (lambda r: (r[0], r[1].splitlines()[0] if r[1] else None))(get(f"{repo}/pulls/2.diff", raw=True))
    out["diff_closed_pr"] = (lambda r: (r[0], r[1].splitlines()[0] if r[1] else None))(get(f"{repo}/pulls/3.diff", raw=True))
    out["diff_unknown_pr"] = get(f"{repo}/pulls/9999.diff", raw=True)[0]
    out["patch_status"] = get(f"{repo}/pulls/1.patch", raw=True)[0]

    # --- commits
    s, commits, h = get(f"{repo}/pulls/1/commits")
    out["commits_status"] = s
    out["commits"] = [c["commit"]["message"].splitlines()[0] for c in commits] if isinstance(commits, list) else commits
    out["commit_item_keys"] = keys(commits[0]) if isinstance(commits, list) and commits else None
    s, c2, _ = get(f"{repo}/pulls/1/commits", {"stat": "false", "verification": "false", "files": "false"})
    out["commits_quiet_status"] = s

    # --- commentaires (API des issues) : le droit read:repository suffit-il ?
    s, comments, _ = get(f"{repo}/issues/1/comments")
    out["comments_status"] = s
    out["comments"] = [(c["user"]["login"], c["body"][:40]) for c in comments] if isinstance(comments, list) else comments
    out["comment_keys"] = keys(comments[0]) if isinstance(comments, list) and comments else None
    out["timeline_status"] = get(f"{repo}/issues/1/timeline")[0]
    out["issue_of_pr_status"] = get(f"{repo}/issues/1")[0]

    # --- revues
    s, reviews, _ = get(f"{repo}/pulls/1/reviews")
    out["reviews_status"] = s
    out["reviews"] = [{k: r.get(k) for k in ("state", "body", "comments_count", "stale", "official", "dismissed")} | {"user": (r.get("user") or {}).get("login")} for r in reviews] if isinstance(reviews, list) else reviews
    out["review_keys"] = keys(reviews[0]) if isinstance(reviews, list) and reviews else None
    if isinstance(reviews, list):
        rc = next((r for r in reviews if r.get("comments_count")), None)
        if rc:
            s, rcs, _ = get(f"{repo}/pulls/1/reviews/{rc['id']}/comments")
            out["review_comments_status"] = s
            out["review_comments"] = [{k: c.get(k) for k in ("path", "body", "position", "original_position", "diff_hunk")} for c in rcs] if isinstance(rcs, list) else rcs
            out["review_comment_keys"] = keys(rcs[0]) if isinstance(rcs, list) and rcs else None
    out["requested_reviewers_status"] = get(f"{repo}/pulls/1/requested_reviewers")[0]

    # --- statuts de CI de la tête
    sha = p1["head"]["sha"]
    s, st, _ = get(f"{repo}/commits/{sha}/status")
    out["combined_status"] = (s, st.get("state") if isinstance(st, dict) else st)
    out["combined_keys"] = keys(st)
    out["combined_statuses"] = [{k: x.get(k) for k in ("context", "status", "state", "description", "target_url")} for x in st.get("statuses") or []] if isinstance(st, dict) else None
    out["combined_by_branch"] = (lambda r: (r[0], r[1].get("state") if isinstance(r[1], dict) else r[1]))(get(f"{repo}/commits/feature%2Fgarde/status"))
    s, st4, _ = get(f"{repo}/commits/{allp[4]['head']['sha']}/status")
    out["combined_failure"] = st4.get("state") if isinstance(st4, dict) else st4
    s, st2, _ = get(f"{repo}/commits/{allp[6]['head']['sha']}/status")
    out["combined_without_status"] = (s, st2.get("state") if isinstance(st2, dict) else st2, st2.get("total_count") if isinstance(st2, dict) else None)

    # --- dépôt vide, dépôt sans PR
    s, d, _ = get(f"/repos/{user}/vide/pulls")
    out["empty_repo_pulls"] = (s, d)
    s, d, _ = get(f"/repos/{user}/tree/pulls")
    out["no_pr_repo"] = (s, d)

    request("DELETE", f"/users/{user}/tokens/{tok['id']}", auth=basic)
    print(json.dumps(out, indent=1, ensure_ascii=False, sort_keys=True))


main()
