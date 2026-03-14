.PHONY: gen lint test build publish \
        dev-up dev-up-full dev-down dev-reset dev-logs dev-ps \
        test-unit test-integration test-parity \
        oms-check oms-build oms-test oms-test-integration oms-bench oms-e2e-bench oms-run oms-redis-clear

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
#   make dev-up    → start infra (NATS/Redis/PG/Vault/mock-gw); oms-svc NOT started
#   make oms-run   → run OMS locally (cargo run, with hot-reload)
#   make oms-e2e-bench
#
# Fully-dockerised stack (no local OMS):
#   make dev-up-full  → start everything including oms-svc container
#
COMPOSE := docker compose -f devops/docker-compose.yml

dev-up: ## Start infra only (NATS, Redis, PG, Vault, mock-gw); run OMS locally with make oms-run
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

dev-ps:
	$(COMPOSE) ps

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

oms-run: ## Run OMS locally (requires dev stack up: make dev-up)
	cd rust && ZK_OMS_ID=oms_dev_1 \
	           ZK_NATS_URL=nats://localhost:4222 \
	           ZK_REDIS_URL=redis://localhost:6379 \
	           ZK_PG_URL=postgres://zk:zk@localhost:5432/zkbot \
	           ZK_GRPC_PORT=50051 \
	           ZK_GATEWAY_KV_PREFIX=svc.gw \
	           ZK_RISK_CHECK_ENABLED=false \
	           RUST_LOG=zk_oms_svc=debug,info \
	           cargo run -p zk-oms-svc

oms-redis-clear: ## Delete all oms:{OMS_ID}:* keys from dev Redis (OMS_ID default: oms_dev_1)
	./scripts/clear_oms_redis.sh
