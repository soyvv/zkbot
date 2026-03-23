#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <service-name> <command> [args...]" >&2
  exit 1
fi

service_name="$1"
shift

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
log_root="${ZK_DEV_LOG_DIR:-$repo_root/devops/logs}"
service_log_dir="$log_root/$service_name"
timestamp="$(date '+%Y%m%d-%H%M%S')"
log_file="$service_log_dir/$timestamp.log"

mkdir -p "$service_log_dir"
ln -sfn "$log_file" "$service_log_dir/latest.log"

echo "capturing $service_name logs to $log_file"
"$@" 2>&1 | tee -a "$log_file"
