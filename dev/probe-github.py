#!/usr/bin/env python3
"""Sonde l'API REST de GitHub pour un futur backend smithy et affiche un JSON.

    GITHUB_TOKEN=$(gh auth token) probe-github.py <dépôt avec Actions> <dépôt avec des PR>

Lecture seule (GET uniquement). Mesure, comme les sondes Forgejo, ce que smithy consomme :
dépôts (listes, recherche, fiche), runs, jobs et étapes, logs (job et run entier), branches,
contenu, historique et diff, pull requests (revues, commentaires, fichiers, checks), et les
contraintes transverses (pagination, quotas, accès sans token). Aucun contenu n'est recopié :
seulement des formes (clés, codes, comptes, premières lignes).
"""
import io
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import zipfile

API = "https://api.github.com"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


def main():
    ci_repo, pr_repo = sys.argv[1], sys.argv[2]
    token = os.environ["GITHUB_TOKEN"]
    base_hdr = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
                "User-Agent": "smithy-probe"}
    out = {}
    plain_opener = urllib.request.build_opener()
    noredir = urllib.request.build_opener(NoRedirect)

    def get(path, query=None, accept=None, auth=True, follow=True, raw=False):
        url = path if path.startswith("http") else f"{API}{path}"
        if query:
            url += ("&" if "?" in url else "?") + urllib.parse.urlencode(query)
        hdr = dict(base_hdr)
        if accept:
            hdr["Accept"] = accept
        if auth:
            hdr["Authorization"] = f"Bearer {token}"
        req = urllib.request.Request(url, headers=hdr)
        try:
            r = (plain_opener if follow else noredir).open(req, timeout=60)
        except urllib.error.HTTPError as e:
            r = e
        body = r.read()
        if raw:
            return r.status, body, r.headers
        try:
            return r.status, json.loads(body), r.headers
        except Exception:
            return r.status, body.decode("utf8", "replace")[:200], r.headers

    keys = lambda d: sorted(d.keys()) if isinstance(d, dict) else type(d).__name__
    first = lambda d: d[0] if isinstance(d, list) and d else {}

    # --- transverse : quota, pagination
    s, rl, h = get("/rate_limit")
    out["rate_core"] = rl["resources"]["core"]["limit"]
    out["rate_search"] = rl["resources"]["search"]["limit"]
    s, d, h = get("/rate_limit", auth=False)
    out["rate_core_without_token"] = d["resources"]["core"]["limit"]
    s, d, h = get("/user/repos", {"per_page": 100, "page": 1})
    out["user_repos"] = (s, len(d))
    out["pagination_link_header"] = bool(h.get("link"))
    s, d, h = get("/user/repos", {"per_page": 101})
    out["per_page_101_gives"] = len(d) if isinstance(d, list) else d
    out["scopes_header"] = h.get("x-oauth-scopes")

    # --- accueil
    out["repo_item_keys"] = keys(first(get("/user/repos", {"per_page": 1})[1]))
    for name, path in (("subscriptions", "/user/subscriptions"), ("starred", "/user/starred"), ("orgs", "/user/orgs")):
        s, d, _ = get(path, {"per_page": 5})
        out[f"{name}"] = (s, len(d) if isinstance(d, list) else d)
    s, d, _ = get("/search/repositories", {"q": "smithy tui", "per_page": 5})
    out["search"] = (s, keys(d), d.get("total_count") if isinstance(d, dict) else None)
    s, info, _ = get(f"/repos/{ci_repo}")
    out["repo_keys"] = keys(info)
    out["repo_counts"] = {k: info.get(k) for k in ("open_issues_count", "stargazers_count", "default_branch", "archived", "fork", "private", "size")}

    # --- CI : runs, jobs, étapes
    s, runs, h = get(f"/repos/{ci_repo}/actions/runs", {"per_page": 5})
    out["runs_status"] = s
    out["runs_envelope_keys"] = keys(runs)
    run = runs["workflow_runs"][0]
    out["run_keys"] = keys(run)
    out["run_sample"] = {k: run.get(k) for k in ("id", "run_number", "run_attempt", "name", "path", "display_title", "head_branch", "event", "status", "conclusion")}
    s, all_runs, _ = get(f"/repos/{ci_repo}/actions/runs", {"per_page": 100})
    out["run_status_values"] = sorted({(r["status"], r["conclusion"]) for r in all_runs["workflow_runs"]}, key=str)
    out["runs_total_count"] = all_runs.get("total_count")
    s, jobs, _ = get(f"/repos/{ci_repo}/actions/runs/{run['id']}/jobs", {"per_page": 100})
    out["jobs_envelope_keys"] = keys(jobs)
    job = jobs["jobs"][0]
    out["job_keys"] = keys(job)
    out["job_sample"] = {k: job.get(k) for k in ("id", "name", "status", "conclusion", "run_attempt", "workflow_name", "head_branch")}
    out["step_keys"] = keys(first(job.get("steps")))
    out["steps"] = [(st["number"], st["name"], st["status"], st["conclusion"]) for st in job.get("steps") or []]
    # Jobs de tous les runs d'un coup ? (non : par run seulement)
    out["jobs_per_run_list"] = [len(get(f"/repos/{ci_repo}/actions/runs/{r['id']}/jobs")[1].get("jobs", [])) for r in runs["workflow_runs"][:3]]

    # --- logs d'un job : redirection, format, marqueurs d'étapes
    s, _, h = get(f"/repos/{ci_repo}/actions/jobs/{job['id']}/logs", follow=False, raw=True)
    out["job_logs_redirect"] = (s, urllib.parse.urlparse(h.get("location") or "").netloc)
    s, _, h = get(f"/repos/{ci_repo}/actions/jobs/{job['id']}/logs", auth=False, follow=False, raw=True)
    out["job_logs_without_token"] = s
    # La redirection vise une URL signée (stockage Azure) : la suivre **sans** le token, qui
    # y est refusé (401 InvalidAuthenticationInfo).
    s, _, h = get(f"/repos/{ci_repo}/actions/jobs/{job['id']}/logs", follow=False, raw=True)
    out["job_logs_with_token_forwarded"] = get(h["location"], raw=True)[0]
    s, body, h = get(h["location"], auth=False, raw=True)
    text = body.decode("utf8", "replace")
    lines = text.splitlines()
    out["job_logs"] = {"status": s, "content_type": h.get("content-type"), "bytes": len(body), "lines": len(lines)}
    ts = re.compile(r"^\ufeff?\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z ")
    out["job_logs_timestamp_prefix_ratio"] = round(sum(bool(ts.match(l)) for l in lines) / max(len(lines), 1), 3)
    out["job_logs_first_line_shape"] = ts.sub("<ts> ", lines[0])[:60] if lines else None
    out["job_logs_bom"] = text.startswith("\ufeff")
    groups = [ts.sub("", l) for l in lines if "##[group]" in l]
    out["job_logs_groups"] = [g[:60] for g in groups]
    out["job_logs_markers"] = sorted({m for l in lines for m in re.findall(r"##\[(\w+)\]", l)})
    out["job_logs_ansi"] = any("\x1b[" in l for l in lines)

    # --- logs du run entier : archive zip, un fichier par étape ?
    s, body, h = get(f"/repos/{ci_repo}/actions/runs/{run['id']}/logs", raw=True)
    out["run_logs_status"] = (s, h.get("content-type"))
    if s == 200:
        z = zipfile.ZipFile(io.BytesIO(body))
        names = z.namelist()
        out["run_logs_zip_entries"] = names[:20]
        out["run_logs_zip_has_step_files"] = any("/" in n for n in names)

    # --- code : branches, contenu, brut
    s, br, h = get(f"/repos/{ci_repo}/branches", {"per_page": 100})
    out["branch_keys"] = keys(first(br))
    out["branch_commit_keys"] = keys(first(br).get("commit"))
    s, root, _ = get(f"/repos/{ci_repo}/contents", {"ref": info["default_branch"]})
    out["contents_dir"] = (s, type(root).__name__, len(root) if isinstance(root, list) else None)
    out["entry_keys"] = keys(first(root))
    out["entry_types"] = sorted({e["type"] for e in root})
    f = next(e for e in root if e["type"] == "file")
    s, body, h = get(f"/repos/{ci_repo}/contents/{urllib.parse.quote(f['path'])}", {"ref": info["default_branch"]},
                     accept="application/vnd.github.raw+json", raw=True)
    out["raw_file"] = (s, h.get("content-type"), h.get("content-length") is not None, len(body) == f["size"])
    s, d, _ = get(f"/repos/{ci_repo}/git/trees/{info['default_branch']}", {"recursive": "1"})
    out["tree_recursive"] = (s, len(d.get("tree", [])) if isinstance(d, dict) else d, d.get("truncated") if isinstance(d, dict) else None)

    # --- historique
    s, commits, h = get(f"/repos/{ci_repo}/commits", {"per_page": 100, "sha": info["default_branch"]})
    out["commits"] = (s, len(commits), bool(h.get("link")))
    out["commit_item_keys"] = keys(first(commits))
    out["commit_item_files_present"] = "files" in first(commits)
    sha = commits[0]["sha"]
    s, det, _ = get(f"/repos/{ci_repo}/commits/{sha}")
    out["commit_detail_keys"] = keys(det)
    out["commit_file_keys"] = keys(first(det.get("files")))
    out["commit_file_status_values"] = sorted({x["status"] for x in det.get("files") or []})
    s, body, h = get(f"/repos/{ci_repo}/commits/{sha}", accept="application/vnd.github.diff", raw=True)
    out["commit_diff"] = (s, h.get("content-type"), body[:10].decode("utf8", "replace"))

    # --- pull requests (dépôt actif)
    s, pulls, h = get(f"/repos/{pr_repo}/pulls", {"state": "all", "per_page": 30})
    out["pulls_list"] = (s, len(pulls))
    out["pull_list_keys"] = keys(first(pulls))
    out["pull_list_has_counts"] = {k: k in first(pulls) for k in ("additions", "changed_files", "comments", "mergeable", "draft", "merged_at")}
    n = next(p["number"] for p in pulls if p.get("merged_at"))
    s, pr, _ = get(f"/repos/{pr_repo}/pulls/{n}")
    out["pull_detail_extra_keys"] = sorted(set(pr) - set(first(pulls)))
    out["pull_detail_sample"] = {k: pr.get(k) for k in ("state", "merged", "mergeable", "mergeable_state", "draft", "additions", "deletions", "changed_files", "comments", "review_comments", "commits")}
    s, rv, _ = get(f"/repos/{pr_repo}/pulls/{n}/reviews", {"per_page": 100})
    out["reviews"] = (s, len(rv), sorted({r["state"] for r in rv}))
    out["review_keys"] = keys(first(rv))
    s, rc, _ = get(f"/repos/{pr_repo}/pulls/{n}/comments", {"per_page": 100})
    out["review_comments"] = (s, len(rc))
    out["review_comment_keys"] = keys(first(rc))
    s, ic, _ = get(f"/repos/{pr_repo}/issues/{n}/comments", {"per_page": 100})
    out["issue_comments"] = (s, len(ic))
    s, files, _ = get(f"/repos/{pr_repo}/pulls/{n}/files", {"per_page": 100})
    out["pull_files"] = (s, len(files), keys(first(files)))
    s, body, h = get(f"/repos/{pr_repo}/pulls/{n}", accept="application/vnd.github.diff", raw=True)
    out["pull_diff"] = (s, h.get("content-type"), body[:10].decode("utf8", "replace"))
    head = pr["head"]["sha"]
    s, st, _ = get(f"/repos/{pr_repo}/commits/{head}/status")
    out["combined_status"] = (s, st.get("state"), st.get("total_count"))
    s, cr, _ = get(f"/repos/{pr_repo}/commits/{head}/check-runs", {"per_page": 100})
    out["check_runs"] = (s, cr.get("total_count"), keys(first(cr.get("check_runs"))))
    out["check_run_values"] = sorted({(c["status"], c["conclusion"]) for c in cr.get("check_runs") or []}, key=str)
    out["check_run_app"] = sorted({(c.get("app") or {}).get("slug", "") for c in cr.get("check_runs") or []})

    # --- sans token : dépôt public lisible ?
    out["public_repo_without_token"] = get(f"/repos/{pr_repo}/actions/runs", {"per_page": 1}, auth=False)[0]

    s, rl, _ = get("/rate_limit")
    out["rate_core_used_by_probe_session"] = rl["resources"]["core"]["used"]
    print(json.dumps(out, indent=1, ensure_ascii=False, sort_keys=True))


main()
