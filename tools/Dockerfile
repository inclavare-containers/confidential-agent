ARG BASE_IMAGE=alibaba-cloud-linux-3-registry.cn-hangzhou.cr.aliyuncs.com/alinux3/alinux3:latest

FROM ${BASE_IMAGE} AS tools

ARG INSTALL_PACKAGES="binutils ca-certificates curl rpm tar"
ARG TNG_RPM_VERSION=2.9.2-1.al8
ARG COSIGN_VERSION=3.0.6
ARG REKOR_VERSION=1.5.1
ARG ALIYUN_CLI_VERSION=3.0.232
ARG OSSUTIL_VERSION=1.7.18
ARG COSIGN_SHA256=c956e5dfcac53d52bcf058360d579472f0c1d2d9b69f55209e256fe7783f4c74
ARG REKOR_CLI_SHA256=0b4964af85477892c37039fb80793b151864970d19838873eaa1a777ca2fb813
ARG ALIYUN_CLI_SHA256=f6dca50b17e745881c5bb5d2e02ea7382bf1cf2a6d3ec76488b071a04a06c0e4
ARG OSSUTIL_SHA256=c0d1c40cb406ae2295a4d1e6a8a0aef8b4319d609c6b8acd742348c88734b697
ARG GITHUB_PROXY_URL=https://gh-proxy.org/
ARG GITHUB_PROXY_FIRST=0
ARG GITHUB_DOWNLOAD_CONNECT_TIMEOUT=20
ARG GITHUB_DOWNLOAD_MAX_TIME=120
ARG GITHUB_DOWNLOAD_SPEED_LIMIT=65536
ARG GITHUB_DOWNLOAD_SPEED_TIME=20
# URLs may be overridden for local mirrors; the fixed SHA256 values still gate content.
ARG COSIGN_URL=https://github.com/sigstore/cosign/releases/download/v${COSIGN_VERSION}/cosign-linux-amd64
ARG REKOR_CLI_URL=https://github.com/sigstore/rekor/releases/download/v${REKOR_VERSION}/rekor-cli-linux-amd64
ARG ALIYUN_CLI_URL=https://aliyuncli.alicdn.com/aliyun-cli-linux-${ALIYUN_CLI_VERSION}-amd64.tgz
ARG OSSUTIL_URL=https://gosspublic.alicdn.com/ossutil/${OSSUTIL_VERSION}/ossutil64

RUN yum install -y \
        --setopt=install_weak_deps=False \
        ${INSTALL_PACKAGES} && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum

RUN download() { \
        dest="$1"; \
        url="$2"; \
        proxy_url=""; \
        case "$url" in \
            https://github.com/*) \
                if [ -n "$GITHUB_PROXY_URL" ]; then \
                    case "$GITHUB_PROXY_URL" in \
                        */) proxy_url="${GITHUB_PROXY_URL}${url}" ;; \
                        *) proxy_url="${GITHUB_PROXY_URL}/${url}" ;; \
                    esac; \
                fi; \
                ;; \
        esac; \
        for attempt in 1 2 3 4 5; do \
            if [ "$GITHUB_PROXY_FIRST" = "1" ]; then \
                candidates="$proxy_url $url"; \
            else \
                candidates="$url $proxy_url"; \
            fi; \
            for candidate in $candidates; do \
                [ -n "$candidate" ] || continue; \
                if curl -fsSL \
                    --connect-timeout "$GITHUB_DOWNLOAD_CONNECT_TIMEOUT" \
                    --max-time "$GITHUB_DOWNLOAD_MAX_TIME" \
                    --speed-limit "$GITHUB_DOWNLOAD_SPEED_LIMIT" \
                    --speed-time "$GITHUB_DOWNLOAD_SPEED_TIME" \
                    -o "${dest}.tmp" "$candidate"; then \
                    mv "${dest}.tmp" "$dest"; \
                    return 0; \
                fi; \
                rm -f "${dest}.tmp"; \
                echo "download failed for ${candidate} (attempt ${attempt}/5)" >&2; \
            done; \
            [ "$attempt" = 5 ] || sleep "$((attempt * 5))"; \
        done; \
        return 1; \
    }; \
    download /usr/bin/cosign "${COSIGN_URL}" && \
    echo "${COSIGN_SHA256}  /usr/bin/cosign" | sha256sum -c - && \
    download /usr/bin/rekor-cli "${REKOR_CLI_URL}" && \
    echo "${REKOR_CLI_SHA256}  /usr/bin/rekor-cli" | sha256sum -c - && \
    chmod 0755 /usr/bin/cosign /usr/bin/rekor-cli

RUN download() { \
        dest="$1"; \
        url="$2"; \
        for attempt in 1 2 3 4 5; do \
            if curl -fsSL --connect-timeout 20 --max-time 300 -o "${dest}.tmp" "$url"; then \
                mv "${dest}.tmp" "$dest"; \
                return 0; \
            fi; \
            rm -f "${dest}.tmp"; \
            echo "download failed for ${url} (attempt ${attempt}/5)" >&2; \
            [ "$attempt" = 5 ] || sleep "$((attempt * 5))"; \
        done; \
        return 1; \
    }; \
    download /tmp/aliyun-cli.tgz "${ALIYUN_CLI_URL}" && \
    echo "${ALIYUN_CLI_SHA256}  /tmp/aliyun-cli.tgz" | sha256sum -c - && \
    tar xzf /tmp/aliyun-cli.tgz -C /usr/bin/ && \
    rm -f /tmp/aliyun-cli.tgz && \
    chmod 0755 /usr/bin/aliyun

