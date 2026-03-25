.PHONY: gen lint test build publish \
        dev-up dev-up-full dev-down dev-reset dev-logs dev-logs-save dev-ps dev-health dev-reset-redis dev-reset-pg \
        test-unit test-integration test-parity \
        oms-check oms-build oms-test oms-test-integration oms-bench oms-e2e-bench oms-run oms-run-release oms-redis-clear \
        gw-check gw-build gw-test gw-run gw-okx-demo gw-okx-demo-pilot \
        rtmd-sim-run rtmd-okx-demo rtmd-okx-demo-pilot \
        refdata-run pilot-run engine-run \
        pilot-java-build pilot-java-test pilot-java-run \
        oms-run-pilot gw-run-pilot

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
