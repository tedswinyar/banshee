# Banshee — root Makefile. Layer-aware: targets for pruned layers
# (no swift/, no website/) degrade to a clear message, not an error spew.

.PHONY: build install test verify ci bootstrap hooks _hooks-ensure mutations release-build app dmg website serve-website clean help \
        daemon-install daemon-uninstall daemon-status daemon-restart daemon-logs

help: ## Show this help
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  %-16s %s\n", $$1, $$2}'

bootstrap: hooks ## First-run setup for a fresh clone: install the pre-push verify gate
	@echo "banshee: bootstrap complete — the push gate is installed"

hooks: ## Install or refresh the pre-push verify gate
	./scripts/install-hooks.sh

# Silently self-heal the push gate: git does not run install code on clone (by
# design), so instead any make-driven work installs the hook if it is missing
# or stale. That closes the "fresh clone / second machine never ran step 5"
# enforcement gap (banshee-nnm) without a paid remote CI — the local gate is
# the CI for this single-dev tool. check-hooks.sh is the cheap idempotent test
# (it byte-compares the installed block); only reinstall when it fails, and
# never fail a build over it (a source tarball outside git has no hooks dir).
_hooks-ensure:
	@if ! ./scripts/check-hooks.sh >/dev/null 2>&1; then \
		echo "banshee: installing the pre-push verify gate (was missing or stale)"; \
		./scripts/install-hooks.sh >/dev/null 2>&1 || \
		  echo "banshee: could not install the push gate (not a git checkout?); continuing"; \
	fi

build: _hooks-ensure ## Debug-build the Rust workspace (and Swift app if present)
	cd rust && cargo build --workspace
	@if [ -d swift ]; then cd swift && swift build; fi

release-build: _hooks-ensure ## Release-build the Rust workspace
	cd rust && cargo build --workspace --release

test: verify ## Alias for verify

ci: verify ## Canonical "run the whole gate" entry point (= verify)

mutations: ## Replay the checked-in mutation manifest (.mutations/); minutes, not per-push
	./scripts/replay-mutations.sh

verify: _hooks-ensure ## Run all repo health checks (auto-detects layers)
	./scripts/verify.sh

app: ## Assemble the .app bundle (requires swift/)
	@if [ ! -d swift ]; then echo "no swift/ layer in this project"; exit 1; fi
	./scripts/build-app.sh

dmg: ## Build the distributable DMG (requires swift/ + release config)
	@if [ ! -d swift ]; then echo "no swift/ layer in this project"; exit 1; fi
	./scripts/build-dmg.sh

website: ## Build the website (requires website/)
	@if [ ! -d website ]; then echo "no website/ layer in this project"; exit 1; fi
	cd website && hugo build

serve-website: ## Serve the website locally with live reload
	@if [ ! -d website ]; then echo "no website/ layer in this project"; exit 1; fi
	cd website && hugo server

start: ## Start the API server in the dev profile (foreground)
	./scripts/start.sh

# --- The daemon (ADR-0004) --------------------------------------------------
# REBUILDING DOES NOT UPDATE THE RUNNING SERVICE. `daemon-install` does, and it
# is idempotent — run it after any change you want the daemon to serve. This is
# the one thing every contributor has to learn once.

daemon-install: ## Install/update the daemon + CLI + perf-scan shim (the way to ship a change)
	./scripts/install-launchd.sh

install: daemon-install app ## Ship a change to THIS machine: daemon + CLI, then the app into /Applications, relaunched. Always both (they only agree when built from one commit)
	./scripts/install-app.sh

daemon-uninstall: ## Unregister the LaunchAgent (leaves the database alone)
	./scripts/uninstall-launchd.sh

daemon-status: ## Report the daemon's MACHINE state (not part of verify, on purpose)
	./scripts/check-service.sh

daemon-restart: ## Restart the running daemon without reinstalling
	launchctl kickstart -k "gui/$$(id -u)/com.tedswinyar.banshee-api"
	./scripts/check-service.sh

daemon-logs: ## Tail the daemon's logs
	tail -f "$$HOME/Library/Logs/banshee-api.log" "$$HOME/Library/Logs/banshee-api.err.log"

clean: ## Remove build products
	cd rust && cargo clean
	@if [ -d swift ]; then cd swift && swift package clean; fi
	rm -rf .runtime build dist
