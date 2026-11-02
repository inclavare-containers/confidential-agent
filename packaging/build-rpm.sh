#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
top="${1:-$root/target/rpm}"
mkdir -p "$top"
top="$(cd "$top" && pwd)"
"$root/packaging/prepare-rpm-sources.sh" "$top/SOURCES"
# Dependencies are declared in the spec; do not silently modify the build host.
rpmbuild -ba --define "_topdir $top" --define "_smp_build_ncpus ${CAGENT_BUILD_JOBS:-4}" --define "_smp_mflags -j${CAGENT_BUILD_JOBS:-4}" "$root/cagent.spec"
shelter_rpm="$(python3.11 -c 'import json, sys; print(json.load(open(sys.argv[1]))["file"])' "$root/hack/shelter-rpm.json")"
shelter_path="$("$root/hack/resolve-shelter-rpm.sh")"
release_dir="$top/RPMS/x86_64"
cp "$shelter_path" "$release_dir/$shelter_rpm.tmp"
mv "$release_dir/$shelter_rpm.tmp" "$release_dir/$shelter_rpm"
cp "$root/hack/shelter-rpm.json" "$release_dir/shelter-rpm.json"
(
    cd "$release_dir"
    for package in cagent-*.rpm "$shelter_rpm"; do
        sha256sum "$package" > "$package.sha256"
    done
    sha256sum cagent-*.rpm "$shelter_rpm" > SHA256SUMS
)
find "$top/RPMS" -name 'cagent-*.rpm' -type f -exec sha256sum {} +
