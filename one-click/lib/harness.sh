#!/usr/bin/env bash

is_supported_harness() {
  case "$1" in
    openclaw|hermes|codex|claude-code|qodercli|opencode|pi|dsh) return 0 ;;
    *) return 1 ;;
  esac
}

default_api_base_url() {
  if [[ "${CA_HARNESS:-openclaw}" == claude-code ]]; then
    printf '%s\n' "${DASHSCOPE_ANTHROPIC_BASE_URL:-https://dashscope.aliyuncs.com/apps/anthropic}"
  else
    printf '%s\n' "${DASHSCOPE_BASE_URL:-https://dashscope.aliyuncs.com/compatible-mode/v1}"
  fi
}

harness_access_options() {
  case "$1" in
    openclaw) printf 'web+tui web tui ssh' ;;
    hermes) printf 'http ssh' ;;
    codex) printf 'ws ssh' ;;
    opencode) printf 'web+attach web attach ssh' ;;
    dsh) printf 'web ssh' ;;
    claude-code|qodercli|pi) printf 'ssh' ;;
    *) die "unsupported harness: $1" ;;
  esac
}

normalize_harness_access() {
  local choices access mode
  choices="$(harness_access_options "$CA_HARNESS")" || return
  access="${CA_ACCESS_METHOD:-${choices%% *}}"
  if [[ -z "$CA_ACCESS_METHOD" && "$CA_HARNESS_MODE" == cli ]]; then access=ssh; fi
  [[ " $choices " == *" $access "* ]] || die "unsupported access '$access' for $CA_HARNESS (choose $choices)"
  case "$access" in
    ssh) mode=cli ;;
    ws) mode=app-server ;;
    *) case "$CA_HARNESS" in openclaw|hermes) mode=gateway ;; *) mode=web ;; esac ;;
  esac
  [[ -z "$CA_HARNESS_MODE" || "$CA_HARNESS_MODE" == "$mode" ]] || die "$CA_HARNESS --access $access requires --harness-mode $mode"
  CA_ACCESS_METHOD="$access"
  CA_HARNESS_MODE="$mode"
}

resolve_harness_options() {
  if [[ "$CA_ONE_CLICK_TARGET" == openclaw && "$CA_MODE" == deploy-openclaw && "$CA_NON_INTERACTIVE" != 1 ]]; then
    if [[ -z "${CA_HARNESS:-}" ]]; then
      while true; do
        prompt_value CA_HARNESS "Harness (openclaw/hermes/codex/claude-code/qodercli/opencode/pi/dsh)" openclaw
        if is_supported_harness "$CA_HARNESS"; then break; fi
        warn "unsupported harness: $CA_HARNESS"
      done
    fi
    if [[ -z "${CA_ACCESS_METHOD:-}${CA_HARNESS_MODE:-}" ]]; then
      local choices
      choices="$(harness_access_options "$CA_HARNESS")"
      if [[ "$choices" == *" "* ]]; then
        prompt_value CA_ACCESS_METHOD "Access ($choices)" "${choices%% *}"
      fi
    fi
    if [[ "$CA_HARNESS" != qodercli ]]; then
      if [[ -z "${CA_MODEL:-}" ]]; then
        prompt_value CA_MODEL "Model" "$(ca_software_version bailian_model)"
      fi
      if [[ -z "${CA_API_BASE_URL:-}" ]]; then
        prompt_value CA_API_BASE_URL "Inference API base URL" "$(default_api_base_url)"
      fi
    fi
  fi

  CA_HARNESS="${CA_HARNESS:-openclaw}"
  CA_MODEL="${CA_MODEL:-$(ca_software_version bailian_model)}"
  CA_API_BASE_URL="${CA_API_BASE_URL:-$(default_api_base_url)}"
  if [[ "$CA_MODE" == deploy-openclaw ]] && is_supported_harness "$CA_HARNESS"; then
    normalize_harness_access
  fi
}

harness_guest_port() {
  case "${CA_HARNESS:-openclaw}:${CA_HARNESS_MODE:-gateway}" in
    openclaw:gateway) printf '18789\n' ;;
    hermes:gateway) printf '8642\n' ;;
    codex:app-server) printf '4500\n' ;;
    opencode:web) printf '4096\n' ;;
    dsh:web) printf '3080\n' ;;
    *:cli) ;;
    *) return 1 ;;
  esac
}

