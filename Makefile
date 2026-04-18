.PHONY: gen lint test build publish \
        dev-up dev-up-full dev-down dev-reset dev-logs dev-logs-save dev-ps dev-health dev-reset-redis dev-reset-pg \
        test-unit test-integration test-parity \
        oms-check oms-build oms-test oms-test-integration oms-bench oms-e2e-bench oms-run oms-run-release oms-redis-clear \
        gw-check gw-build gw-test gw-run gw-okx-demo gw-okx-demo-pilot \
        rtmd-sim-run rtmd-okx-demo rtmd-okx-demo-pilot \
        oanda-venv gw-oanda-demo gw-oanda-demo-pilot rtmd-oanda-demo rtmd-oanda-demo-pilot \
        refdata-run pilot-run engine-run \
        pilot-java-build pilot-java-test pilot-java-run \
        oms-run-pilot gw-run-pilot \
        recorder-check recorder-build recorder-run \
        engine-smoke-run-pilot engine-ets-oanda-run-pilot

gen:
	buf generate protos

lint:
	ruff check .
	mypy libs services

test:
	pytest -q

build:
	find libs -maxdepth 2 -name pyproject.toml -execdir uv build \;

publish:
	@echo "TODO: implement publish script to Nexus"

# ── Local dev stack ───────────────────────────────────────────────────────────
# Typical local workflow:
#   make dev-up    → start infra (NATS/Redis/PG/Vault/gw-sim); oms-svc NOT started
#   make oms-run   → run OMS locally (cargo run, with hot-reload)
#   make oms-e2e-bench
#
# Fully-dockerised stack (no local OMS):
#   make dev-up-full  → start everything including oms-svc container
#
COMPOSE := docker compose -f devops/docker-compose.yml
DEV_LOG_DIR ?= $(CURDIR)/devops/logs

dev-up: ## Start infra only (NATS, Redis, PG, Vault, gw-sim); run OMS locally with make oms-run
	$(COMPOSE) up -d --build
	devops/init/vault.sh
	$(MAKE) oms-redis-clear

dev-up-full: ## Start full stack including oms-svc container (no local OMS needed)
	$(COMPOSE) --profile full up -d --build
	devops/init/vault.sh
	$(MAKE) oms-redis-clear

dev-down:
	$(COMPOSE) --profile full down

dev-reset: ## Wipe volumes (Redis/PG/NATS) and restart clean
	$(COMPOSE) --profile full down -v
	$(MAKE) dev-up

dev-logs:
	$(COMPOSE) logs -f

dev-logs-save:
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh docker-dev $(COMPOSE) logs -f --timestamps

dev-ps:
	$(COMPOSE) ps

dev-health: ## Show container health/status with ports
	$(COMPOSE) ps --format "table {{.Name}}\t{{.Status}}\t{{.Ports}}"

dev-reset-redis: ## Flush Redis without full stack reset
	docker exec zk-dev-redis-1 redis-cli flushall

dev-reset-pg: ## Wipe Postgres volume and restart (re-runs init scripts)
	$(COMPOSE) stop postgres && $(COMPOSE) rm -f postgres && \
	docker volume rm zkbot_postgres_data 2>/dev/null; \
	$(COMPOSE) up -d postgres

# ── Rust tests ────────────────────────────────────────────────────────────────
test-unit:
	cd rust && cargo test --workspace --exclude zk-pyo3-rs

test-integration:
	cd rust && cargo test --workspace --exclude zk-pyo3-rs -- --ignored

test-parity:
	cd rust && cargo test --workspace -- parity

# ── OMS service ───────────────────────────────────────────────────────────────
oms-check:
	cd rust && cargo check -p zk-oms-svc

oms-build:
	cd rust && cargo build --release -p zk-oms-svc

oms-test:
	cd rust && cargo test -p zk-oms-svc

oms-test-integration:
	cd rust && cargo test -p zk-oms-svc -- --ignored

oms-bench:
	cd rust && cargo bench -p zk-oms-svc

oms-e2e-bench: ## E2E latency bench (dev stack must be up: make dev-up && make oms-run)
	cd rust && cargo run --example e2e_latency -p zk-oms-svc --release

