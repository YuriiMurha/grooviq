# Repeatable commands. `make help` lists them. macOS or Linux shell.
.PHONY: help hooks ui-install fmt check check-changed golden-update rtsan bench bench-instructions profile build-app install-app qa-ui qa-app

# rustup installs cargo into ~/.cargo/bin, which is often not on PATH.
export PATH := $(HOME)/.cargo/bin:$(PATH)
CARGO := $(HOME)/.cargo/bin/cargo
UI := app/ui
APP_BUNDLE := target/release/bundle/macos/Grooviq.app
# `make profile SAMPLY="samply record --save-only"` records without opening the profiler.
SAMPLY := samply record

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-19s\033[0m %s\n", $$1, $$2}'

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
	$(MAKE) notes-edit-budget

.PHONY: notes-edit-budget
notes-edit-budget: ## Optimized 10,000-note edit budget, including undo bookkeeping
	node --test scripts/notes-edit-budget.test.mjs
	node scripts/notes-edit-budget.mjs

check-changed: ## Only the checks the changes against origin/main need (ARGS=--plan prints the plan)
	node scripts/check-changed.mjs $(ARGS)

qa-ui: ui-install ## Playwright on the web build with the mock engine (WebKit and Chromium); shots in app/ui/e2e-results
	cd $(UI) && npx playwright install chromium webkit && npm run e2e

golden-update: ## Rewrite the golden reference WAVs on purpose; say why in the PR
	GOLDEN_UPDATE=1 $(CARGO) test -p grooviq-golden

rtsan: ## The no_alloc tests under RealtimeSanitizer (macOS/Linux; downloads LLVM's runtime once)
	RTSAN_ENABLE=1 RTSAN_OPTIONS=suppressions=$(CURDIR)/engine/tests/rtsan.supp $(CARGO) test -p grooviq-engine --all-features --test no_alloc

bench: ## Render speed, block times and determinism of the reference scenes, then DSP kernel timings
	$(CARGO) bench -p grooviq-golden --bench render
	$(CARGO) bench -p grooviq-dsp --bench kernels

bench-instructions: ## Instruction counts (Linux only: needs valgrind and gungraun-runner)
	$(CARGO) bench -p grooviq-golden --bench instructions

profile: ## Profile the render benchmark with samply (cargo install --locked samply); ARGS= scene filter
	@command -v samply >/dev/null || { echo "samply is missing: cargo install --locked samply"; exit 1; }
	@mkdir -p target
	CARGO_PROFILE_BENCH_DEBUG=line-tables-only $(CARGO) bench -p grooviq-golden --bench render --no-run > target/render-build.log 2>&1 || { cat target/render-build.log; exit 1; }
	$(SAMPLY) -o target/render-profile.json.gz -- "$$(sed -n 's/^ *Executable .*(\(.*\))$$/\1/p' target/render-build.log)" $(ARGS)

build-app: ui-install ## Build the release Grooviq.app (no .dmg) into target/release/bundle/macos
	npm --prefix $(UI) run tauri build -- --bundles app

qa-app: ## Run the built app silently (GROOVIQ_AUDIO=null) with a throwaway home folder (QA_HOME=<dir> reuses one)
	sh scripts/qa-app.sh

install-app: ## Copy the built Grooviq.app into /Applications (run build-app first)
	@test -d $(APP_BUNDLE) || { echo "No $(APP_BUNDLE). Run make build-app first."; exit 1; }
	rm -rf /Applications/Grooviq.app
	ditto $(APP_BUNDLE) /Applications/Grooviq.app

.PHONY: qa-fixture
export QA_HOME
qa-fixture: ## Generate a persistent offline QA home (optional QA_HOME=<empty directory>; macOS launcher layout)
	$(CARGO) run --quiet -p grooviq-qa-fixture
