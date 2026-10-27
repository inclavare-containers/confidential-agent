#!/usr/bin/env bash
# Use RPM's real dependency solver with an isolated database and tiny payloads.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
for tool in rpm rpmbuild; do
  command -v "$tool" >/dev/null || { echo "SKIP paired RPM upgrade: $tool unavailable"; exit 0; }
done
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/db" "$work/packages"
rpm --dbpath "$work/db" --initdb

ca_version="$(awk '$1 == "Version:" { print $2 }' "$repo/cagent.spec")"
ca_release="$(awk '$1 == "Release:" { sub(/%.*/, "", $2); print $2 }' "$repo/cagent.spec")"
required="$(awk '$1 == "Requires:" && $2 == "shelter" { print $2, $3, $4 }' "$repo/cagent.spec")"
[[ "$required" == 'shelter >= 0.3.2-2' && "$ca_release" -gt 1 ]]

make_package() {
  local name="$1" version="$2" release="$3" requirement="$4"
  cat > "$work/package.spec" <<SPEC
Name: $name
Version: $version
Release: $release
Summary: Paired upgrade dependency fixture
License: MIT
BuildArch: noarch
$requirement
%description
Private upgrade fixture.
%install
mkdir -p %{buildroot}$work/payload
printf '%s\n' '$version-$release' > %{buildroot}$work/payload/$name
%files
$work/payload/$name
SPEC
  rpmbuild -bb --quiet --define "_topdir $work/build" "$work/package.spec"
  mv "$work/build/RPMS/noarch/$name-$version-$release.noarch.rpm" "$work/packages/"
}

make_package shelter 0.3.2 1 ''
make_package shelter 0.3.2 2 ''
make_package cagent "$ca_version" 1 'Requires: shelter >= 0.3.2-1'
make_package cagent "$ca_version" "$ca_release" "Requires: $required"
old_shelter="$work/packages/shelter-0.3.2-1.noarch.rpm"
new_shelter="$work/packages/shelter-0.3.2-2.noarch.rpm"
old_ca="$work/packages/cagent-$ca_version-1.noarch.rpm"
new_ca="$work/packages/cagent-$ca_version-$ca_release.noarch.rpm"
rpm --dbpath "$work/db" -U --noscripts "$old_shelter" "$old_ca"
if rpm --dbpath "$work/db" -U --test --noscripts "$new_ca" > "$work/rejected" 2>&1; then
  echo 'new cagent accepted the old Shelter dependency' >&2
  exit 1
fi
grep -Fq "$required" "$work/rejected"
rpm --dbpath "$work/db" -U --test --noscripts "$new_shelter" "$new_ca"
rpm --dbpath "$work/db" -U --noscripts "$new_shelter" "$new_ca"
[[ "$(rpm --dbpath "$work/db" -q --qf '%{VERSION}-%{RELEASE}' shelter)" == 0.3.2-2 ]]
[[ "$(rpm --dbpath "$work/db" -q --qf '%{VERSION}-%{RELEASE}' cagent)" == "$ca_version-$ca_release" ]]
printf '%s\n' 'Paired RPM upgrade rejects old Shelter and upgrades both releases: PASS'