RUN download() { \
        dest="$1"; \
        url="$2"; \
        for attempt in 1 2 3 4 5; do \
            if curl -fsSL --connect-timeout 20 --max-time 300 -o "${dest}.tmp" "$url"; then \
                mv "${dest}.tmp" "$dest"; \
                return 0; \
            fi; \
            rm -f "${dest}.tmp"; \
            echo "download failed for ${url} (attempt ${attempt}/5)" >&2; \
            [ "$attempt" = 5 ] || sleep "$((attempt * 5))"; \
        done; \
        return 1; \
    }; \
    download /usr/bin/ossutil64 "${OSSUTIL_URL}" && \
    echo "${OSSUTIL_SHA256}  /usr/bin/ossutil64" | sha256sum -c - && \
    chmod 0755 /usr/bin/ossutil64

RUN yum install -y \
        --setopt=install_weak_deps=False \
        libtdx-verify \
        attestation-challenge-client \
        "trusted-network-gateway-${TNG_RPM_VERSION}" && \
    rpm -q trusted-network-gateway | grep -qxF "trusted-network-gateway-${TNG_RPM_VERSION}.x86_64" && \
    /usr/bin/tng --version | sed -n '1p' | grep -qxF 'tng 2.9.2' && \
    yum clean all && \
    rm -rf /var/cache/dnf /var/cache/yum && \
    ! rpm -q trustee && \
    strip --strip-unneeded /usr/bin/attestation-challenge-client /usr/bin/cosign /usr/bin/rekor-cli /usr/bin/aliyun /usr/bin/ossutil64

FROM ${BASE_IMAGE}

COPY --from=tools /usr/bin/attestation-challenge-client /usr/bin/attestation-challenge-client
COPY --from=tools /usr/bin/tng /usr/bin/tng
COPY --from=tools /usr/bin/cosign /usr/bin/cosign
COPY --from=tools /usr/bin/rekor-cli /usr/bin/rekor-cli
COPY --from=tools /usr/bin/aliyun /usr/bin/aliyun
COPY --from=tools /usr/bin/ossutil64 /usr/bin/ossutil64
COPY --from=tools /etc/sgx_default_qcnl.conf /etc/sgx_default_qcnl.conf
COPY --from=tools /usr/lib64/libsgx_dcap_quoteverify.so.1.22.100.3 /usr/lib64/libsgx_dcap_quoteverify.so.1.22.100.3
COPY --from=tools /usr/lib64/libsgx_default_qcnl_wrapper.so.1.22.100.3 /usr/lib64/libsgx_default_qcnl_wrapper.so.1.22.100.3
COPY --from=tools /usr/lib64/libdcap_quoteprov.so.1.13.109.3 /usr/lib64/libdcap_quoteprov.so.1.13.109.3
COPY --from=tools /usr/share/sgx/tee_appraisal_policy.wasm /usr/share/sgx/tee_appraisal_policy.wasm
COPY tools/policies /opt/confidential-agent/policies
# This is the attestation-challenge-client default OPA policy location; it does not install the trustee RPM.
COPY tools/policies/trustee-opa-default.rego /var/lib/attestation/token/ear/policies/opa/default.rego

RUN ln -sf libsgx_dcap_quoteverify.so.1.22.100.3 /usr/lib64/libsgx_dcap_quoteverify.so.1 && \
    ln -sf libsgx_default_qcnl_wrapper.so.1.22.100.3 /usr/lib64/libsgx_default_qcnl_wrapper.so.1 && \
    ln -sf libdcap_quoteprov.so.1.13.109.3 /usr/lib64/libdcap_quoteprov.so.1 && \
    chmod 0755 /usr/bin/attestation-challenge-client /usr/bin/tng /usr/bin/cosign /usr/bin/rekor-cli /usr/bin/aliyun /usr/bin/ossutil64 && \
    chmod 0644 /etc/sgx_default_qcnl.conf /usr/share/sgx/tee_appraisal_policy.wasm /var/lib/attestation/token/ear/policies/opa/default.rego /opt/confidential-agent/policies/*.rego && \
    chmod 0755 /usr/share/sgx /var/lib/attestation /var/lib/attestation/token /var/lib/attestation/token/ear /var/lib/attestation/token/ear/policies /var/lib/attestation/token/ear/policies/opa /opt/confidential-agent /opt/confidential-agent/policies

RUN /usr/bin/tng --version | sed -n '1p' | grep -qxF 'tng 2.9.2' && \
    /usr/bin/cosign version >/dev/null && \
    /usr/bin/rekor-cli version >/dev/null && \
    /usr/bin/aliyun version >/dev/null && \
    /usr/bin/ossutil64 --version >/dev/null && \
    ! ldd /usr/bin/attestation-challenge-client | grep -q 'not found' && \
    /usr/bin/attestation-challenge-client --help >/dev/null
