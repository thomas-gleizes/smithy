SHELL := /bin/bash
export PATH := $(HOME)/.cargo/bin:$(PATH)

DEV := dev/dev.sh
DEV_ENV := set -a; . dev/.env.local; set +a;

.DEFAULT_GOAL := help
.PHONY: help build release run dev-run test lint fmt fmt-check check ci clean \
        dev-up dev-seed dev-down dev-reset dev-logs dev-env

help: ## Affiche cette aide
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  \033[36m%-11s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

## --- Application ---

build: ## Compile en debug
	cargo build --workspace

release: ## Compile en release (binaire dans target/release/smithy)
	cargo build --release -p smithy

run: ## Lance smithy avec SMITHY_URL/SMITHY_TOKEN ou ~/.config/smithy/config.toml
	cargo run -p smithy

dev-run: dev-seed ## Lance smithy contre le Forgejo local (démarre l'environnement si besoin)
	$(DEV_ENV) cargo run -p smithy

## --- Qualité ---

test: ## Lance tous les tests
	cargo test --workspace

lint: ## Clippy, warnings = erreurs
	cargo clippy --workspace --all-targets -- -D warnings

fmt: ## Formate le code
	cargo fmt --all

fmt-check: ## Vérifie le formatage sans modifier
	cargo fmt --all -- --check

check: fmt-check lint test ## Tout ce que la CI vérifiera

ci: check ## Alias de check

clean: ## Supprime target/
	cargo clean

## --- Forgejo local jetable (Docker) ---

dev-up: ## Démarre Forgejo + runner, crée utilisateur, token et dépôt
	$(DEV) up

dev-seed: ## Idem + pousse les workflows de démo (un vert, un rouge)
	$(DEV) seed

dev-down: ## Arrête les conteneurs en gardant les données
	cd dev && docker compose stop

dev-reset: ## Détruit conteneurs, volumes et token (retour à zéro)
	$(DEV) reset

dev-logs: ## Suit les logs de Forgejo et du runner
	cd dev && docker compose logs -f --tail 50

dev-env: ## Affiche SMITHY_URL et SMITHY_TOKEN du Forgejo local
	$(DEV) env
