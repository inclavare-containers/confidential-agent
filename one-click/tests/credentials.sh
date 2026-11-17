#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

# shellcheck source=one-click/lib/common.sh
. "$ROOT_DIR/one-click/lib/common.sh"
# shellcheck source=one-click/lib/credentials.sh
. "$ROOT_DIR/one-click/lib/credentials.sh"

fail() {
  printf 'not ok - %s\n' "$*" >&2
  exit 1
}

assert_eq() {
  local expected="$1"
  local actual="$2"
  local message="$3"
  if [[ "$actual" != "$expected" ]]; then
    printf 'not ok - %s\nexpected:\n%s\nactual:\n%s\n' "$message" "$expected" "$actual" >&2
    exit 1
  fi
}

aliyun() {
  if [[ "${1:-} ${2:-}" == "sts GetCallerIdentity" ]]; then
    return 0
  fi
  if [[ "${1:-} ${2:-} ${3:-}" == "configure get profile" ]]; then
    printf 'profile=cli-current\n'
    return 0
  fi
  printf 'unexpected fake aliyun invocation: %s\n' "$*" >&2
  return 2
}

test_current_cli_profile_is_exported_for_terraform() (
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT
  mkdir -p "$tmp_dir/.aliyun"
  printf '%s\n' \
    '{"current":"cli-current","profiles":[{"name":"cli-current","mode":"AK","access_key_id":"profile-ak","access_key_secret":"profile-sk"}]}' \
    >"$tmp_dir/.aliyun/config.json"

  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  HOME="$tmp_dir"
  CA_NON_INTERACTIVE=1

  ensure_aliyun_credentials

  assert_eq "cli-current" "$ALIBABA_CLOUD_PROFILE" \
    "the current Aliyun CLI profile should be exported for the Terraform provider"
  assert_eq "cli-current" "$ALICLOUD_PROFILE" \
    "the legacy Terraform profile alias should also be exported"
  assert_eq "$tmp_dir/.aliyun/config.json" "$ALIBABA_CLOUD_CREDENTIALS_FILE" \
    "the Aliyun CLI credentials file should be exported for the Terraform provider"
  assert_eq "$tmp_dir/.aliyun/config.json" "$ALICLOUD_SHARED_CREDENTIALS_FILE" \
    "the legacy credentials-file alias should also be exported"
  assert_eq "profile-ak" "$ALICLOUD_ACCESS_KEY" \
    "the CLI profile access key should be available to containerized tools"
  assert_eq "profile-sk" "$ALICLOUD_SECRET_KEY" \
    "the CLI profile secret key should be available to containerized tools"
)

test_explicit_profile_is_preserved_and_normalized() (
  local tmp_dir
  tmp_dir="$(mktemp -d)"
  trap 'rm -rf "$tmp_dir"' EXIT
  printf '%s\n' \
    '{"current":"unused","profiles":[{"name":"explicit-profile","mode":"AK","access_key_id":"explicit-ak","access_key_secret":"explicit-sk"}]}' \
    >"$tmp_dir/config.json"

  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE
  ALIBABA_CLOUD_PROFILE="explicit-profile"
  ALIBABA_CLOUD_CREDENTIALS_FILE="$tmp_dir/config.json"
  unset ALICLOUD_SHARED_CREDENTIALS_FILE
  CA_NON_INTERACTIVE=1

  ensure_aliyun_credentials

  assert_eq "explicit-profile" "$ALIBABA_CLOUD_PROFILE" \
    "an explicit canonical profile should be preserved"
  assert_eq "explicit-profile" "$ALICLOUD_PROFILE" \
    "an explicit canonical profile should be copied to the legacy alias"
  assert_eq "explicit-ak" "$ALIBABA_CLOUD_ACCESS_KEY_ID" \
    "an explicit CLI AK profile should be materialized for downstream tools"
  assert_eq "explicit-sk" "$ALIBABA_CLOUD_ACCESS_KEY_SECRET" \
    "an explicit CLI AK profile secret should be materialized for downstream tools"
)