oms-run: ## Run OMS locally (debug build; requires dev stack up: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh oms zsh -lc 'cd rust && ZK_OMS_ID=oms_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_REDIS_URL=redis://localhost:6379 \
	           ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	           ZK_GRPC_PORT=50051 \
	           ZK_GATEWAY_KV_PREFIX=svc.gw \
	           ZK_RISK_CHECK_ENABLED=false \
	           RUST_LOG=zk_oms_svc=debug,info \
	           cargo run -p zk-oms-svc'

oms-run-release: ## Run OMS locally (release build; requires dev stack up: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh oms-release zsh -lc 'cd rust && ZK_OMS_ID=oms_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_REDIS_URL=redis://localhost:6379 \
	           ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	           ZK_GRPC_PORT=50051 \
	           ZK_GATEWAY_KV_PREFIX=svc.gw \
	           ZK_RISK_CHECK_ENABLED=false \
	           RUST_LOG=zk_oms_svc=debug,info \
	           cargo run --release -p zk-oms-svc'

oms-run-pilot: ## Run OMS with Pilot bootstrap (requires: make dev-up + make pilot-java-run)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh oms-pilot zsh -lc 'cd rust && ZK_OMS_ID=oms_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_REDIS_URL=redis://localhost:6379 \
	           ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	           ZK_GRPC_PORT=50051 \
	           ZK_GATEWAY_KV_PREFIX=svc.gw \
	           ZK_RISK_CHECK_ENABLED=false \
	           ZK_BOOTSTRAP_TOKEN=dev-oms-token-1 \
	           ZK_INSTANCE_TYPE=OMS \
	           ZK_ENV=dev \
	           RUST_LOG=zk_oms_svc=debug,zk_infra_rs=debug,info \
	           cargo run -p zk-oms-svc'

oms-redis-clear: ## Delete all oms:{OMS_ID}:* keys from dev Redis (OMS_ID default: oms_dev_1)
	./scripts/clear_oms_redis.sh

# ── Recorder service ────────────────────────────────────────────────────────
recorder-check:
	cd rust && cargo check -p zk-recorder-svc

recorder-build:
	cd rust && cargo build --release -p zk-recorder-svc

recorder-run: ## Run recorder locally (requires dev stack: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh recorder zsh -lc 'cd rust && ZK_RECORDER_ID=recorder_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_PG_URL=postgresql://zk:zk@localhost:5432/zkbot \
	           ZK_PG_MAX_CONNECTIONS=5 \
	           RUST_LOG=zk_recorder_svc=debug,info \
	           cargo run -p zk-recorder-svc'

# ── Gateway service ─────────────────────────────────────────────────────────
gw-check:
	cd rust && cargo check -p zk-gw-svc

gw-build:
	cd rust && cargo build --release -p zk-gw-svc

gw-test:
	cd rust && cargo test -p zk-gw-svc

gw-run: ## Run gateway simulator locally (requires NATS: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw zsh -lc 'cd rust && ZK_GW_ID=gw_sim_1 \
	           ZK_VENUE=simulator \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_ACCOUNT_ID=9001 \
	           ZK_MATCH_POLICY=fcfs \
	           ZK_MOCK_BALANCES="BTC:10,USDT:100000,ETH:50" \
	           ZK_GRPC_PORT=51051 \
	           ZK_ADMIN_GRPC_PORT=51052 \
	           ZK_ENABLE_ADMIN_CONTROLS=true \
	           RUST_LOG=zk_gw_svc=debug,info \
	           cargo run -p zk-gw-svc'

gw-run-pilot: ## Run gateway simulator with Pilot bootstrap (requires: make dev-up + make pilot-java-run)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw-pilot zsh -lc 'cd rust && ZK_GW_ID=gw_sim_1 \
	           ZK_VENUE=simulator \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_ACCOUNT_ID=9001 \
	           ZK_MATCH_POLICY=fcfs \
	           ZK_MOCK_BALANCES="BTC:10,USDT:100000,ETH:50" \
	           ZK_GRPC_PORT=51051 \
	           ZK_ADMIN_GRPC_PORT=51052 \
	           ZK_ENABLE_ADMIN_CONTROLS=true \
	           ZK_BOOTSTRAP_TOKEN=dev-gw-token-1 \
	           ZK_INSTANCE_TYPE=GW \
	           ZK_ENV=dev \
	           RUST_LOG=zk_gw_svc=debug,zk_infra_rs=debug,info \
	           cargo run -p zk-gw-svc'

