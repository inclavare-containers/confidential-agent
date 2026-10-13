#!/usr/bin/env bash

normalize_aliyun_env() {
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    export ALIBABA_CLOUD_ACCESS_KEY_ID="${ALIBABA_CLOUD_ACCESS_KEY_ID:-$ALICLOUD_ACCESS_KEY}"
    export ALIBABA_CLOUD_ACCESS_KEY_SECRET="${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-$ALICLOUD_SECRET_KEY}"
  fi
  if [[ -n "${ALIBABA_CLOUD_ACCESS_KEY_ID:-}" && -n "${ALIBABA_CLOUD_ACCESS_KEY_SECRET:-}" ]]; then
    export ALICLOUD_ACCESS_KEY="${ALICLOUD_ACCESS_KEY:-$ALIBABA_CLOUD_ACCESS_KEY_ID}"
    export ALICLOUD_SECRET_KEY="${ALICLOUD_SECRET_KEY:-$ALIBABA_CLOUD_ACCESS_KEY_SECRET}"
  fi
  if [[ -n "${ALICLOUD_PROFILE:-}" ]]; then
    export ALIBABA_CLOUD_PROFILE="${ALIBABA_CLOUD_PROFILE:-$ALICLOUD_PROFILE}"
  fi
  if [[ -n "${ALIBABA_CLOUD_PROFILE:-}" ]]; then
    export ALICLOUD_PROFILE="${ALICLOUD_PROFILE:-$ALIBABA_CLOUD_PROFILE}"
  fi
  if [[ -n "${ALICLOUD_SHARED_CREDENTIALS_FILE:-}" ]]; then
    export ALIBABA_CLOUD_CREDENTIALS_FILE="${ALIBABA_CLOUD_CREDENTIALS_FILE:-$ALICLOUD_SHARED_CREDENTIALS_FILE}"
  fi
  if [[ -n "${ALIBABA_CLOUD_CREDENTIALS_FILE:-}" ]]; then
    export ALICLOUD_SHARED_CREDENTIALS_FILE="${ALICLOUD_SHARED_CREDENTIALS_FILE:-$ALIBABA_CLOUD_CREDENTIALS_FILE}"
  fi
}

aliyun_cli_profile_works() {
  command -v aliyun >/dev/null 2>&1 || return 1
  local profile="${1:-}"
  local credentials_file="${2:-}"
  local args=(sts GetCallerIdentity)
  if [[ -n "$profile" ]]; then
    args+=(--profile "$profile")
  fi
  if [[ -n "$credentials_file" ]]; then
    args+=(--config-path "$credentials_file")
  fi
  aliyun "${args[@]}" >/dev/null 2>&1 || return 1
}

aliyun_cli_current_profile() {
  local credentials_file="${1:-}"
  local output profile
  local args=(configure get profile)
  if [[ -n "$credentials_file" ]]; then
    args+=(--config-path "$credentials_file")
  fi
  output="$(aliyun "${args[@]}" 2>/dev/null)" || return 1
  profile="${output#profile=}"
  profile="${profile%%$'\n'*}"
  profile="${profile%$'\r'}"
  [[ -n "$profile" && "$profile" != "$output" ]] || return 1
  printf '%s\n' "$profile"
}

export_aliyun_cli_ak_profile() {
  local credentials_file="$1"
  local profile="$2"
  local credentials ak sk
  command -v jq >/dev/null 2>&1 || return 1
  [[ -r "$credentials_file" ]] || return 1

  credentials="$(
    jq -er --arg profile "$profile" '
      first(
        .profiles[]?
        | select(.name == $profile and .mode == "AK")
        | select(
            (.access_key_id | type) == "string"
            and (.access_key_id | length) > 0
            and (.access_key_secret | type) == "string"
            and (.access_key_secret | length) > 0
          )
        | [.access_key_id, .access_key_secret]
        | @tsv
      )
    ' "$credentials_file" 2>/dev/null
  )" || return 1
  IFS=$'\t' read -r ak sk <<<"$credentials"
  [[ -n "$ak" && -n "$sk" ]] || return 1

  export ALICLOUD_ACCESS_KEY="$ak"
  export ALICLOUD_SECRET_KEY="$sk"
  export ALIBABA_CLOUD_ACCESS_KEY_ID="$ak"
  export ALIBABA_CLOUD_ACCESS_KEY_SECRET="$sk"
}

