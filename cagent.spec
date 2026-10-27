%global debug_package %{nil}
%global _build_id_links none
%global cagent_target %{_topdir}/cargo-target
# Application templates execute inside guests; preserve their interpreters and
# do not add their Node/Python dependencies to the host RPM.
%global __requires_exclude_from ^%{_datadir}/cagent/.*$
%global __brp_mangle_shebangs %{nil}
%global __brp_python_bytecompile %{nil}

Name:           cagent
Version:        0.1.0
Release:        2%{?dist}
Summary:        Confidential Agent host CLI and guest image assets
License:        Apache-2.0
URL:            https://github.com/inclavare-containers/confidential-agent
Source0:        cagent-%{version}.tar.gz
Source1:        cagent-vendor.tar.gz
Source2:        cagent-native-tools.tar.gz
ExclusiveArch:  x86_64

BuildRequires:  cargo
BuildRequires:  rust >= 1.75
BuildRequires:  gcc
BuildRequires:  glibc-devel
BuildRequires:  python3.11
BuildRequires:  /usr/bin/pkg-config

Requires:       shelter >= 0.3.2-2
Requires:       attestation-challenge-client
Requires:       libtdx-verify
Requires:       trusted-network-gateway = 2.9.2-1.al8
Requires:       ca-certificates
Requires:       coreutils
Requires:       openssh-clients
Requires:       openssl
Requires:       bash
Requires:       jq

%description
Build, publish and deploy confidential AI agents with native host tools.
Includes the CA CLI, guest daemon/gateway/PEP assets, pinned cloud/signing tools,
and application examples. Attestation and TNG runtimes are installed through
RPM dependencies; rekor-cli is shared with Shelter. Docker is not a host
dependency. Applications may still use container isolation inside their guest.

%prep
%setup -q
tar -xzf %{SOURCE1}
tar -xzf %{SOURCE2}
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
EOF

%build
export CARGO_TARGET_DIR=%{cagent_target}
export CARGO_BUILD_JOBS=%{?_smp_build_ncpus}%{!?_smp_build_ncpus:4}
cargo build --release --locked --offline -p confidential-agent-cli -p confidential-agentd -p cai-gateway -p cai-pep

%install
for binary in confidential-agent confidential-agentd cai-gateway cai-pep; do
    install -D -m 0755 %{cagent_target}/release/$binary %{buildroot}%{_bindir}/$binary
done
ln -s confidential-agent %{buildroot}%{_bindir}/cagent
install -d %{buildroot}%{_libexecdir}/cagent/bin
for tool in cosign aliyun ossutil64; do
    install -m 0755 native-tools/$tool %{buildroot}%{_libexecdir}/cagent/bin/$tool
done
for tool in tng attestation-challenge-client; do
    ln -s %{_bindir}/$tool %{buildroot}%{_libexecdir}/cagent/bin/$tool
done
ln -s %{_libexecdir}/shelter/bin/rekor-cli %{buildroot}%{_libexecdir}/cagent/bin/rekor-cli
install -m 0644 native-tools/SOURCES.tsv %{buildroot}%{_libexecdir}/cagent/SOURCES.tsv
install -d %{buildroot}%{_datadir}/cagent
cp -a examples common software-versions.json %{buildroot}%{_datadir}/cagent/
install -m 0644 hack/shelter-rpm.json %{buildroot}%{_datadir}/cagent/shelter-rpm.json

%check
export CARGO_TARGET_DIR=%{cagent_target}
cargo test --release --locked --offline -p confidential-agent-cli --quiet
bash one-click/tests/shelter-cleanup.sh
bash one-click/tests/shelter-upgrade.sh
bash one-click/tests/paired-rpm-upgrade.sh
%{cagent_target}/release/confidential-agent --version
native-tools/cosign version >/dev/null
native-tools/aliyun version >/dev/null
native-tools/ossutil64 --version >/dev/null

%files
%license LICENSE
%doc README.md docs/host-dependencies.md docs/rpm.md
%{_bindir}/cagent
%{_bindir}/confidential-agent
%{_bindir}/confidential-agentd
%{_bindir}/cai-gateway
%{_bindir}/cai-pep
%{_libexecdir}/cagent/
%{_datadir}/cagent/

%changelog
* Tue Oct 27 2026 Weidong Sun <sunweidong@linux.alibaba.com> - 0.1.0-2
- Require Shelter 0.3.2-2 so package upgrades select the current runtime contract.
- Check cleanup capabilities without requiring image-build tool versions.

* Mon Oct 19 2026 Weidong Sun <sunweidong@linux.alibaba.com> - 0.1.0-1
- Package native host tools and guest assets without a Docker dependency.
- Share the attestation runtime and Rekor tool with host packages and Shelter.