gw-okx-demo: ## Run OKX gateway against demo account (requires NATS: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw-okx-demo bash -c '\
	  source devops/scripts/load-okx-demo-env.sh && \
	  cd rust && ZK_GW_ID=gw_okx_demo \
	  ZK_VENUE=okx \
	  ZK_NATS_URL=nats://localhost:4222 \
	  ZK_ACCOUNT_ID=9001 \
	  ZK_GRPC_PORT=51053 \
	  ZK_VENUE_CONFIG='"'"'{"api_key":"env:apikey","secret_key":"env:secretkey","passphrase":"env:OKX_PASSPHRASE","demo_mode":true}'"'"' \
	  RUST_LOG=zk_gw_svc=debug,zk_venue_okx=debug,info \
	  cargo run --release -p zk-gw-svc'

gw-okx-demo-pilot: ## Run OKX demo trading GW with Pilot bootstrap (requires: make dev-up + make pilot-java-run)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw-okx-demo-pilot bash -c '\
	  source devops/scripts/load-okx-demo-env.sh && \
	  cd rust && ZK_GW_ID=gw_okx_demo1 \
	  ZK_VENUE=okx \
	  ZK_NATS_URL=nats://localhost:4222 \
	  VAULT_ADDR=http://localhost:8200 \
	  VAULT_TOKEN=dev-root-token \
	  ZK_ACCOUNT_ID=9001 \
	  ZK_GRPC_PORT=51053 \
	  ZK_VENUE_CONFIG='"'"'{"api_key":"env:apikey","secret_key":"env:secretkey","passphrase":"env:OKX_PASSPHRASE","demo_mode":true}'"'"' \
	  ZK_BOOTSTRAP_TOKEN=302ebb8add2a9a493d379119becb70a912867ac808fb7ebd5f155ce1d1e1ed20 \
	  ZK_INSTANCE_TYPE=GW \
	  ZK_ENV=dev \
	  RUST_LOG=zk_gw_svc=debug,zk_venue_okx=debug,zk_infra_rs=debug,info \
	  cargo run --release -p zk-gw-svc'

# ── RTMD gateway service ──────────────────────────────────────────────────────
rtmd-sim-run: ## Run RTMD simulator gateway locally (requires NATS: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh rtmd-sim zsh -lc 'cd rust && ZK_MDGW_ID=mdgw_sim_1 \
	           ZK_VENUE=simulator \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_GRPC_PORT=52051 \
	           RUST_LOG=zk_rtmd_gw_svc=debug,info \
	           cargo run -p zk-rtmd-gw-svc'

rtmd-okx-demo: ## Run OKX RTMD gateway against demo/public endpoints (requires NATS: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh rtmd-okx-demo zsh -lc 'cd rust && ZK_MDGW_ID=mdgw_okx_demo \
	           ZK_VENUE=okx \
	           ZK_GRPC_PORT=51054 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_VENUE_CONFIG='"'"'{"demo_mode":true}'"'"' \
	           RUST_LOG=zk_rtmd_gw_svc=debug,zk_rtmd_rs=debug,zk_venue_okx=debug,info \
	           cargo run --release -p zk-rtmd-gw-svc'

rtmd-okx-demo-pilot: ## Run OKX RTMD gateway with Pilot bootstrap (requires: make dev-up + make pilot-java-run)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh mdgw-okx-pilot zsh -lc 'cd rust && ZK_MDGW_ID=mdgw-okx-demo1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_GRPC_PORT=51054 \
	           ZK_BOOTSTRAP_TOKEN=3036b51c7a672a951e8a6ea0bb0ab5a685b77cc3a5d376ecf596fcf49ac5c312 \
	           ZK_INSTANCE_TYPE=MDGW \
	           ZK_ENV=dev \
	           RUST_LOG=zk_rtmd_gw_svc=debug,zk_rtmd_rs=debug,zk_venue_okx=debug,zk_infra_rs=debug,info \
	           cargo run --release -p zk-rtmd-gw-svc'

