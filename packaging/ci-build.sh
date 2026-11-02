#!/usr/bin/env bash
# GitHub-hosted Linux runner: compile and test in the target distribution.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["container"])' "$root/packaging/release-build.json")"
docker run --rm \
  -e CAGENT_BUILD_JOBS="${CAGENT_BUILD_JOBS:-4}" \
  -e CA_GITHUB_PROXY_FIRST=0 \
  -e CARGO_HTTP_MULTIPLEXING=false \
  -v "$root/target/ci-dnf-cache:/var/cache/dnf" \
  -v "$root:/src" -w /src "$image" bash -euo pipefail -c '
    bash packaging/prepare-ci-host.sh
    dnf --setopt=keepcache=True install -y rpm-build cargo rust gcc glibc-devel pkgconf-pkg-config python3.11 curl tar git
    git config --global --add safe.directory /src
    packaging/build-rpm.sh /src/target/release-rpm
    rpm -qa | sort > /src/target/release-rpm/build-environment.txt
    python3.11 packaging/collect-release.py /src/target/release-rpm /src/target/release-dist
  '
