#!/usr/bin/env python3
"""Sonde les routes web de logs d'une instance Forgejo de dev et affiche un JSON.

    probe-logs.py <url> <login> <mot de passe> <token> [dépôt privé=smithy/secret]

Sert à savoir, version par version, ce qui change dans ce que smithy-forgejo consomme :
nom du cookie de session, jeton CSRF, segment `attempt`, forme des réponses. Les résultats
fondent les adaptateurs de `smithy-forgejo` ; à relancer sur toute nouvelle version.
Instance jetable uniquement : le mot de passe passe en argument.
"""
import http.cookiejar
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *a, **k):
        return None


def main():
    base, user, password, token = sys.argv[1:5]
    repo = sys.argv[5] if len(sys.argv) > 5 else "smithy/secret"
    base = base.rstrip("/")
    jar = http.cookiejar.CookieJar()
    opener = urllib.request.build_opener(NoRedirect, urllib.request.HTTPCookieProcessor(jar))

    def call(path, data=None, headers=None, cookies=None, json_body=None, method=None):
        h = dict(headers or {})
        body = data
        if json_body is not None:
            body = json.dumps(json_body).encode()
            h["Content-Type"] = "application/json"
        if cookies is not None:
            h["Cookie"] = "; ".join(f"{k}={v}" for k, v in cookies.items())
        req = urllib.request.Request(base + path, data=body, headers=h, method=method)
        try:
            r = opener.open(req, timeout=15)
        except urllib.error.HTTPError as e:
            r = e
        text = r.read().decode("utf8", "replace")
        return r.status, text, r.headers

    out = {}
    s, t, _ = call("/api/v1/version")
    out["version"] = json.loads(t)["version"]

    # --- formulaire de login
    s, form, _ = call("/user/login")
    m = re.search(r'name="_csrf" value="([^"]+)"', form)
    out["login_csrf_field"] = bool(m)
    out["login_remember_field"] = 'name="remember"' in form
    fields = {"user_name": user, "password": password, "remember": "on"}
    if m:
        fields["_csrf"] = m.group(1)
    s, _, h = call("/user/login", data=urllib.parse.urlencode(fields).encode(), headers={"Content-Type": "application/x-www-form-urlencoded"})
    out["login_status"] = s
    out["login_location"] = h.get("Location")
    set_cookies = h.get_all("Set-Cookie") or []
    names = [c.split("=", 1)[0] for c in set_cookies]
    out["login_cookies"] = sorted(set(names))
    session = {}
    for c in set_cookies:
        k, v = c.split(";")[0].split("=", 1)
        if v and "max-age=0" not in c.lower():
            session[k] = v
        else:
            session.pop(k, None)
    session.pop("lang", None)
    session.pop("_csrf", None)
    out["session_cookie_names"] = sorted(session)
    s, _, _ = call("/user/login", data=urllib.parse.urlencode({**fields, "password": "faux"}).encode(), headers={"Content-Type": "application/x-www-form-urlencoded"})
    out["bad_password_status"] = s

    # --- création de token (basic auth)
    import base64
    auth = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()
    s, _, _ = call(f"/api/v1/users/{user}/tokens", json_body={"name": "probe", "scopes": ["read:repository"]}, headers={"Authorization": auth})
    out["token_creation_status"] = s

    # --- un run avec 2 jobs (rich.yml)
    s, t, _ = call(f"/api/v1/repos/{repo}/actions/tasks", headers={"Authorization": f"token {token}"})
    runs = json.loads(t).get("workflow_runs", [])
    out["tasks_status"] = s
    out["tasks_keys"] = sorted(runs[0]) if runs else None
    rich = [r for r in runs if r.get("workflow_id") == "rich.yml" and r.get("name") == "test"]
    if not rich:
        out["error"] = "aucun run rich.yml/test : seed terminé ?"
        print(json.dumps(out, ensure_ascii=False, indent=1))
        return
    number = rich[0]["run_number"]
    out["run_number_in_url"] = rich[0]["url"].rstrip("/").endswith(f"/runs/{number}")

    # --- jeton CSRF des pages (cookie `_csrf` + valeur dans la page)
    s, page, h = call(f"/{repo}/actions/runs/{number}", cookies=session)
    out["page_status"] = s
    m = re.search(r"csrfToken: *'([^']*)'", page)
    page_csrf = m.group(1) if m else None
    out["page_has_csrf_token"] = bool(page_csrf)
    csrf_cookie = None
    for c in h.get_all("Set-Cookie") or []:
        if c.startswith("_csrf="):
            csrf_cookie = c.split(";")[0].split("=", 1)[1]
    out["page_sets_csrf_cookie"] = bool(csrf_cookie)

    cursors = {"logCursors": []}

    def post(path, ck, csrf_header, body=cursors):
        c = dict(session) if ck is not None else {}
        if ck:
            c["_csrf"] = csrf_cookie or ""
        hh = {"X-Csrf-Token": page_csrf} if csrf_header and page_csrf else {}
        s, t, _ = call(path, json_body=body, headers=hh, cookies=c)
        try:
            d = json.loads(t)
            cj = d["state"]["currentJob"]
            return {"status": s, "json": True, "steps": len(cj.get("steps", [])), "keys": sorted(cj), "jobs": [j["name"] for j in d["state"]["run"]["jobs"]]}
        except Exception:
            return {"status": s, "json": False, "body": t[:40].strip()}

    base_path = f"/{repo}/actions/runs/{number}/jobs"
    variants = {}
    # index du job « test » : lu sur le run (liste des jobs), repli sur 1
    first = post(f"{base_path}/0", True, True)
    idx = first.get("jobs", ["build", "test"]).index("test") if first.get("json") else 1
    out["job_index_by_name"] = idx
    variants["session seule"] = post(f"{base_path}/{idx}", None, False)
    variants["session + cookie _csrf"] = post(f"{base_path}/{idx}", True, False)
    variants["session + cookie _csrf + en-tête"] = post(f"{base_path}/{idx}", True, True)
    variants["avec /attempt/1 (+ _csrf)"] = post(f"{base_path}/{idx}/attempt/1", True, True)
    out["variants"] = variants

    # --- sortie des étapes (meilleure variante), étape hors bornes
    best = f"{base_path}/{idx}/attempt/1" if variants["avec /attempt/1 (+ _csrf)"]["json"] else f"{base_path}/{idx}"
    steps = max(variants["avec /attempt/1 (+ _csrf)"].get("steps", 0), variants["session + cookie _csrf + en-tête"].get("steps", 0))
    cur = {"logCursors": [{"step": i, "cursor": None, "expanded": True} for i in range(steps)]}
    c = dict(session)
    c["_csrf"] = csrf_cookie or ""
    s, t, _ = call(best, json_body=cur, headers={"X-Csrf-Token": page_csrf or ""}, cookies=c)
    try:
        d = json.loads(t)
        out["lines_per_step"] = [len(x["lines"]) for x in d["logs"]["stepsLog"]]
        out["step_summary_keys"] = sorted(d["state"]["currentJob"]["steps"][0])
        out["has_all_attempts"] = "allAttempts" in d["state"]["currentJob"]
    except Exception:
        out["lines_per_step"] = f"HTTP {s} non JSON"
    s, t, _ = call(best, json_body={"logCursors": [{"step": 99, "cursor": None, "expanded": True}]}, headers={"X-Csrf-Token": page_csrf or ""}, cookies=c)
    out["out_of_range_step_status"] = s
    s, _, _ = call(best, json_body=cursors, cookies={})
    out["without_cookie_status"] = s
    print(json.dumps(out, ensure_ascii=False, indent=1))


main()
