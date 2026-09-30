# SpaceRazer — common development tasks.
# Run `make` or `make help` to list targets.

CARGO ?= cargo
BIN   := target/release/spacerazer

# Optional: folder(s) to open, e.g. `make run DIR=~/Projects`
DIR ?=
# Optional: CLI arguments, e.g. `make cli ARGS="scan ~ --depth 2"`
ARGS ?=

.DEFAULT_GOAL := help
.PHONY: help run dev scan cli build release test lint fmt fmt-check clippy check-cross clean

help: ## Show this help
	@awk 'BEGIN {FS = ":.*## "} /^[a-zA-Z_-]+:.*## / {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

run: release ## Build optimised and launch the app (DIR=folder to scan on start)
	$(BIN) $(DIR)

dev: ## Launch a debug build (faster to compile, slower to run)
	$(CARGO) run -p sr-gui -- $(DIR)

scan: release ## Open the app and immediately scan DIR (default: home folder)
	$(BIN) $(if $(DIR),$(DIR),$(HOME))

cli: release ## Run the headless CLI, e.g. make cli ARGS="dev ~/code"
	$(BIN) $(ARGS)

build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

release: ## Optimised build of the app
	$(CARGO) build --release -p sr-gui

test: ## Run all tests
	$(CARGO) test --workspace

lint: fmt-check clippy ## Formatting check and clippy (CI gate, NFR-MAIN-02)

fmt: ## Format all code
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check-cross: ## Type-check the core crates for Linux and Windows
	rustup target add x86_64-unknown-linux-gnu x86_64-pc-windows-msvc
	$(CARGO) check --target x86_64-unknown-linux-gnu -p sr-core -p sr-platform -p sr-scan -p sr-ops -p sr-devsweep -p sr-dedup -p sr-cli
	$(CARGO) check --target x86_64-pc-windows-msvc -p sr-core -p sr-platform -p sr-scan -p sr-devsweep
	$(CARGO) check --target x86_64-pc-windows-msvc -p sr-ops --features blake3/pure
	$(CARGO) check --target x86_64-pc-windows-msvc -p sr-dedup --features blake3/pure

clean: ## Remove build output
	$(CARGO) clean
