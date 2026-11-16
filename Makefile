# Repeatable commands. `make help` lists them. macOS or Linux shell.
.PHONY: help hooks ui-install fmt check check-changed golden-update build-app install-app

# rustup installs cargo into ~/.cargo/bin, which is often not on PATH.
export PATH := $(HOME)/.cargo/bin:$(PATH)
CARGO := $(HOME)/.cargo/bin/cargo
UI := app/ui
APP_BUNDLE := target/release/bundle/macos/Grooviq.app

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'

hooks: ## Point git at .githooks (staged-format check before each commit)
	git config core.hooksPath .githooks

ui-install: ## Install the UI's locked dependencies (npm ci)
	npm --prefix $(UI) ci --no-audit --no-fund

fmt: ## Format all Rust code
	$(CARGO) fmt --all

check: ui-install ## Everything CI runs: fmt, clippy, workspace tests, UI lint/typecheck/tests/build
	npm --prefix $(UI) run lint
	npm --prefix $(UI) run typecheck
	npm --prefix $(UI) test -- --run
	npm --prefix $(UI) run build
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	$(CARGO) test --workspace --all-features

check-changed: ## Only the checks the changes against origin/main need (ARGS=--plan prints the plan)
	node scripts/check-changed.mjs $(ARGS)

golden-update: ## Rewrite the golden reference WAVs on purpose; say why in the PR
	GOLDEN_UPDATE=1 $(CARGO) test -p grooviq-golden

build-app: ui-install ## Build the release Grooviq.app (no .dmg) into target/release/bundle/macos
	npm --prefix $(UI) run tauri build -- --bundles app

install-app: ## Copy the built Grooviq.app into /Applications (run build-app first)
	@test -d $(APP_BUNDLE) || { echo "No $(APP_BUNDLE). Run make build-app first."; exit 1; }
	rm -rf /Applications/Grooviq.app
	ditto $(APP_BUNDLE) /Applications/Grooviq.app
