.PHONY: gen lint test build publish

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