selected_host_client() {
  if [[ "${CA_SKIP_HOST_CLIENT:-0}" == "1" || "${CA_HARNESS_MODE:-cli}" == "cli" ]]; then
    return 0
  fi
  case "${CA_HARNESS:-openclaw}:${CA_HARNESS_MODE:-gateway}:${CA_ACCESS_METHOD:-web+tui}" in
    openclaw:gateway:web+tui|openclaw:gateway:tui)
      if [[ "${CA_SKIP_HOST_OPENCLAW:-0}" != "1" ]]; then
        printf 'openclaw\n'
      fi
      ;;
    codex:app-server:*)
      printf 'codex\n'
      ;;
    opencode:web:web+attach|opencode:web:attach)
      printf 'opencode\n'
      ;;
  esac
}

resolve_cpu_image() {
  local artifact="$CA_ARTIFACT"
  if [[ "$CA_IMAGE_SOURCE" == build ]]; then
    local builder="$CA_WORK_DIR/allinone-builder"
    CA_IMAGE_RELEASE="$builder/image-release.json"
    if [[ "$CA_SKIP_BUILD" != 1 ]]; then
      local args=(--reference-values "$CA_REFERENCE_VALUES" --slsa-generator "$CA_SLSA_GENERATOR")
      if [[ -n "${CA_COSIGN_KEY:-}" ]]; then args+=(--cosign-key "$CA_COSIGN_KEY"); fi
      (cd "$ROOT_DIR" && ca_cmd init --stage build --non-interactive --output-dir "$CA_WORK_DIR" --force "${args[@]}")
      (cd "$builder" && ca_cmd build --spec "$builder/build.yaml")
    fi
    mkdir -p "$builder"
    ca_cmd image export allinone-builder --output "$CA_IMAGE_RELEASE"
    CA_RELEASE_DIGEST="sha256:$(sha256sum "$CA_IMAGE_RELEASE" | cut -d' ' -f1)"
    if [[ "$CA_SKIP_DEPLOY" != 1 ]]; then
      ca_cmd image publish allinone-builder --region "$CA_REGION"
    fi
    return
  fi

  if [[ -z "$CA_IMAGE_RELEASE" ]]; then
    local version metadata reference
    version="${CA_RELEASE_VERSION:-$("$CA_BIN" --version | awk '{print $2}')}"
    local args=(--output-dir "${CA_RELEASE_IMAGE_CACHE:-$CA_STATE_DIR/cache/release-image}/v${version#v}" --version "${version#v}")
    if [[ -n "${CA_OFFICIAL_METADATA_DIR:-}" ]]; then args+=(--from-dir "$CA_OFFICIAL_METADATA_DIR"); fi
    metadata="$("$ROOT_DIR/hack/resolve-release-image.sh" "${args[@]}")"
    CA_IMAGE_RELEASE="$(jq -er .image_release <<<"$metadata")"
    reference="$(jq -er .oci_reference <<<"$metadata")"
    CA_RELEASE_DIGEST="${CA_RELEASE_DIGEST:-$(jq -er .release_digest <<<"$metadata")}"
    artifact="${artifact:-${CA_GHCR_MIRROR:-ghcr.1ms.run}/${reference#ghcr.io/}}"
  fi
  if [[ "$CA_SKIP_DEPLOY" != 1 && -z "$CA_IMAGE_ID" && -n "$artifact" ]]; then
    ca_cmd image publish --image-release "$CA_IMAGE_RELEASE" --release-digest "$CA_RELEASE_DIGEST" \
      --artifact "$artifact" --region "$CA_REGION"
  fi
}

