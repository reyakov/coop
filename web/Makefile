.PHONY: help dev build-wasm build-web build clean install

help: ## Show help information
	@echo "Coop - Available commands:"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-15s\033[0m %s\n", $$1, $$2}'

install: ## Install all dependencies
	@echo "Checking Rust WASM target..."
	@rustup target add wasm32-unknown-unknown || true
	@echo "Checking wasm-bindgen-cli..."
	@cargo install wasm-bindgen-cli || true
	@echo "Installing frontend dependencies..."
	@cd www && deno install

build-wasm: ## Build WASM (release mode)
	@./script/build-wasm.sh --release

build-wasm-dev: ## Build WASM (debug mode)
	@./script/build-wasm.sh

build-web: ## Build frontend
	@cd www && deno task build

build-web-prod: ## Build frontend for production (GitHub Pages)
	@cd www && deno install && NODE_ENV=production deno task build

build: build-wasm build-web ## Build complete project (WASM + frontend)

build-prod: build-wasm build-web-prod ## Build complete project for production

dev: build-wasm-dev ## Start development server
	@cd www && deno install && deno task dev

preview: ## Preview production build
	@cd www && deno task preview

clean: ## Clean build artifacts
	@echo "Cleaning build artifacts..."
	@rm -rf www/dist
	@rm -rf www/wasm/*.js www/wasm/*.wasm
	@cargo clean

watch-wasm: ## Watch Rust code changes and auto-rebuild WASM
	@echo "Watching WASM changes..."
	@cargo watch -x 'build --target wasm32-unknown-unknown' -s './script/build-wasm.sh'
