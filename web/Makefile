.PHONY: help dev build-wasm build-wasm-dev build-web build-web-prod build build-prod preview clean install watch-wasm

help: ## Show help information
	@echo "Coop - Available commands:"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'

install: ## Install all dependencies
	@echo "Checking Rust WASM target..."
	@rustup target add wasm32-unknown-unknown --toolchain nightly || true
	@echo "Installing frontend dependencies..."
	@cd www && npm install

build-wasm: ## Build WASM (release mode)
	@./script/build-wasm.sh --release

build-wasm-dev: ## Build WASM (debug mode)
	@./script/build-wasm.sh

build-web: ## Build frontend
	@cd www && npm run build

build-web-prod: ## Build frontend for production (GitHub Pages)
	@cd www && npm install && NODE_ENV=production npm run build

build: build-wasm build-web ## Build complete project (WASM + frontend)

build-prod: build-wasm build-web-prod ## Build complete project for production

dev: build-wasm-dev ## Start development server
	@cd www && npm install && npm run dev

preview: ## Preview production build
	@cd www && npm run preview

clean: ## Clean build artifacts
	@echo "Cleaning build artifacts..."
	@rm -rf www/dist
	@rm -rf www/wasm/*.js www/wasm/*.wasm
	@cargo clean

watch-wasm: ## Watch Rust code changes and auto-rebuild WASM
	@echo "Watching WASM changes..."
	@cargo watch -w ../crates -w ../Cargo.toml -w ../Cargo.lock -s './script/build-wasm.sh'