test_non_ak_cli_profile_is_not_accepted_for_the_full_flow() (
  local tmp_dir error_file
  tmp_dir="$(mktemp -d)"
  error_file="$tmp_dir/error"
  trap 'rm -rf "$tmp_dir"' EXIT
  mkdir -p "$tmp_dir/.aliyun"
  printf '%s\n' \
    '{"current":"cli-current","profiles":[{"name":"cli-current","mode":"EcsRamRole","ram_role_name":"example"}]}' \
    >"$tmp_dir/.aliyun/config.json"

  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  HOME="$tmp_dir"
  CA_NON_INTERACTIVE=1

  if (ensure_aliyun_credentials) 2>"$error_file"; then
    fail "a non-AK CLI profile should not be accepted when downstream tools need AK/SK"
  fi
  if [[ "$(<"$error_file")" != *"not a readable AK profile"* ]]; then
    fail "the unsupported CLI profile error should explain why it cannot drive the full flow"
  fi
)

test_legacy_env_credentials_are_normalized() (
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  ALICLOUD_ACCESS_KEY="legacy-ak"
  ALICLOUD_SECRET_KEY="legacy-sk"
  CA_NON_INTERACTIVE=1

  ensure_aliyun_credentials

  assert_eq "legacy-ak" "$ALIBABA_CLOUD_ACCESS_KEY_ID" \
    "the legacy access key should be copied to the canonical alias"
  assert_eq "legacy-sk" "$ALIBABA_CLOUD_ACCESS_KEY_SECRET" \
    "the legacy secret key should be copied to the canonical alias"
)

test_canonical_env_credentials_are_normalized() (
  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  ALIBABA_CLOUD_ACCESS_KEY_ID="canonical-ak"
  ALIBABA_CLOUD_ACCESS_KEY_SECRET="canonical-sk"
  CA_NON_INTERACTIVE=1

  ensure_aliyun_credentials

  assert_eq "canonical-ak" "$ALICLOUD_ACCESS_KEY" \
    "the canonical access key should be copied to the legacy alias"
  assert_eq "canonical-sk" "$ALICLOUD_SECRET_KEY" \
    "the canonical secret key should be copied to the legacy alias"
)

test_interactive_credentials_are_exported_with_both_names() (
  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  CA_NON_INTERACTIVE=0

  configure_terraform_from_aliyun_cli() {
    return 1
  }

  ensure_aliyun_credentials <<< $'interactive-ak\ninteractive-sk' 2>/dev/null

  assert_eq "interactive-ak" "$ALICLOUD_ACCESS_KEY" \
    "interactive input should set the legacy access key"
  assert_eq "interactive-sk" "$ALICLOUD_SECRET_KEY" \
    "interactive input should set the legacy secret key"
  assert_eq "interactive-ak" "$ALIBABA_CLOUD_ACCESS_KEY_ID" \
    "interactive input should set the canonical access key"
  assert_eq "interactive-sk" "$ALIBABA_CLOUD_ACCESS_KEY_SECRET" \
    "interactive input should set the canonical secret key"
)

test_non_interactive_mode_rejects_missing_credentials() (
  local error_file
  error_file="$(mktemp)"
  trap 'rm -f "$error_file"' EXIT

  unset ALICLOUD_ACCESS_KEY ALICLOUD_SECRET_KEY
  unset ALIBABA_CLOUD_ACCESS_KEY_ID ALIBABA_CLOUD_ACCESS_KEY_SECRET
  unset ALICLOUD_PROFILE ALIBABA_CLOUD_PROFILE
  unset ALICLOUD_SHARED_CREDENTIALS_FILE ALIBABA_CLOUD_CREDENTIALS_FILE
  CA_NON_INTERACTIVE=1

  configure_terraform_from_aliyun_cli() {
    return 1
  }

  if (ensure_aliyun_credentials) 2>"$error_file"; then
    fail "non-interactive mode should reject missing credentials"
  fi
  if [[ "$(<"$error_file")" != *"Aliyun credentials are required"* ]]; then
    fail "the missing-credentials error should explain the accepted inputs"
  fi
)

test_current_cli_profile_is_exported_for_terraform || fail "current CLI profile bridge failed"
test_explicit_profile_is_preserved_and_normalized || fail "explicit profile normalization failed"
test_non_ak_cli_profile_is_not_accepted_for_the_full_flow || fail "unsupported CLI profile was accepted"
test_legacy_env_credentials_are_normalized || fail "legacy environment normalization failed"
test_canonical_env_credentials_are_normalized || fail "canonical environment normalization failed"
test_interactive_credentials_are_exported_with_both_names || fail "interactive credential prompt failed"
test_non_interactive_mode_rejects_missing_credentials || fail "missing non-interactive credentials were accepted"
printf 'ok - one-click credential tests passed\n'
