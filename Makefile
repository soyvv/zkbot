.PHONY: gen lint test build publish \
        dev-up dev-down dev-reset dev-logs dev-ps \
        test-unit test-integration test-parity

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
COMPOSE := docker compose -f devops/docker-compose.yml

dev-up:
	$(COMPOSE) up -d --build
	devops/init/vault.sh

dev-down:
	$(COMPOSE) down

dev-reset:
	$(COMPOSE) down -v
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