configure_terraform_from_aliyun_cli() {
  local profile credentials_file
  credentials_file="${ALIBABA_CLOUD_CREDENTIALS_FILE:-${ALICLOUD_SHARED_CREDENTIALS_FILE:-}}"
  if [[ -z "$credentials_file" && -n "${HOME:-}" && -r "$HOME/.aliyun/config.json" ]]; then
    credentials_file="$HOME/.aliyun/config.json"
  fi
  [[ -n "$credentials_file" ]] || return 1

  profile="${ALIBABA_CLOUD_PROFILE:-${ALICLOUD_PROFILE:-}}"
  if [[ -n "$profile" ]]; then
    aliyun_cli_profile_works "$profile" "$credentials_file" || return 1
  else
    aliyun_cli_profile_works "" "$credentials_file" || return 1
    profile="$(aliyun_cli_current_profile "$credentials_file")" || return 1
  fi

  if ! export_aliyun_cli_ak_profile "$credentials_file" "$profile"; then
    warn "Aliyun CLI profile '$profile' is valid but is not a readable AK profile; use environment credentials or enter an AccessKey interactively"
    return 1
  fi

  export ALIBABA_CLOUD_PROFILE="$profile"
  export ALICLOUD_PROFILE="$profile"
  export ALIBABA_CLOUD_CREDENTIALS_FILE="$credentials_file"
  export ALICLOUD_SHARED_CREDENTIALS_FILE="$credentials_file"

  log "using Aliyun CLI AK profile '$profile' for one-click authentication"
}

ensure_aliyun_credentials() {
  normalize_aliyun_env
  if [[ -n "${ALICLOUD_ACCESS_KEY:-}" && -n "${ALICLOUD_SECRET_KEY:-}" ]]; then
    return
  fi
  if configure_terraform_from_aliyun_cli; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "Aliyun credentials are required. Set ALICLOUD_ACCESS_KEY/ALICLOUD_SECRET_KEY, ALIBABA_CLOUD_ACCESS_KEY_ID/ALIBABA_CLOUD_ACCESS_KEY_SECRET, or configure aliyun CLI."
  fi
  local ak sk
  prompt_value ak "Aliyun AccessKey ID"
  prompt_secret sk "Aliyun AccessKey Secret"
  [[ -n "$ak" && -n "$sk" ]] || die "Aliyun credentials cannot be empty"
  export ALICLOUD_ACCESS_KEY="$ak"
  export ALICLOUD_SECRET_KEY="$sk"
  export ALIBABA_CLOUD_ACCESS_KEY_ID="$ak"
  export ALIBABA_CLOUD_ACCESS_KEY_SECRET="$sk"
}

ensure_aliyun_cli_region() {
  # Shelter renders terraform data sources that shell out to the aliyun CLI.
  # CLI 3.x refuses to run without a region when no profile is configured,
  # even with --RegionId and credentials in the environment, so a fresh
  # deployer machine fails terraform plan with an empty external result.
  if [[ -z "${ALIBABA_CLOUD_REGION_ID:-}" && -n "${CA_REGION:-}" ]]; then
    export ALIBABA_CLOUD_REGION_ID="$CA_REGION"
  fi
}

ensure_bailian_credentials() {
  if [[ -n "${CA_BAILIAN_API_KEY:-}" ]]; then
    export DASHSCOPE_API_KEY="$CA_BAILIAN_API_KEY"
  fi
  if [[ -z "${DASHSCOPE_API_KEY:-}" && -n "${BAILIAN_API_KEY:-}" ]]; then
    export DASHSCOPE_API_KEY="$BAILIAN_API_KEY"
  fi
  if [[ -n "${DASHSCOPE_API_KEY:-}" ]]; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "Bailian API key is required. Set DASHSCOPE_API_KEY/BAILIAN_API_KEY or pass --bailian-api-key."
  fi
  local key
  prompt_secret key "Bailian/DashScope API key"
  [[ -n "$key" ]] || die "Bailian API key cannot be empty"
  export DASHSCOPE_API_KEY="$key"
}

