# aprsr development tasks.
#
# `make` with no target prints this help. Everything CI runs is reachable from
# `make ci`, so a green `make ci` locally means a green pipeline.

CARGO ?= cargo
NPM   ?= npm
WEB   := web

.DEFAULT_GOAL := help
.PHONY: help build test fmt fmt-check lint web web-check web-test clean ci run coverage assets-check deny

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

build: ## Build the workspace
	$(CARGO) build --workspace

test: ## Run the Rust test suite
	$(CARGO) test --workspace --all-features

fmt: ## Format Rust sources
	$(CARGO) fmt --all

fmt-check: ## Verify formatting (CI)
	$(CARGO) fmt --all --check

lint: ## Clippy with warnings denied (CI)
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

web: ## Build the dashboard assets into crates/aprsr-web/static/
	cd $(WEB) && $(NPM) run build

web-check: ## Type-check the TypeScript sources
	cd $(WEB) && $(NPM) run typecheck

web-test: ## Run the TypeScript unit tests
	cd $(WEB) && $(NPM) test

assets-check: web ## Fail if the committed dashboard assets are stale (CI)
	@git diff --exit-code -- crates/aprsr-web/static \
		|| { echo "ERROR: crates/aprsr-web/static is stale. Run 'make web' and commit the result."; exit 1; }

run: ## Run the server against aprsr.example.toml
	$(CARGO) run -p aprsr -- run --config aprsr.example.toml

coverage: ## Line coverage report (needs cargo-llvm-cov)
	$(CARGO) llvm-cov --workspace --all-features --html

# Skipping is loud on purpose. This check was originally missing from `ci`, so a
# green `make ci` reported success while the pipeline failed on a licence the
# allow-list did not cover. A silent skip would recreate exactly that gap.
deny: ## Licence and advisory check (CI; needs cargo-deny)
	@if command -v cargo-deny >/dev/null 2>&1; then \
		cargo-deny check; \
	else \
		echo "================================================================"; \
		echo "SKIPPED: cargo-deny is not installed, so licences and advisories"; \
		echo "were NOT checked. CI still runs this and can fail where you just"; \
		echo "passed. Install it with:"; \
		echo "    cargo install cargo-deny --locked"; \
		echo "================================================================"; \
	fi

clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf $(WEB)/node_modules www/dist

ci: fmt-check lint test deny web-check web-test assets-check ## Everything CI runs
	@echo "ci: all checks passed"
