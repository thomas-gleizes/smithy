#!/usr/bin/env bash
# Boucle de test locale : Forgejo jetable + utilisateur + token + dépôt de démo.
#   ./dev.sh up      démarre et initialise (idempotent), écrit dev/.env.local
#   ./dev.sh reset   détruit conteneur et volume
#   ./dev.sh env     affiche SMITHY_URL / SMITHY_TOKEN
set -euo pipefail
cd "$(dirname "$0")"

PORT="${FORGEJO_PORT:-3300}"
URL="http://localhost:${PORT}"
USER_NAME="smithy"
USER_PASS="smithy-dev-password"
REPO="demo"
PRIVATE_REPO="secret"
ENV_FILE=".env.local"

fg() { docker compose exec -T -u git forgejo forgejo "$@"; }
api() { curl -fsS -u "${USER_NAME}:${USER_PASS}" -H 'Content-Type: application/json' "$@"; }

up() {
  docker compose up -d --wait forgejo >/dev/null

  if ! fg admin user list 2>/dev/null | grep -qw "$USER_NAME"; then
    fg admin user create --admin --username "$USER_NAME" --password "$USER_PASS" \
      --email "${USER_NAME}@example.test" --must-change-password=false >/dev/null
    echo "utilisateur ${USER_NAME} créé"
  fi

  if [[ ! -f "$ENV_FILE" ]] || ! grep -q SMITHY_TOKEN "$ENV_FILE"; then
    local token
    token=$(fg admin user generate-access-token --username "$USER_NAME" \
      --token-name "smithy-$(date +%s)" --scopes "read:repository,read:user,read:issue,read:notification" --raw)
    printf 'SMITHY_URL=%s\nSMITHY_TOKEN=%s\n' "$URL" "$token" >"$ENV_FILE"
    echo "token (lecture seule) écrit dans dev/${ENV_FILE}"
  fi

  if ! docker compose ps --status running --services | grep -qx runner; then
    RUNNER_TOKEN=$(fg actions generate-runner-token) docker compose up -d --wait runner >/dev/null
    echo "runner enregistré (label: docker)"
  fi

  if ! api "${URL}/api/v1/repos/${USER_NAME}/${REPO}" >/dev/null 2>&1; then
    api -X POST "${URL}/api/v1/user/repos" \
      -d "{\"name\":\"${REPO}\",\"auto_init\":true,\"default_branch\":\"main\"}" >/dev/null
    echo "dépôt ${USER_NAME}/${REPO} créé"
  fi
  echo "prêt : ${URL}  (login ${USER_NAME} / ${USER_PASS})"
}

put_file_in() { # dépôt chemin contenu message ; ne fait rien si le fichier existe déjà
  if api "${URL}/api/v1/repos/${USER_NAME}/$1/contents/$2?ref=main" >/dev/null 2>&1; then
    return
  fi
  local b64; b64=$(printf '%s' "$3" | base64 -w0)
  api -X POST "${URL}/api/v1/repos/${USER_NAME}/$1/contents/$2" \
    -d "{\"content\":\"${b64}\",\"message\":\"$4\",\"branch\":\"main\"}" >/dev/null
}
put_file() { put_file_in "$REPO" "$@"; }

# Dépôt privé : vérifie que les logs passent bien avec le token (le dépôt public les sert à tout le monde).
# Workflow riche : 2 jobs, plusieurs étapes, sortie longue, couleurs ANSI.
seed_private() {
  if ! api "${URL}/api/v1/repos/${USER_NAME}/${PRIVATE_REPO}" >/dev/null 2>&1; then
    api -X POST "${URL}/api/v1/user/repos" \
      -d "{\"name\":\"${PRIVATE_REPO}\",\"private\":true,\"auto_init\":true,\"default_branch\":\"main\"}" >/dev/null
    echo "dépôt privé ${USER_NAME}/${PRIVATE_REPO} créé"
  fi
  put_file_in "$PRIVATE_REPO" .forgejo/workflows/rich.yml 'name: rich
on: [push]
jobs:
  build:
    runs-on: docker
    steps:
      - name: Compilation
        run: |
          printf "\033[32mOK\033[0m compilation\n"
          for i in $(seq 1 250); do echo "ligne $i"; done
      - name: Progression
        run: printf "10%%\r50%%\r100%%\n"
  test:
    runs-on: docker
    steps:
      - name: Tests
        run: printf "\033[31mrouge\033[0m puis \033[1;33mjaune\033[0m\n" && exit 1
' "ci: workflow riche"
}

# Pousse des workflows de démo : chaque commit déclenche un run vert et un run rouge.
seed() {
  up
  put_file .forgejo/workflows/ok.yml 'name: ok
on: [push]
jobs:
  build:
    runs-on: docker
    steps:
      - run: echo "tout va bien" && sleep 5
' "ci: workflow vert"
  put_file .forgejo/workflows/fail.yml 'name: fail
on: [push]
jobs:
  test:
    runs-on: docker
    steps:
      - run: echo "boum" && exit 1
' "ci: workflow rouge"
  seed_private
  echo "workflows poussés ; runs visibles sur ${URL}/${USER_NAME}/${REPO}/actions"
}

case "${1:-up}" in
  up) up ;;
  seed) seed ;;
  reset) docker compose down -v --remove-orphans; rm -f "$ENV_FILE" ;;
  env) cat "$ENV_FILE" ;;
  *) echo "usage: $0 up|seed|reset|env" >&2; exit 2 ;;
esac
