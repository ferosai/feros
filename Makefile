# Voice Agent OS — top-level build commands
# ─────────────────────────────────

PROTO_DIR   := proto
ML_DIR      := inference
RUST_DIR    := voice
API_DIR     := studio/api
WEB_DIR     := studio/web

ifeq (, $(shell command -v uvx 2> /dev/null))
$(error "uvx could not be found. Please install uv (https://docs.astral.sh/uv/) before proceeding")
endif

PROTOC      := uvx --python 3.12 --from grpcio-tools==1.80.0 python -m grpc_tools.protoc

# ── Protobuf ─────────────────────────────────────────────────────
.PHONY: proto
proto: ## Generate Python, TS, and ML stubs from proto definitions
	# ML layer
	mkdir -p $(ML_DIR)/stt
	$(PROTOC) \
		--python_out=$(ML_DIR)/stt \
		--pyi_out=$(ML_DIR)/stt \
		--proto_path=$(PROTO_DIR) \
		$(PROTO_DIR)/stt.proto
	# Studio API layer
	$(PROTOC) \
		--python_out=$(API_DIR)/app/schemas \
		--pyi_out=$(API_DIR)/app/schemas \
		--proto_path=$(PROTO_DIR) \
		$(PROTO_DIR)/agent.proto
	# Studio Web layer (TS interfaces)
	$(PROTOC) \
		--plugin=protoc-gen-ts_proto=$(WEB_DIR)/node_modules/.bin/protoc-gen-ts_proto \
		--ts_proto_out=$(WEB_DIR)/src/lib/api \
		--ts_proto_opt=esModuleInterop=true,forceLong=string,outputServices=false,outputJsonMethods=false,outputClientImpl=false,outputEncodeMethods=false,outputPartialMethods=false,outputTypeRegistry=false,onlyTypes=true,snakeToCamel=false \
		--proto_path=$(PROTO_DIR) \
		$(PROTO_DIR)/agent.proto
	# Generate JSON schema from compiled python stubs
	$(MAKE) api-schema

# ── Auto-Format ──────────────────────────────────────────────────
.PHONY: format format-api format-web format-engine format-server format-integrations
format: format-api format-web format-engine format-server format-integrations ## Format all projects

format-api:
	cd $(API_DIR) && uv run black app/ && uv run isort app/ && uv run ruff check --fix app/
format-web:
	cd $(WEB_DIR) && pnpm run format || true
format-engine:
	cd $(RUST_DIR)/engine && cargo fmt
format-server:
	cd $(RUST_DIR)/server && cargo fmt
format-integrations:
	cd integrations && cargo fmt

# ── Lint & Typecheck ─────────────────────────────────────────────
.PHONY: lint lint-api lint-web lint-engine lint-server lint-integrations lint-oss
lint: lint-api lint-web lint-engine lint-server lint-integrations lint-oss ## Lint all projects

lint-api:
	cd $(API_DIR) && uv run ruff check app/ && uv run mypy app/
lint-web:
	cd $(WEB_DIR) && pnpm run lint
lint-engine:
	cd $(RUST_DIR)/engine && cargo clippy -- -D warnings
lint-server:
	cd $(RUST_DIR)/server && cargo clippy -- -D warnings
lint-integrations:
	cd integrations && cargo clippy -- -D warnings

lint-oss: lint-oss-api lint-oss-web lint-oss-voice ## Lint all components as they appear in the OSS build

lint-oss-api: ## Lint the Studio API as it appears in the OSS build
	@echo "── [OSS] Studio API ──"
	@set -e; \
	TMP=$$(mktemp -d); \
	trap "rm -rf $$TMP" EXIT INT TERM; \
	rsync -a \
		--exclude='*_internal.py' \
		--exclude='*.internal.ts' \
		--exclude='*.internal.tsx' \
		$(API_DIR)/app/ $$TMP/app/; \
	cp $(API_DIR)/pyproject.toml $$TMP/; \
	cd $$TMP && uv run --project $(CURDIR)/$(API_DIR) ruff check app/ && uv run --project $(CURDIR)/$(API_DIR) mypy app/

lint-oss-web: ## Lint the Studio Web as it appears in the OSS build
	@echo "── [OSS] Studio Web ──"
	@set -e; \
	TMP=$$(mktemp -d); \
	trap "rm -rf $$TMP" EXIT INT TERM; \
	rsync -a \
		--exclude='*_internal.ts' \
		--exclude='*.internal.tsx' \
		--exclude='app/login/' \
		$(WEB_DIR)/src/ $$TMP/src/; \
	ln -s $(CURDIR)/$(WEB_DIR)/node_modules $$TMP/node_modules; \
	cp $(WEB_DIR)/package.json $(WEB_DIR)/tsconfig.json $(WEB_DIR)/eslint.config.* $(WEB_DIR)/components.json $$TMP/ 2>/dev/null || true; \
	cd $$TMP && ./node_modules/.bin/eslint src