prepare_cpu_specs() {
  case "$CA_HARNESS:$CA_HARNESS_MODE" in
    openclaw:*|hermes:gateway|opencode:web) ensure_gateway_token "cpu-$CA_SERVICE_ID" ;;
    codex:app-server) ensure_gateway_token "cpu-$CA_SERVICE_ID" base64 ;;
  esac
  export CA_BIN CA_AGENTD_BIN CA_GATEWAY_BIN CA_PEP_BIN
  resolve_cpu_image

  local args=(
    --stage deploy
    --non-interactive
    --force
    --output-dir "$CA_WORK_DIR"
    --service-id "$CA_SERVICE_ID"
    --image-release "$CA_IMAGE_RELEASE"
    --release-digest "$CA_RELEASE_DIGEST"
    --harness-mode "$CA_HARNESS_MODE"
    --region "$CA_REGION"
    --zone-id "$CA_ZONE_ID"
    --instance-type "$CA_INSTANCE_TYPE"
    --disk-gb "$CA_DISK_GB"
    --replicas "$CA_REPLICAS"
    --reference-values "$CA_REFERENCE_VALUES"
  )
  if [[ -n "${CA_IMAGE_ID:-}" ]]; then
    args+=(--image-id "$CA_IMAGE_ID")
  fi

  if [[ "$CA_HARNESS" == openclaw ]]; then
    args+=(
      --gateway-token "$CA_GATEWAY_TOKEN"
      --openclaw-version "$CA_OPENCLAW_VERSION"
      --node-version "$CA_NODE_VERSION"
      --npm-registry "$CA_NPM_REGISTRY"
    )
    if [[ "${CA_DISABLE_PEP:-0}" == "1" ]]; then
      args+=(--disable-pep)
    fi
    if [[ "${CA_ENABLE_DINGTALK:-0}" == "1" ]]; then
      args+=(
        --enable-dingtalk
        --dingtalk-client-id "$DINGTALK_BOT_CLIENT_ID"
        --dingtalk-client-secret "$DINGTALK_BOT_CLIENT_SECRET"
      )
    fi
  elif [[ -n "${CA_GATEWAY_TOKEN:-}" ]]; then
    args+=(--gateway-token "$CA_GATEWAY_TOKEN")
  fi

  if [[ "$CA_HARNESS" != qodercli ]]; then
    args+=(
      --dashscope-api-key "$DASHSCOPE_API_KEY"
      --model "$CA_MODEL"
      --dashscope-base-url "$CA_API_BASE_URL"
      --dashscope-anthropic-base-url "$CA_API_BASE_URL"
    )
  fi

  run_confidential_agent_init "$CA_HARNESS" "${args[@]}"
  log "generated $CA_SERVICE_LABEL spec at $CA_SPEC_PATH"
}

print_cpu_summary() {
  local host="${CA_CONNECT_HOST:-127.0.0.1}"
  local port="${CA_CONNECT_PORT:-}"
  local connect_pid="${CA_CONNECT_PID:-}"
  local log_path="$CA_WORK_DIR/${CA_CONNECT_NAME:-connect}.log"

  cat <<EOF

Deployment summary
  state_dir:    $CA_STATE_DIR
  work_dir:     $CA_WORK_DIR
  spec:         $CA_SPEC_PATH
  service:      $CA_SERVICE_ID
  harness:      $CA_HARNESS ($CA_HARNESS_MODE, access: $CA_ACCESS_METHOD)
  image_source: $CA_IMAGE_SOURCE
  release:      $CA_IMAGE_RELEASE ($CA_RELEASE_DIGEST)
  region:       $CA_REGION
  zone_id:      $CA_ZONE_ID
  instance:     $CA_INSTANCE_TYPE
  cidrs:        ${CA_ALLOWED_CIDRS:-${CA_ALLOWED_CIDR:-not set}}
  deployer:     ${CA_DEPLOYER_CIDR:-not detected}
EOF
  if [[ -n "${CA_IMAGE_ID:-}" ]]; then
    printf '  image_id:     %s\n' "$CA_IMAGE_ID"
  fi
  if [[ "$CA_HARNESS_MODE" == cli ]]; then
    if [[ "$CA_HARNESS" == dsh ]]; then
      printf '  SSH: confidential-agent --state-dir "%s" ssh "%s" -- cai-agent headless "Your task"\n' "$CA_STATE_DIR" "$CA_SERVICE_ID"
    else
      printf '  SSH: confidential-agent --state-dir "%s" ssh "%s" --tty -- cai-agent\n' "$CA_STATE_DIR" "$CA_SERVICE_ID"
    fi
    if [[ "$CA_HARNESS" == qodercli ]]; then log "Sign in to your Qoder account in the interactive session."; fi
  elif [[ -n "$port" ]]; then
    case "$CA_HARNESS" in
      openclaw)
        if [[ "$CA_ACCESS_METHOD" != tui ]]; then
          printf '  Web: http://%s:%s/openclaw/#token=%s\n' "$host" "$port" "$(jq -nr --arg token "$CA_GATEWAY_TOKEN" '$token|@uri')"
        fi
        if [[ "$CA_ACCESS_METHOD" != web ]]; then
          printf '  TUI: openclaw tui --url ws://%s:%s --token "%s"\n' "$host" "$port" "$CA_GATEWAY_TOKEN"
        fi
        cat <<EOF
  API: curl -fsS http://$host:$port/v1/responses -H 'Authorization: Bearer $CA_GATEWAY_TOKEN' \\
         -H 'Content-Type: application/json' -d '{"model":"openclaw/default","input":"hi"}'
