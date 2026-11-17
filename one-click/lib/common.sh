#!/usr/bin/env bash

log() {
  printf '[one-click] %s\n' "$*"
}

warn() {
  printf '[one-click] warning: %s\n' "$*" >&2
}

die() {
  printf '[one-click] error: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

is_root() {
  [[ "$(id -u)" == "0" ]]
}

prompt_value() {
  local var_name="$1"
  local prompt="$2"
  local default_value="${3:-}"
  local value
  if [[ -n "$default_value" ]]; then
    read -r -p "$prompt [$default_value]: " value
    value="${value:-$default_value}"
  else
    read -r -p "$prompt: " value
  fi
  printf -v "$var_name" '%s' "$value"
}

prompt_secret() {
  local var_name="$1"
  local prompt="$2"
  local value
  read -r -s -p "$prompt: " value
  printf '\n' >&2
  printf -v "$var_name" '%s' "$value"
}

confirm() {
  local prompt="$1"
  local default="${2:-n}"
  local suffix answer
  if [[ "$default" == "y" ]]; then
    suffix='[Y/n]'
  else
    suffix='[y/N]'
  fi
  read -r -p "$prompt $suffix " answer
  answer="${answer:-$default}"
  case "$answer" in
    y|Y|yes|YES) return 0 ;;
    *) return 1 ;;
  esac
}

write_secret_summary() {
  local name="$1"
  local value="${2:-}"
  if [[ -n "$value" ]]; then
    log "$name: set"
  else
    log "$name: not set"
  fi
}

yaml_quote() {
  python3.11 - "$1" <<'PY'
import sys
value = sys.argv[1]
if "\n" in value or "\r" in value:
    raise SystemExit("YAML scalar values must not contain newlines")
print("'" + value.replace("'", "''") + "'")
PY
}

json_string() {
  python3.11 - "$1" <<'PY'
import json
import sys
print(json.dumps(sys.argv[1]))
PY
}

first_existing_file() {
  local path
  for path in "$@"; do
    if [[ -f "$path" ]]; then
      printf '%s\n' "$path"
      return 0
    fi
  done
  return 1
}
