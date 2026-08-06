# aprsr development tasks.
#
# `make` with no target prints this help. Everything CI runs is reachable from
# `make ci`, so a green `make ci` locally means a green pipeline.

CARGO ?= cargo
NPM   ?= npm
WEB   := web

.DEFAULT_GOAL := help
.PHONY: help build test fmt fmt-check lint web web-check web-test clean ci run coverage assets-check

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

clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf $(WEB)/node_modules www/dist

ci: fmt-check lint test web-check web-test assets-check ## Everything CI runs
	@echo "ci: all checks passed"
