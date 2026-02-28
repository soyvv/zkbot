#!/usr/bin/env bash
set -euo pipefail

SCRIPT_PATH="$(cd "$(dirname "$0")" && pwd)"
BASE_PATH="$(dirname "$SCRIPT_PATH")"
PROTO_PATH="${BASE_PATH}/protos"
OUT_PATH="${BASE_PATH}/libs/zk-datamodel/src/zk_datamodel"

if [[ -x "${BASE_PATH}/.venv/bin/python" ]]; then
  PYTHON_BIN="${BASE_PATH}/.venv/bin/python"
  BETTERPROTO_PLUGIN="${BASE_PATH}/.venv/bin/protoc-gen-python_betterproto"
else
  PYTHON_BIN="${PYTHON_BIN:-python}"
  BETTERPROTO_PLUGIN="${BETTERPROTO_PLUGIN:-$(command -v protoc-gen-python_betterproto || true)}"
fi

if ! command -v "${PYTHON_BIN}" >/dev/null 2>&1; then
  echo "[ERR] Python not found: ${PYTHON_BIN}" >&2
  exit 1
fi

if [[ -z "${BETTERPROTO_PLUGIN}" || ! -x "${BETTERPROTO_PLUGIN}" ]]; then
  echo "[ERR] protoc-gen-python_betterproto not found: ${BETTERPROTO_PLUGIN:-<empty>}" >&2
  exit 1
fi

if [[ ! -d "${PROTO_PATH}" ]]; then
  echo "[ERR] Proto directory not found: ${PROTO_PATH}" >&2
  exit 1
fi

if [[ ! -d "${OUT_PATH}" ]]; then
  echo "[ERR] Output directory not found: ${OUT_PATH}" >&2
  exit 1
fi

PROTO_FILES=(
  "${PROTO_PATH}/common.proto"
  "${PROTO_PATH}/exch-gateway.proto"
  "${PROTO_PATH}/rpc-exch-gateway.proto"
  "${PROTO_PATH}/oms.proto"
  "${PROTO_PATH}/rpc-oms.proto"
  "${PROTO_PATH}/ods.proto"
  "${PROTO_PATH}/rpc-ods.proto"
  "${PROTO_PATH}/rpc-refdata.proto"
  "${PROTO_PATH}/rtmd.proto"
  "${PROTO_PATH}/strategy.proto"
)

echo "${SCRIPT_PATH}"
echo "${BASE_PATH}"
echo "${PROTO_PATH}"

"${PYTHON_BIN}" -m grpc_tools.protoc \
  "--proto_path=${PROTO_PATH}" \
  "--plugin=protoc-gen-python_betterproto=${BETTERPROTO_PLUGIN}" \
  "--python_betterproto_out=${OUT_PATH}" \
  "${PROTO_FILES[@]}"

"${PYTHON_BIN}" "${SCRIPT_PATH}/gen_proto_legacy_compat.py"

echo "Generated BetterProto Python modules in ${OUT_PATH}"
