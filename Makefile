# Repeatable commands. `make help` lists them. macOS or Linux shell.
.PHONY: help hooks ui-install fmt check check-changed golden-update rtsan bench bench-roll-operations roll-timing-test bench-instructions profile profile-tracy build-app install-app qa-ui qa-app

# rustup installs cargo into ~/.cargo/bin, which is often not on PATH.
export PATH := $(HOME)/.cargo/bin:$(PATH)
CARGO := $(HOME)/.cargo/bin/cargo
UI := app/ui
APP_BUNDLE := target/release/bundle/macos/Grooviq.app
# `make profile SAMPLY="samply record --save-only"` records without opening the profiler.
SAMPLY := samply record

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-19s\033[0m %s\n", $$1, $$2}'

hooks: ## Point git at .githooks (staged conflict/format checks before each commit)
	git config core.hooksPath .githooks

.PHONY: merge-train test-merge-train
BATCH ?= 4
merge-train: ## Quality only: advance a tested batch (ORDER="12=<approved SHA> 34=<approved SHA>", BATCH=4, optional ACK_MISMATCH=<batch SHA>)
	python3 -B scripts/merge_train.py --order $(ORDER) --batch $(BATCH) $(if $(ACK_MISMATCH),--ack-mismatch $(ACK_MISMATCH)) || [ $$? -eq 75 ]

test-merge-train: ## Test ordered protected merges without calling GitHub
	python3 -B -m unittest discover -s scripts -p test_merge_train.py -v

.PHONY: merge-train-agent-enable merge-train-agent-disable merge-train-agent-status merge-train-agent-resume merge-train-agent-tick merge-train-agent-ledger merge-train-order test-merge-train-agent
merge-train-agent-enable: ## Enable optional three-minute merge-train ticks from merged main
	$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py enable

merge-train-agent-disable: ## Disable future merge-train ticks without interrupting an active tick
	$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py disable

merge-train-agent-status: ## Show the local approved order and any merge-train stop record
	@$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py status

merge-train-agent-resume: ## Clear the agent stop record after investigating it
	$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py resume

merge-train-agent-tick: ## Run the same guarded tick that launchd runs
	$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py tick

merge-train-agent-ledger: ## Print landed merge-train batches as Markdown rows
	@$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py ledger

merge-train-order: export BATCH := $(BATCH)
merge-train-order: ## Save Quality's approved order (ORDER="12=<approved SHA> 34=<approved SHA>", BATCH=4)
	$(REFRESH_PYTHON) -E -s -S -B scripts/merge_train_agent.py set-order --make-order="$$ORDER" --batch="$$BATCH"

test-merge-train-agent: ## Exercise the merge-train agent with fake launchd and local Git fixtures
	$(REFRESH_PYTHON) -B scripts/test_merge_train_agent.py -v

.PHONY: test-loom
test-loom: ## Model the real-time worker protocol with Loom
	RUSTFLAGS="--cfg loom" CARGO_TARGET_DIR=target/loom $(CARGO) test -p grooviq-engine --lib rt_workers::loom -- --test-threads=1

.PHONY: test-pre-commit
test-pre-commit: ## Exercise staged conflict checks in disposable Git repositories
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_pre_commit.py -v

.PHONY: reclaim-caches disk-check test-reclaim-caches
reclaim-caches: ## Preview idle worktree target cleanup (RECLAIM_APPLY=1 deletes, RECENT_MINUTES=45)
	@python3 -B scripts/reclaim_caches.py reclaim-caches $(if $(filter command line,$(origin RECLAIM_APPLY)),--apply="$$RECLAIM_APPLY") --recent-minutes="$${RECENT_MINUTES:-45}"

disk-check: ## Require free space on the repo volume (MIN_GIB=20)
	@python3 -B scripts/reclaim_caches.py disk-check --min-gib="$${MIN_GIB:-20}"

test-reclaim-caches: ## Exercise cache reclamation in temporary repositories
	python3 -B -m unittest discover -s scripts -p test_reclaim_caches.py -v

ui-install: ## Install the UI's locked dependencies (npm ci)
	npm --prefix $(UI) ci --no-audit --no-fund

fmt: ## Format all Rust code
	$(CARGO) fmt --all

check: test-instruction-profiles test-merge-train test-merge-train-agent test-pre-commit test-qa-driver ci-paths-test e2e-browser-results-test e2e-diagnostics-test rerun-no-runner-test optional-rust-policy-test ui-install test-refresh-app ## Everything CI runs: fmt, clippy, workspace tests, UI lint/typecheck/tests/build
	npm --prefix $(UI) run lint
	npm --prefix $(UI) run typecheck
	npm --prefix $(UI) test -- --run
	npm --prefix $(UI) run build
	$(CARGO) fmt --all --check
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings
	@cargo_commands="$$($(CARGO) --list)" && \
	if printf '%s\n' "$$cargo_commands" | grep -Eq '^[[:space:]]+nextest([[:space:]]|$$)'; then \
		$(CARGO) nextest run --workspace --all-features --profile ci && \
		$(CARGO) test --workspace --all-features --doc; \
	else \
		printf '%s\n' 'cargo-nextest unavailable. Install with: cargo install cargo-nextest --version 0.9.151 --locked'; \
		$(CARGO) test --workspace --all-features; \
	fi
	$(MAKE) grid-budget
	$(MAKE) notes-edit-budget
	$(MAKE) export-budget

.PHONY: notes-edit-budget
notes-edit-budget: ## Optimized 10,000-note edit budget, including undo bookkeeping
	node --test scripts/notes-edit-budget.test.mjs
	node scripts/notes-edit-budget.mjs

check-changed: ## Only the checks the changes against origin/main need (ARGS=--plan prints the plan)
	node scripts/check-changed.mjs $(ARGS)

.PHONY: ipc-contract-update ipc-contract-check ipc-contract-ui
ipc-contract-ui:
	@test -f $(UI)/dist/index.html || $(MAKE) ui-install
	@test -f $(UI)/dist/index.html || npm --prefix $(UI) run build

ipc-contract-update: ipc-contract-ui ## Regenerate the TypeScript IPC contract
	$(CARGO) run -p grooviq-app --features ipc-contract --example ipc-contract -- update

ipc-contract-check: ipc-contract-ui ## Verify the generated TypeScript IPC contract
	$(CARGO) run -p grooviq-app --features ipc-contract --example ipc-contract -- check

.PHONY: replay replay-update
replay: ## Compare the real offline Session and MIDI hub against both browser mocks
replay-update: ## Regenerate the real Session and MIDI transcripts (REPLAY_UPDATE_ONLY=1 skips browser checks)
ifeq ($(MAKECMDGOALS):$(REPLAY_UPDATE_ONLY),replay-update:1)
replay-update:
	GROOVIQ_UPDATE_REPLAY=1 node scripts/semantic-replay.mjs
else
replay replay-update: ui-install
	npm --prefix $(UI) run build
	$(if $(filter replay-update,$@),GROOVIQ_UPDATE_REPLAY=1 node scripts/semantic-replay.mjs)
	node scripts/semantic-replay.mjs
	npm --prefix $(UI) exec -- playwright install chromium webkit
	npm --prefix $(UI) run e2e -- --config playwright.replay.config.ts
endif

qa-ui: ui-install ## Playwright on the web build with the mock engine (WebKit and Chromium); shots in app/ui/e2e-results
	cd $(UI) && npx playwright install chromium webkit && npm run e2e

.PHONY: qa-mixer-ui
qa-mixer-ui: ui-install ## Playwright on the mixer components' synthetic fixture only (WebKit and Chromium); ARGS=<extra playwright args>
	cd $(UI) && npx playwright install chromium webkit && npm run e2e -- e2e/m4-mixer-components.e2e.ts $(ARGS)

GALLERY_PORT ?= 4795
.PHONY: ui-gallery ui-gallery-build ui-gallery-check ui-gallery-capture
ui-gallery: ## Serve actual controls with generated state; installed UI dependencies required
	cd $(UI) && node node_modules/vite/bin/vite.js --config gallery/vite.config.mts --host 127.0.0.1 --port $(GALLERY_PORT) --strictPort

ui-gallery-build: ## Build the additive control gallery without installing dependencies
	cd $(UI) && node node_modules/vite/bin/vite.js build --config gallery/vite.config.mts

ui-gallery-check: ## Typecheck and lint the control gallery against its real imported APIs
	cd $(UI) && node node_modules/typescript/bin/tsc --project gallery/tsconfig.json --noEmit
	cd $(UI) && node node_modules/eslint/bin/eslint.js gallery --max-warnings 0

ui-gallery-capture: ## Capture managed gallery URL (GROOVIQ_GALLERY_URL); no server or install implied
	cd $(UI) && node node_modules/@playwright/test/cli.js test --config gallery/playwright.config.mts

golden-update: ## Rewrite the golden reference WAVs on purpose; say why in the PR
	GOLDEN_UPDATE=1 $(CARGO) test -p grooviq-golden

rtsan: ## The no_alloc tests under RealtimeSanitizer (macOS/Linux; downloads LLVM's runtime once)
	RTSAN_ENABLE=1 RTSAN_OPTIONS= $(CARGO) test -p grooviq-engine --all-features --test no_alloc

bench: ## Render speed, block times and determinism of the reference scenes, then DSP kernel timings
	$(CARGO) bench -p grooviq-golden --bench render
	$(CARGO) bench -p grooviq-dsp --bench kernels

.PHONY: bench-song
bench-song: ## Five warmed production exports of the M3 golden song (run on a quiet host)
	$(CARGO) bench -p grooviq-golden --bench song_export

bench-roll-operations: ## The six piano-roll timing budgets, without work counters (run on a quiet host)
	@set --; \
	if [ -n "$$ROLL_TIMING_REPORT" ]; then \
		set -- --reporter=default --reporter=json --outputFile.json="$$ROLL_TIMING_REPORT"; \
	fi; \
	GROOVIQ_ROLL_PERF=1 npm --prefix $(UI) test -- --run src/roll/budgets.test.ts --maxWorkers=1 --disableConsoleIntercept "$$@"

roll-timing-test: ## Test evidence validation and the bounded piano-roll timing rerun
	node --test scripts/roll-timing.test.cjs

bench-instructions: ## Instruction counts (Linux only: needs valgrind and gungraun-runner)
	$(CARGO) bench -p grooviq-golden --bench instructions

profile: ## Profile the render benchmark with samply (cargo install --locked samply); ARGS= scene filter
	@command -v samply >/dev/null || { echo "samply is missing: cargo install --locked samply"; exit 1; }
	@mkdir -p target
	CARGO_PROFILE_BENCH_DEBUG=line-tables-only $(CARGO) bench -p grooviq-golden --bench render --no-run > target/render-build.log 2>&1 || { cat target/render-build.log; exit 1; }
	$(SAMPLY) -o target/render-profile.json.gz -- "$$(sed -n 's/^ *Executable .*(\(.*\))$$/\1/p' target/render-build.log)" $(ARGS)

profile-tracy: ui-install ## Build an optimised profiling bundle in target/profiling
	CARGO_TARGET_DIR="$(CURDIR)/target/profiling" npm --prefix $(UI) run tauri build -- --bundles app --features profiling

build-app: ui-install ## Build the release Grooviq.app (no .dmg) into target/release/bundle/macos
	npm --prefix $(UI) run tauri build -- --bundles app

qa-app: ## Run the built app silently (GROOVIQ_AUDIO=null) with a throwaway home folder (QA_HOME=<dir> reuses one)
	sh scripts/qa-app.sh

install-app: refresh-app ## Alias for the verified merged-main refresh

.PHONY: refresh-app test-refresh-app
# Local refresh uses tarfile's safe extraction filter (Python 3.12+).
REFRESH_PYTHON ?= $(shell command -v python3.14 || command -v python3.13 || command -v python3.12 || command -v python3)
refresh-app: ## Quality only: build merged origin/main in the stable cache and replace safely
	@$(REFRESH_PYTHON) -c 'import sys; sys.exit(0 if sys.version_info >= (3, 12) else "Grooviq refresh requires Python 3.12+; install Homebrew python or set REFRESH_PYTHON.")'
	$(REFRESH_PYTHON) -B scripts/refresh_install.py

test-refresh-app: ## Exercise install failures with owned fake apps; never builds or launches
	PYTHONDONTWRITEBYTECODE=1 $(REFRESH_PYTHON) scripts/test_refresh_install.py
	PYTHONDONTWRITEBYTECODE=1 $(REFRESH_PYTHON) scripts/test_refresh_agent.py

.PHONY: refresh-agent-enable refresh-agent-status refresh-agent-disable refresh-agent-tick
refresh-agent-enable: ## Enable optional five-minute merged-main refreshes (reviewed merged code only)
	$(REFRESH_PYTHON) -B scripts/refresh_agent.py enable

refresh-agent-status: ## Show local refresh queue, install source and lease deferral
	$(REFRESH_PYTHON) -B scripts/refresh_agent.py status

refresh-agent-disable: ## Disable future automatic refreshes without interrupting an active one
	$(REFRESH_PYTHON) -B scripts/refresh_agent.py disable

refresh-agent-tick: ## Check merged-main queue once through the same guarded coordinator
	$(REFRESH_PYTHON) -B scripts/refresh_agent.py tick

.PHONY: qa-driver-build qa-driver
qa-driver-build: ui-install ## Build the optional macOS QA driver app
	npm --prefix $(UI) run tauri build -- --bundles app --features qa-driver

qa-driver: ## Control QA_HOME's silent automation app; SCRIPT=<trusted JS file>
	python3 scripts/qa-driver.py --home "$(QA_HOME)" $(if $(SCRIPT),--script "$(SCRIPT)",)

.PHONY: qa-driver-stop qa-driver-quit test-qa-driver
test-qa-driver: ## Test private QA command separation without launching the app
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_qa_driver.py -v

qa-driver-quit: ## Request normal Quit in QA_HOME, including the unsaved-close prompt
	python3 scripts/qa-driver.py --home "$(QA_HOME)" --quit

qa-driver-stop: ## Stop QA_HOME's automation app without a native dialog
	python3 scripts/qa-driver.py --home "$(QA_HOME)" --shutdown

.PHONY: qa-driver-app
qa-driver-app: ## Launch the qa-driver build hidden; use QA_HOME=<private temporary directory>
	$(MAKE) qa-app QA_AUTOMATION=1

.PHONY: qa-fixture
export QA_HOME
qa-fixture: ## Generate a persistent offline QA home (optional QA_HOME=<empty directory>; macOS launcher layout)
	$(CARGO) run --quiet -p grooviq-qa-fixture

.PHONY: grid-budget
grid-budget: ## Largest step-grid build: original 50 ms budget on standalone V8 CPU time
	node scripts/grid-budget.mjs

.PHONY: export-budget
export-budget: ## Optimized 8-second PCM24 export with the original 4-second budget
	node --test scripts/notes-edit-budget.test.mjs
	node scripts/export-budget.mjs

.PHONY: optional-rust-policy-test
optional-rust-policy-test: ## Test optional Rust CI selection and conservative failure fallback
	node --test scripts/optional-rust-checks.test.cjs

.PHONY: ci-paths-test
ci-paths-test: ## Test CI path selection and conservative failure fallback
	node --test scripts/ci-paths.test.cjs

.PHONY: rerun-no-runner-test
rerun-no-runner-test: ## Test the macOS runner acquisition retry policy
	node --test scripts/rerun-no-runner.test.cjs

# T13: isolated Song reference selection; no global golden or benchmark workload changes.
.PHONY: song-golden-test song-golden-update song-export-budget
song-golden-test: ## Compare complete M3 WAV, partitions and bounded reference storage
	@set -eu; log=$$(mktemp); trap 'rm -f "$$log"' EXIT; \
	env -u SONG_M3_UPDATE CARGO_BUILD_JOBS=2 $(CARGO) test -p grooviq-golden --lib song::storage -- --skip the_system_ports_list_without_error_and_refuse_a_name_that_is_not_there --skip lists_the_real_outputs >"$$log" 2>&1 || { cat "$$log"; exit 1; }; \
	cat "$$log"; test "$$(grep -c '^test result: ok. 2 passed; 0 failed; 0 ignored;' "$$log")" = 1; \
	env -u SONG_M3_UPDATE CARGO_BUILD_JOBS=2 $(CARGO) test -p grooviq-golden --release --test song_m3 -- --skip the_system_ports_list_without_error_and_refuse_a_name_that_is_not_there --skip lists_the_real_outputs >"$$log" 2>&1 || { cat "$$log"; exit 1; }; \
	cat "$$log"; test "$$(grep -c '^test result: ok. 3 passed; 0 failed; 1 ignored;' "$$log")" = 1

song-golden-update: ## Explicit SONG_M3_UPDATE=1 opt-in; update only the new M3 Song reference
	@test "$(SONG_M3_UPDATE)" = 1 || { echo 'Set SONG_M3_UPDATE=1 explicitly to update only song_m3'; exit 1; }
	@set -eu; log=$$(mktemp); trap 'rm -f "$$log"' EXIT; \
	SONG_M3_UPDATE=1 CARGO_BUILD_JOBS=2 $(CARGO) test -p grooviq-golden --release --test song_m3 complete_song_bytes_roundtrip_and_full_callback_partitions -- --exact --skip the_system_ports_list_without_error_and_refuse_a_name_that_is_not_there --skip lists_the_real_outputs >"$$log" 2>&1 || { cat "$$log"; exit 1; }; \
	cat "$$log"; test "$$(grep -c '^test result: ok. 1 passed; 0 failed; 0 ignored;' "$$log")" = 1

song-export-budget: ## Five warmed release production exports; median at most nine seconds
	@set -eu; log=$$(mktemp); trap 'rm -f "$$log"' EXIT; \
	env -u SONG_M3_UPDATE CARGO_BUILD_JOBS=2 $(CARGO) test -p grooviq-golden --release --test song_m3 warmed_export_budget -- --ignored --exact --nocapture --skip the_system_ports_list_without_error_and_refuse_a_name_that_is_not_there --skip lists_the_real_outputs >"$$log" 2>&1 || { cat "$$log"; exit 1; }; \
	cat "$$log"; test "$$(grep -c '^test result: ok. 1 passed; 0 failed; 0 ignored;' "$$log")" = 1

.PHONY: qa-stability qa-stability-smoke
qa-stability: ## Opt-in: twenty full Linux WebKit + Chromium suites at HEAD in Docker (2 CPUs)
	sh scripts/stability/container.sh proof

qa-stability-smoke: ## Tiny real Playwright fixtures exercising stability accounting in Docker
	sh scripts/stability/container.sh smoke

.PHONY: capture-instruction-profiles test-instruction-profiles
capture-instruction-profiles: ## Historical Linux capture; ARGS must give --repo and a new external --output
	python3 scripts/capture-instruction-profiles.py $(ARGS)

test-instruction-profiles: ## Safe profile capture tests (no Rust build or measurement)
	PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_capture_instruction_profiles.py

.PHONY: e2e-diagnostics-test
e2e-diagnostics-test: ## Test bounded timeout-surviving e2e diagnostics (Node only)
	node --test scripts/e2e-diagnostics.test.mjs

.PHONY: e2e-browser-results-test
e2e-browser-results-test: ## Test complete browser CI accounting (Node only)
	node --test scripts/e2e-browser-results.test.mjs

.PHONY: test-perf-fetch
test-perf-fetch: ## Exercise bounded benchmark dependency setup without Cargo or network
	PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p test_perf_fetch.py -v

.PHONY: bench-mixer
bench-mixer: ## Static mixer kernel render timings at 64/128 frames (smoke only, no deadline claim)
	$(CARGO) bench -p grooviq-engine --bench mixer_kernels

.PHONY: bench-rt-workers
bench-rt-workers: ## Worker pool block timings, wake latency or idle CPU (ARGS=--wake-latency or --idle-cpu=2)
	$(CARGO) bench -p grooviq-engine --bench rt_workers -- $(ARGS)
