#!/usr/bin/env bash
# The base image lists ECS-only mirrors first. CI uses public HTTPS mirrors.
set -euo pipefail
sed -i \
  -e 's|http://mirrors.cloud.aliyuncs.com|https://mirrors.aliyun.com|g' \
  -e 's|http://mirrors.aliyun.com|https://mirrors.aliyun.com|g' \
  /etc/yum.repos.d/*.repo