lint-oss-voice: ## Lint the Voice server as it appears in the OSS build
	@echo "── [OSS] Voice server ──"
	@set -e; \
	TMP="$(CURDIR)/.tmp-oss-voice"; \
	rm -rf $$TMP; \
	mkdir -p $$TMP/voice-server $$TMP/proto $$TMP/integrations; \
	rsync -a \
		--exclude='*_internal.rs' \
		--exclude='target/' \
		$(RUST_DIR)/ $$TMP/voice-server/; \
	rsync -a proto/ $$TMP/proto/; \
	rsync -a \
		--exclude='target/' \
		integrations/ $$TMP/integrations/; \
	export CARGO_TARGET_DIR="$(CURDIR)/target-oss"; \
	cd $$TMP/voice-server/engine && cargo clippy -- -D warnings; \
	cd $$TMP/voice-server/server && cargo clippy -- -D warnings; \
	rm -rf $$TMP

# ── Test ─────────────────────────────────────────────────────────
.PHONY: test test-api test-engine test-server test-integrations
test: test-api test-engine test-server test-integrations ## Run all tests

test-api:
	cd $(API_DIR) && uv run pytest tests/ -v
test-engine:
	cd $(RUST_DIR)/engine && cargo test
test-server:
	cd $(RUST_DIR)/server && cargo test
test-integrations:
	cd integrations && cargo test

# ── ML Inference Utilities ───────────────────────────────────────
.PHONY: inf-build-stt inf-build-tts inf-stt inf-tts inf-health inf-gpu

inf-build-stt: ## Build the standalone Whisper GPU inference container
	cd $(ML_DIR) && docker build --build-arg ENGINE=whisper -t prime8-inference-stt .

inf-build-tts: ## Build the standalone Fish GPU inference container
	cd $(ML_DIR) && docker build --build-arg ENGINE=fish -t prime8-inference-tts .

inf-stt: ## Run STT standalone container (Fast WS, Port 9001)
	docker run --rm -it --gpus all -p 9001:9001 prime8-inference-stt stt --engine whisper --port 9001

inf-tts: ## Run TTS standalone container (Fast WS, Port 9002)
	docker run --rm -it --gpus all -p 9002:9002 prime8-inference-tts tts --engine fish --port 9002

inf-health: ## Check ML service health
	@echo "── STT ──"

# ── Studio API Utilities ─────────────────────────────────────────
.PHONY: api-dev api-serve api-migrate api-migration api-clean api-env-schema api-schema

api-dev: ## Install Studio API dev dependencies
	cd $(API_DIR) && uv sync

api-serve: api-clean ## Start Studio API dev server
	cd $(API_DIR) && uv run python -m uvicorn app.main:app --host 127.0.0.1 --port 8000 --reload

api-env-schema: ## Gen Studio API .env.example
	cd $(API_DIR) && uv run python -m scripts.dump_env_schema > .env.example
	@echo "✓ .env.example updated"

api-schema: ## Regenerate JSON Schema for Agent configs (from Pydantic + Proto)
	cd $(API_DIR) && uv run python -m scripts.generate_schema

api-migrate: ## Run Studio API db migrations
	cd $(API_DIR) && uv run python -m scripts.check_squash_migration
	cd $(API_DIR) && uv run alembic upgrade head

api-migration: ## Create migration (make api-migration msg="...")
	cd $(API_DIR) && uv run alembic revision --autogenerate -m "$(msg)"

api-clean: ## Remove Studio API caches
	cd $(API_DIR) && find . -type d -name __pycache__ -exec rm -rf {} + 2>/dev/null || true
	cd $(API_DIR) && rm -rf .pytest_cache .ruff_cache .mypy_cache htmlcov .coverage build/ *.egg-info
	cd $(API_DIR) && find app/ -name '*.so' -delete 2>/dev/null || true
	cd $(API_DIR) && find app/ -name '_audio_core.c' -delete 2>/dev/null || true

# ── Release ──────────────────────────────────────────────────────
.PHONY: release-prep release-publish

release-prep: ## Create a release PR for a new version (e.g. make release-prep VERSION=0.2.0)
	@if [ -z "$(VERSION)" ]; then \
		echo "Error: VERSION is required. Usage: make release-prep VERSION=0.2.0"; \
		exit 1; \
	fi
	@./release.sh prep $(VERSION)

release-publish: ## Tag and publish the release (e.g. make release-publish VERSION=0.2.0)
	@if [ -z "$(VERSION)" ]; then \
		echo "Error: VERSION is required. Usage: make release-publish VERSION=0.2.0"; \
		exit 1; \
	fi
	@./release.sh publish $(VERSION)

# ── Help ─────────────────────────────────────────────────────────

.PHONY: help
help: ## Show available targets
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-19s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