# ── OANDA venue venv ─────────────────────────────────────────────────────────
# Resolve Python through uv so the dev stack does not depend on pyenv-managed paths.
# The venv and embedded PyO3 runtime must use the same interpreter.
UV_PYTHON_SPEC   ?= 3.13
PYO3_PYTHON_BIN  := $(shell uv python find --python-preference only-managed $(UV_PYTHON_SPEC))
PYO3_PYTHON_HOME := $(shell $(PYO3_PYTHON_BIN) -c 'import sys; print(sys.base_prefix)')
STRATEGY_VENV_PYTHON_BIN := $(abspath ../zkstrategy_research/.venv/bin/python)
STRATEGY_VENV_PYTHON_HOME := $(shell $(STRATEGY_VENV_PYTHON_BIN) -c 'import sys; print(sys.base_prefix)')
STRATEGY_VENV_SITE_PACKAGES := $(shell $(STRATEGY_VENV_PYTHON_BIN) -c 'import sysconfig; print(sysconfig.get_paths()["purelib"])')
ENGINE_PYTHONPATH := $(STRATEGY_VENV_SITE_PACKAGES):$(abspath ../zkstrategy_research/zk-strategylib):$(CURDIR)/libs/zk-core/src:$(CURDIR)/libs/zk-datamodel/src
oanda-venv: ## Create/update OANDA Python venv
	cd venue-integrations/oanda && uv venv .venv --python $(UV_PYTHON_SPEC) && \
	uv pip install -e ".[dev]" --python .venv/bin/python \
	$(if $(ZK_PYPI_EXTRA_INDEX),--extra-index-url $(ZK_PYPI_EXTRA_INDEX),)

# ── OANDA gateway service ───────────────────────────────────────────────────
gw-oanda-demo: oanda-venv ## Run OANDA gateway (practice, direct mode)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw-oanda-demo \
	  bash -c 'source devops/scripts/load-oanda-token.sh && \
	  cd rust && \
	  unset VIRTUAL_ENV CONDA_PREFIX && \
	  PYO3_PYTHON=$(PYO3_PYTHON_BIN) \
	  PYTHONHOME=$(PYO3_PYTHON_HOME) \
	  ZK_GW_ID=gw_oanda_demo \
	  ZK_VENUE=oanda \
	  ZK_NATS_URL=nats://localhost:4222 \
	  ZK_ACCOUNT_ID=8003 \
	  ZK_EXCH_ACCOUNT_ID=101-003-26138765-001 \
	  ZK_GRPC_PORT=51055 \
	  ZK_VENUE_ROOT=$(CURDIR)/venue-integrations \
	  ZK_VENUE_CONFIG='"'"'{"environment":"practice","account_id":"101-003-26138765-001","secret_ref":"kv/trading/gw/8003"}'"'"' \
	  RUST_LOG=zk_gw_svc=debug,zk_pyo3_bridge=info,warn \
	  cargo run --release -p zk-gw-svc --features python-venue'

gw-oanda-demo-pilot: oanda-venv ## Run OANDA gateway with Pilot bootstrap
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh gw-oanda-demo-pilot \
	  bash -c 'cd rust && \
	  unset VIRTUAL_ENV CONDA_PREFIX && \
	  PYO3_PYTHON=$(PYO3_PYTHON_BIN) \
	  PYTHONHOME=$(PYO3_PYTHON_HOME) \
	  ZK_GW_ID=gw_oanda_demo1 \
	  ZK_VENUE=oanda \
	  ZK_NATS_URL=nats://localhost:4222 \
	  ZK_GRPC_PORT=51055 \
	  ZK_VENUE_ROOT=$(CURDIR)/venue-integrations \
	  VAULT_ADDR=http://localhost:8200 \
	  VAULT_TOKEN=dev-root-token \
	  ZK_BOOTSTRAP_TOKEN=$(ZK_OANDA_GW_BOOTSTRAP_TOKEN) \
	  ZK_INSTANCE_TYPE=GW \
	  ZK_ENV=dev \
	  RUST_LOG=zk_gw_svc=debug,zk_pyo3_bridge=info,zk_infra_rs=debug,warn \
	  cargo run --release -p zk-gw-svc --features python-venue'