resolve_dingtalk_enablement() {
  if [[ "$CA_ENABLE_DINGTALK" == "1" ]]; then
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    return
  fi
  local default="n"
  if [[ -n "${DINGTALK_BOT_CLIENT_ID:-}" && -n "${DINGTALK_BOT_CLIENT_SECRET:-}" ]]; then
    default="y"
  fi
  if confirm "Enable DingTalk channel for OpenClaw?" "$default"; then
    CA_ENABLE_DINGTALK=1
  fi
}

ensure_dingtalk_credentials() {
  resolve_dingtalk_enablement
  if [[ "$CA_ENABLE_DINGTALK" != "1" ]]; then
    return
  fi
  if [[ -z "${DINGTALK_BOT_CLIENT_ID:-}" ]]; then
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      die "DINGTALK_BOT_CLIENT_ID is required when --enable-dingtalk is set"
    fi
    local id
    prompt_value id "DingTalk bot client ID"
    export DINGTALK_BOT_CLIENT_ID="$id"
  fi
  if [[ -z "${DINGTALK_BOT_CLIENT_SECRET:-}" ]]; then
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      die "DINGTALK_BOT_CLIENT_SECRET is required when --enable-dingtalk is set"
    fi
    local secret
    prompt_secret secret "DingTalk bot client secret"
    export DINGTALK_BOT_CLIENT_SECRET="$secret"
  fi
}

detect_public_ip() {
  local url ip
  for url in \
    "https://ifconfig.me/ip" \
    "https://ipinfo.io/ip" \
    "https://checkip.amazonaws.com"; do
    ip="$(curl -fsSL --max-time 10 "$url" 2>/dev/null | tr -d '[:space:]' || true)"
    if [[ "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
      printf '%s\n' "$ip"
      return 0
    fi
  done
  return 1
}

validate_cidr() {
  local cidr="$1"
  [[ "$cidr" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/([0-9]|[12][0-9]|3[0-2])$ ]] || return 1
  local ip="${cidr%/*}"
  local octet
  IFS=. read -r o1 o2 o3 o4 <<<"$ip"
  for octet in "$o1" "$o2" "$o3" "$o4"; do
    [[ "$octet" -le 255 ]] || return 1
  done
}

normalize_cidr_list() {
  local raw="$1"
  local out_var="$2"
  local invalid_var="${3:-}"
  local cidr result=""
  raw="${raw//,/ }"
  raw="${raw//;/ }"
  for cidr in $raw; do
    if ! validate_cidr "$cidr"; then
      if [[ -n "$invalid_var" ]]; then
        printf -v "$invalid_var" '%s' "$cidr"
      fi
      return 1
    fi
    case " $result " in
      *" $cidr "*) ;;
      *) result="${result:+$result }$cidr" ;;
    esac
  done
  if [[ -z "$result" ]]; then
    if [[ -n "$invalid_var" ]]; then
      printf -v "$invalid_var" '%s' ""
    fi
    return 1
  fi
  printf -v "$out_var" '%s' "$result"
}

cidr_list_contains() {
  local list="$1"
  local needle="$2"
  local cidr
  for cidr in $list; do
    [[ "$cidr" == "$needle" ]] && return 0
  done
  return 1
}

warn_if_open_cidr() {
  local cidr="$1"
  if [[ "$cidr" == "0.0.0.0/0" ]]; then
    warn "0.0.0.0/0 allows any IPv4 source to reach the operator-facing security group ports."
    warn "Default OpenClaw config disables device auth and authenticates with a single gateway token; combined with 0.0.0.0/0 the token alone protects the control UI."
    warn "Only use this CIDR for temporary demos or controlled environments, and keep the gateway token secret."
  fi
}

set_allowed_cidrs() {
  local raw="$1"
  local source="$2"
  local normalized invalid cidr
  if ! normalize_cidr_list "$raw" normalized invalid; then
    if [[ -n "$invalid" ]]; then
      die "invalid $source CIDR: $invalid"
    fi
    die "$source CIDR list cannot be empty"
  fi
  CA_ALLOWED_CIDRS="$normalized"
  CA_ALLOWED_CIDR="$normalized"
  for cidr in $CA_ALLOWED_CIDRS; do
    warn_if_open_cidr "$cidr"
  done
}