EOF
        ;;
      hermes)
        local hermes_key
        hermes_key="$(jq -er .environment.API_SERVER_KEY "$CA_PROJECT_DIR/secrets/runtime.json")"
        cat <<EOF
  http:         http://$host:$port
  curl:         curl -fsS http://$host:$port/v1/chat/completions \\
                  -H 'Authorization: Bearer $hermes_key' \\
                  -H 'Content-Type: application/json' \\
                  -d '{"model":"$CA_MODEL","messages":[{"role":"user","content":"hi"}]}'
EOF
        ;;
      codex)
        local codex_token_file
        codex_token_file="$CA_PROJECT_DIR/secrets/app-server-token"
        cat <<EOF
  ws:           ws://$host:$port
  codex:        CODEX_REMOTE_TOKEN="\$(cat "$codex_token_file")" codex --remote ws://$host:$port --remote-auth-token-env CODEX_REMOTE_TOKEN
EOF
        ;;
      opencode)
        local opencode_pw=""
        opencode_pw="$(jq -er .environment.OPENCODE_SERVER_PASSWORD "$CA_PROJECT_DIR/secrets/runtime.json")"
        if [[ "$CA_ACCESS_METHOD" != attach ]]; then
          printf '  Web: http://%s:%s (user: opencode, password: %s)\n' "$host" "$port" "$opencode_pw"
        fi
        if [[ "$CA_ACCESS_METHOD" != web ]]; then
          printf '  Attach: OPENCODE_SERVER_USERNAME=opencode OPENCODE_SERVER_PASSWORD="%s" opencode attach http://%s:%s\n' "$opencode_pw" "$host" "$port"
        fi
        ;;
      dsh)
        cat <<EOF
  web:          $CA_DSH_WEB_URL
EOF
        ;;
    esac
    cat <<EOF
  ready_json:   $CA_READY_JSON
  connect:      running (pid ${connect_pid:-unknown}, log $log_path)
EOF
  else
    cat <<EOF
  connect:      run when the service is active:
                confidential-agent --tools-dir "$CA_TOOLS_DIR" --state-dir "$CA_STATE_DIR" connect start --service "$CA_SERVICE_ID" --ready-json "$CA_READY_JSON"
EOF
  fi

  cat <<EOF

Cleanup (destroys the instance; shared cloud image is retained):
  one-click/install.sh cleanup --harness "$CA_HARNESS" --service-id "$CA_SERVICE_ID" --state-dir "$CA_STATE_DIR" --work-dir "$CA_WORK_DIR"
EOF
  if [[ -n "$port" ]]; then
    cat <<EOF
  confidential-agent --tools-dir "$CA_TOOLS_DIR" --state-dir "$CA_STATE_DIR" connect stop --ready-json "$CA_READY_JSON"  # stop local tunnel only

EOF
  else
    printf '\n'
  fi
}

run_deploy_cpu() {
  install_os_dependencies
  ensure_host_access_client
  if [[ "$CA_SKIP_DEPLOY" != 1 ]]; then
    ensure_aliyun_credentials
    ensure_aliyun_cli_region
    resolve_allowed_cidr
  fi
  if [[ "$CA_HARNESS" != qodercli ]]; then
    ensure_bailian_credentials
  fi
  if [[ "$CA_HARNESS" == openclaw ]]; then
    ensure_dingtalk_credentials
  fi
  ensure_shelter
  build_confidential_agent
  install_confidential_agent_cli
  ensure_native_tools
  prepare_cpu_specs
  if [[ "$CA_SKIP_DEPLOY" == 1 ]]; then
    log "generated $CA_SPEC_PATH (--skip-deploy)"
    return
  fi
  ensure_operator_peering
  deploy_openclaw_service
  sync_active_openclaw_resources
  if [[ "${CA_ACTIVE_RESOURCES_SYNCED:-0}" == 1 ]]; then
    ca_cmd ssh "$CA_SERVICE_ID" -- systemctl restart cai-allinone.service
  fi
  wait_for_live_status
  if [[ -n "$(harness_guest_port)" ]]; then
    start_connect
  fi
  print_cpu_summary
}
