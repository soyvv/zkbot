#!/usr/bin/env bash
# audit_dependency_contract.sh — enforce the forbidden-pattern rules from
# docs/system-arch/dependency-contract.md. Zero hits required for success.
#
# Read-only; safe to wire into `make ci-lint` and pre-commit hooks.

set -u
set -o pipefail

if ! command -v rg >/dev/null 2>&1; then
  echo "audit: ripgrep (rg) is required" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

fail=0
section() { printf '\n== %s ==\n' "$1"; }
run_rule() {
  local name="$1"; shift
  local out
  out="$(rg --color=never -n "$@" || true)"
  if [[ -n "$out" ]]; then
    printf '\n[FAIL] %s\n%s\n' "$name" "$out"
    fail=1
  fi
}

# 1. sys.path mutation outside dev.py / tests / legacy
run_rule "sys.path mutation outside dev.py/tests/legacy" \
  'sys\.path\.(insert|append|extend)' \
  python/libs python/services python/tools \
  --glob '!**/legacy/**' \
  --glob '!**/tests/**' \
  --glob '!**/dev.py'

# 2. PYTHONPATH=... in the Makefile and scripts. Docs are allowed to reference
# the forbidden string literally (they document the contract). Dockerfiles
# in devops/ are out of scope for zb-00029 (tracked as a follow-up devops
# ticket — see docs/system-arch/dependency-contract.md §Migration notes).
run_rule "PYTHONPATH= in Makefile/scripts" \
  'PYTHONPATH=' \
  Makefile scripts \
  --glob '!scripts/audit_dependency_contract.sh'

# 3. Retired env vars anywhere in the non-legacy tree
run_rule "retired env vars" \
  'ZK_VENUE_VENV|ZK_VENUE_ROOT|ZK_DEV_MODE|ENGINE_PYTHONPATH|ZK_ENGINE_BINARY_PATH' \
  rust Makefile java python venue-integrations \
  --glob '!**/legacy/**' \
  --glob '!docs/**'

# 4. `maturin develop` is forbidden in code, build scripts, and Cargo/py
# project configs (wheel-only contract). Doc and ticket references that
# describe the prohibition are allowed.
run_rule "maturin develop" \
  'maturin develop' \
  Makefile scripts rust python venue-integrations \
  --glob '!scripts/audit_dependency_contract.sh'

# 5. python_search_path field in code / schemas / manifests
run_rule "python_search_path field" \
  'python_search_path' \
  rust service-manifests python \
  --glob '!docs/**'

# 6. spec_from_file_location outside zk_strategy/dev.py
run_rule "spec_from_file_location outside dev.py" \
  'spec_from_file_location' \
  python/libs python/services python/tools \
  --glob '!**/zk_strategy/dev.py' \
  --glob '!**/tests/**' \
  --glob '!**/legacy/**'

# 7. Repo-relative zkstrategy_research leaks outside docs
run_rule "zkstrategy_research repo-relative leak" \
  '\.\./zkstrategy_research' \
  Makefile rust python

# 8. No candidate-list / fallback logic in BotService engine-binary resolution.
# Bans the removed helper (`engineBinaryCandidates`), the removed env var
# (`ZK_ENGINE_BINARY_PATH`), and common candidate/fallback-loop tokens in
# PilotProperties and the engine-path resolver itself. The broader JDBC
# `fallback` in buildEnrichedConfig is unrelated and lives on a different
# file region; this rule targets only PilotProperties + the engine-binary
# resolver helper.
run_rule "candidate-list or fallback tokens in BotService engine-binary resolution" \
  'engineBinaryCandidates|ZK_ENGINE_BINARY_PATH|resolveEngineBinaryCandidates' \
  java/pilot-service/src/main/java/com/zkbot/pilot/bot/BotService.java \
  java/pilot-service/src/main/java/com/zkbot/pilot/config/PilotProperties.java

if [[ $fail -ne 0 ]]; then
  echo
  echo "audit: dependency-contract rules violated (see docs/system-arch/dependency-contract.md)" >&2
  exit 1
fi

echo "audit: dependency-contract OK"
exit 0