resolve_deployer_cidr() {
  if [[ -n "${CA_DEPLOYER_CIDR:-}" ]]; then
    validate_cidr "$CA_DEPLOYER_CIDR" || die "invalid CA_DEPLOYER_CIDR: $CA_DEPLOYER_CIDR"
    if [[ -z "${CA_OPERATOR_EGRESS_IP:-}" && "$CA_DEPLOYER_CIDR" == */32 ]]; then
      export CA_OPERATOR_EGRESS_IP="${CA_DEPLOYER_CIDR%/32}"
    fi
    return
  fi

  if [[ -n "${CA_OPERATOR_EGRESS_IP:-}" ]]; then
    if [[ "$CA_OPERATOR_EGRESS_IP" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
      CA_DEPLOYER_CIDR="$CA_OPERATOR_EGRESS_IP/32"
      validate_cidr "$CA_DEPLOYER_CIDR" || die "invalid CA_OPERATOR_EGRESS_IP: $CA_OPERATOR_EGRESS_IP"
      return
    fi
    die "CA_OPERATOR_EGRESS_IP is not a valid IPv4 address: $CA_OPERATOR_EGRESS_IP"
  fi

  local ip
  if ip="$(detect_public_ip)"; then
    CA_DEPLOYER_CIDR="$ip/32"
    export CA_OPERATOR_EGRESS_IP="$ip"
    log "detected deployment host egress CIDR: $CA_DEPLOYER_CIDR"
    return
  fi

  warn "could not detect deployment host public egress IP; deploy may fail unless the operator CIDR covers this host"
}

prompt_allowed_cidr_list() {
  local cidrs normalized invalid
  while true; do
    prompt_value cidrs "Operator CIDR list, comma or space separated, for example 203.0.113.10/32 198.51.100.0/24"
    if normalize_cidr_list "$cidrs" normalized invalid; then
      if cidr_list_contains "$normalized" "0.0.0.0/0"; then
        warn_if_open_cidr "0.0.0.0/0"
        confirm "Use 0.0.0.0/0 anyway?" "n" || continue
      fi
      CA_ALLOWED_CIDRS="$normalized"
      CA_ALLOWED_CIDR="$normalized"
      return
    fi
    if [[ -n "$invalid" ]]; then
      warn "invalid CIDR: $invalid"
    else
      warn "CIDR list cannot be empty"
    fi
  done
}

choose_operator_cidr() {
  local detected="$1"
  local answer
  cat <<EOF

Operator CIDR controls who can reach deployment/status/debug/connect ports.
  1) Current machine only: $detected
  2) Custom CIDR list
  3) Allow all IPv4 sources: 0.0.0.0/0

EOF
  while true; do
    read -r -p "Choose operator CIDR [1]: " answer
    answer="${answer:-1}"
    case "$answer" in
      1|local|host|current)
        set_allowed_cidrs "$detected" "detected operator"
        return
        ;;
      2|custom|list|manual)
        prompt_allowed_cidr_list
        return
        ;;
      3|all|open|0.0.0.0|0.0.0.0/0)
        warn_if_open_cidr "0.0.0.0/0"
        if confirm "Use 0.0.0.0/0 anyway?" "n"; then
          CA_ALLOWED_CIDRS="0.0.0.0/0"
          CA_ALLOWED_CIDR="$CA_ALLOWED_CIDRS"
          return
        fi
        ;;
      *)
        warn "enter 1 for current machine only, 2 for a custom CIDR list, or 3 for 0.0.0.0/0"
        ;;
    esac
  done
}

resolve_allowed_cidr() {
  if [[ -n "${CA_ALLOWED_CIDR:-}" ]]; then
    set_allowed_cidrs "$CA_ALLOWED_CIDR" "--allowed-cidr"
    resolve_deployer_cidr
    return
  fi
  local ip detected
  if ip="$(detect_public_ip)"; then
    detected="$ip/32"
    CA_DEPLOYER_CIDR="${CA_DEPLOYER_CIDR:-$detected}"
    export CA_OPERATOR_EGRESS_IP="${CA_OPERATOR_EGRESS_IP:-$ip}"
    if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
      set_allowed_cidrs "$detected" "detected operator"
      return
    fi
    choose_operator_cidr "$detected"
    resolve_deployer_cidr
    return
  fi
  if [[ "$CA_NON_INTERACTIVE" == "1" ]]; then
    die "could not detect public egress IP; pass --allowed-cidr"
  fi
  prompt_allowed_cidr_list
  resolve_deployer_cidr
}