# ── OANDA RTMD gateway service ──────────────────────────────────────────────
rtmd-oanda-demo: oanda-venv ## Run OANDA RTMD gateway (practice, direct mode)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh rtmd-oanda-demo \
	  bash -c 'source devops/scripts/load-oanda-token.sh && \
	  cd rust && \
	  unset VIRTUAL_ENV CONDA_PREFIX && \
	  PYO3_PYTHON=$(PYO3_PYTHON_BIN) \
	  PYTHONHOME=$(PYO3_PYTHON_HOME) \
	  ZK_MDGW_ID=mdgw_oanda_demo \
	  ZK_VENUE=oanda \
	  ZK_NATS_URL=nats://localhost:4222 \
	  ZK_GRPC_PORT=52055 \
	  ZK_VENUE_ROOT=$(CURDIR)/venue-integrations \
	  ZK_VENUE_CONFIG='"'"'{"environment":"practice","account_id":"101-003-26138765-001","secret_ref":"kv/trading/gw/8003"}'"'"' \
	  RUST_LOG=zk_rtmd_gw_svc=debug,zk_pyo3_bridge=info,warn \
	  cargo run --release -p zk-rtmd-gw-svc --features python-venue'

rtmd-oanda-demo-pilot: oanda-venv ## Run OANDA RTMD gateway with Pilot bootstrap
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh rtmd-oanda-demo-pilot \
	  bash -c 'cd rust && \
	  unset VIRTUAL_ENV CONDA_PREFIX && \
	  PYO3_PYTHON=$(PYO3_PYTHON_BIN) \
	  PYTHONHOME=$(PYO3_PYTHON_HOME) \
	  ZK_MDGW_ID=mdgw_oanda_demo1 \
	  ZK_VENUE=oanda \
	  ZK_NATS_URL=nats://localhost:4222 \
	  ZK_GRPC_PORT=52055 \
	  ZK_VENUE_ROOT=$(CURDIR)/venue-integrations \
	  VAULT_ADDR=http://localhost:8200 \
	  VAULT_TOKEN=dev-root-token \
	  ZK_BOOTSTRAP_TOKEN=$(ZK_OANDA_MDGW_BOOTSTRAP_TOKEN) \
	  ZK_INSTANCE_TYPE=MDGW \
	  ZK_ENV=dev \
	  RUST_LOG=zk_rtmd_gw_svc=debug,zk_pyo3_bridge=info,zk_infra_rs=debug,warn \
	  cargo run --release -p zk-rtmd-gw-svc --features python-venue'

# ── Refdata service ───────────────────────────────────────────────────────────
refdata-run: ## Run refdata-svc locally (requires NATS+PG: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh refdata zsh -lc '\
	ZK_NATS_URL=nats://localhost:4222 \
	ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	ZK_REFDATA_LOGICAL_ID=refdata_dev_1 \
	ZK_REFDATA_GRPC_PORT=50052 \
	uv run zk-refdata-svc'

# ── Pilot service ─────────────────────────────────────────────────────────────
pilot-run: ## Run pilot locally (requires NATS+PG: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh pilot zsh -lc 'cd services/zk-pilot && \
	ZK_NATS_URL=nats://localhost:4222 \
	ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	ZK_ENV=dev \
	ZK_PILOT_ID=pilot_dev_1 \
	ZK_HTTP_PORT=8090 \
	uv run python -m zk_pilot.main'

# ── Pilot Java service ───────────────────────────────────────────────────────
pilot-java-build: ## Build Java Pilot (Gradle)
	cd java && ./gradlew build -x test

pilot-java-test: ## Run Java Pilot tests
	cd java && ./gradlew test

pilot-java-run: ## Run Java Pilot locally (requires NATS+PG+Redis: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh pilot-java zsh -lc 'cd java && ZK_NATS_URL=nats://localhost:4222 \
	           ZK_PG_URL=jdbc:postgresql://localhost:5432/zkbot \
	           ZK_PG_USER=zk \
	           ZK_PG_PASS=zk \
	           ZK_REDIS_URL=redis://localhost:6379 \
	           ZK_ENV=dev \
	           ZK_PILOT_ID=pilot_dev_1 \
	           ZK_HTTP_PORT=8090 \
	           ZK_ENGINE_BINARY_PATH=$(CURDIR)/rust/target/debug/zk-engine-svc \
	           ZK_SERVICE_MANIFESTS_ROOT=$(CURDIR)/service-manifests \
	           ZK_VENUE_INTEGRATIONS_ROOT=$(CURDIR)/venue-integrations \
	           SPRING_PROFILES_ACTIVE=dev \
	           ./gradlew bootRun'

# ── Engine service ────────────────────────────────────────────────────────────
engine-run: ## Run engine-svc locally (requires NATS+PG: make dev-up)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh engine zsh -lc 'cd rust && ZK_ENGINE_ID=engine_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	           ZK_GRPC_PORT=50053 \
	           RUST_LOG=zk_engine_svc=debug,info \
	           cargo run -p zk-engine-svc'

engine-smoke-run-pilot: ## Run smoke bot engine outside orchestrator but with Pilot bootstrap; requires ZK_BOOTSTRAP_TOKEN
	@test -n "$(ZK_BOOTSTRAP_TOKEN)" || (echo "ZK_BOOTSTRAP_TOKEN is required, e.g. make engine-smoke-run-pilot ZK_BOOTSTRAP_TOKEN=<token>" && exit 1)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh engine-bot-smoke-okx-01 zsh -lc 'cd rust && \
	           cargo build -p zk-engine-svc && \
	           ZK_ENGINE_ID=bot-smoke-okx-01 \
	           ZK_INSTANCE_TYPE=ENGINE \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_ENV=dev \
	           ZK_GRPC_HOST=127.0.0.1 \
	           ZK_GRPC_PORT=50053 \
	           ZK_DISCOVERY_BUCKET=zk-svc-registry-v1 \
	           ZK_KV_HEARTBEAT_SECS=10 \
	           ZK_BOOTSTRAP_TOKEN=$(ZK_BOOTSTRAP_TOKEN) \
	           RUST_LOG=zk_engine_svc=debug,zk_trading_sdk_rs=debug,zk_infra_rs=debug,info \
	           cargo run -p zk-engine-svc'

engine-ets-oanda-run-pilot: ## Run the Pilot-managed ETS Oanda engine; requires ZK_BOOTSTRAP_TOKEN from Pilot bot onboarding
	@test -n "$(ZK_BOOTSTRAP_TOKEN)" || (echo "ZK_BOOTSTRAP_TOKEN is required, e.g. make engine-ets-oanda-run-pilot ZK_BOOTSTRAP_TOKEN=<token>" && exit 1)
	@test -x "$(STRATEGY_VENV_PYTHON_BIN)" || (echo "Missing strategy venv python at $(STRATEGY_VENV_PYTHON_BIN). Create/update zkstrategy_research/.venv first." && exit 1)
	ZK_DEV_LOG_DIR=$(DEV_LOG_DIR) ./devops/scripts/run-with-log.sh engine-ets-oanda-pilot zsh -lc 'cd rust && \
	           unset VIRTUAL_ENV CONDA_PREFIX PYTHONHOME PYTHONEXECUTABLE __PYVENV_LAUNCHER__ && \
	           PYO3_PYTHON=$(STRATEGY_VENV_PYTHON_BIN) cargo build -p zk-engine-svc && \
	           DYLD_LIBRARY_PATH=$(STRATEGY_VENV_PYTHON_HOME)/lib \
	           PYTHONHOME=$(STRATEGY_VENV_PYTHON_HOME) \
	           PYTHONPATH=$(ENGINE_PYTHONPATH) \
	           ZK_ENGINE_ID=engine_ets_oanda_pilot_01 \
	           ZK_INSTANCE_TYPE=ENGINE \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_ENV=dev \
	           ZK_GRPC_HOST=127.0.0.1 \
	           ZK_GRPC_PORT=50093 \
	           ZK_DISCOVERY_BUCKET=zk-svc-registry-v1 \
	           ZK_KV_HEARTBEAT_SECS=10 \
	           ZK_BOOTSTRAP_TOKEN=$(ZK_BOOTSTRAP_TOKEN) \
	           RUST_LOG=zk_engine_svc=debug,zk_trading_sdk_rs=debug,zk_infra_rs=debug,info \
	           cargo run -p zk-engine-svc'
