# Confidential Agent E2E

Run full cloud E2E cases through the single case runner:

```bash
export ALICLOUD_ACCESS_KEY='...'
export ALICLOUD_SECRET_KEY='...'
export DASHSCOPE_API_KEY='...'              # openclaw-bailian/openclaw-a2a/cmaas/claude-code/codex
export DASHSCOPE_BASE_URL='https://dashscope.aliyuncs.com/compatible-mode/v1'
export DASHSCOPE_MODEL='qwen3.8-max'

env -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \
  tools/e2e/run.sh openclaw-bailian
```

Cases:

| Case | What it covers |
|---|---|
| `openclaw-bailian` | `confidential-agent init openclaw` + Bailian 主路径，覆盖默认启用 PEP 的部署分支。 |
| `openclaw-bailian-no-pep` | 同一条 `init openclaw` 主路径，额外传入 `--disable-pep`，覆盖不安装、不启用 cai-pep 的部署分支。 |
| `openclaw-a2a` | Legacy two-OpenClaw A2A bridge coverage. |
| `a2a-data-collab` | Two real LLM-backed agents collaborate over A2A: Analyst delegates a natural-language aggregate data task to a Data Owner, verifies no raw private rows leak, and asserts the A2A TNG routes use the pinned 2.9.2 RPM/config schema. |
| `openclaw-vllm` | `init openclaw-vllm` 生成和 AppSpec 校验；设置 `E2E_OPENCLAW_VLLM_RUN_CLOUD=1` 并选择有库存的 GPU TEE zone，运行真实构建、部署、readiness/chat。未开启时不代表云端验证通过。 |
| `openclaw-vllm-release` | 独立构建并部署旧版 OpenClaw-vLLM 的 release 变体，通过证明状态和受保护入口验证真实推理，确认未启用 debug SSH；完整矩阵同时运行原 debug 用例。 |
| `cmaas` | CMaaS 是主 MCP E2E：自然语言 agent 经 gateway 调用 memory MCP tools，验证 MCP audit 链、虚拟 MCP audit tools、TEE evidence 绑定、TNG 2.9.2 builtin policy 的 deny/recovery 负例、非 TEE baseline rejection 和 snapshot confidentiality；不通过 host connect 直连 MCP `mcp_ports`。 |
| `trustee-duo` | 同一 state/Trustee 下部署 OpenClaw Bailian 与 Codex Bailian，覆盖真实对话、同步/状态、无 CLI 重注入的 reboot 恢复、Trustee fail-closed、逐个销毁与最终清理。 |
| `hermes-agent` | `init hermes` 生成的普通 mkosi 路径：构建期通过官方 installer 从 Hermes 源码安装，运行期通过 `cai-hermes-agent.service` 启动，并覆盖资源注入、deploy、connect、health/chat probe 和失败诊断。 |
| `claude-code` | Claude Code CLI on Bailian qwen3.8-max through the Anthropic-compatible endpoint, with SSH chat and TDX skill probes. |
| `codex` | Codex CLI on Bailian qwen3.8-max through Responses API, with SSH chat, TDX skill, and TNG-protected app-server remote probes. |
| `cli-command-matrix` | Local CLI branch matrix plus an optional real-cloud publish/deploy lane when `E2E_MATRIX_REAL_CLOUD=1`. |

OpenClaw + Bailian 的主路径必须同时覆盖 PEP 和 no-PEP 两个分支。CMaaS 承担 MCP 端到端主覆盖，probe 通过 agent/gateway 入口触发 MCP 工具调用，不把 MCP 端口作为 host connect 的直接访问目标。

`node tools/e2e/tests/openclaw-pep-patch.mjs` checks PEP result propagation and
approval gates against the installed OpenClaw modules in a temporary copy. It
also verifies that intercepted or denied calls never execute the original tool,
ordinary calls still execute, and an unsupported module layout fails before
patching installed files. An optional argument selects another OpenClaw package
directory without modifying that package.

Most E2E cases intentionally mirror the user command flow. Init-covered cases first run `confidential-agent init <target> --non-interactive`, then use the generated AppSpec:

```bash
confidential-agent spec validate --spec <case-spec>
confidential-agent build --spec <case-spec>
confidential-agent peering add --role operator --cidr <operator-cidr> --label ops
confidential-agent deploy --spec <case-spec>
confidential-agent status --live
confidential-agent connect start --service <service-id> --ready-json <ready-json> --wait-ready <seconds>
<case chat probe against the ready-json 127.0.0.1 endpoint>
confidential-agent connect stop --ready-json <ready-json>
```

Business peers are added only after deployment, followed by `confidential-agent peering apply`.
The build phase must not read `peerings.yaml` and must not render Shelter runtime deployment targets or security group config.

CMaaS is the exception to the host `connect start` probe pattern. Its MCP port is `mcp_ports ⊆ mesh_ports`, so the E2E probe runs from the peer agent inside the confidential mesh and also asserts that `connect --render-only --service cmaas` fails. The test must not expose or probe MCP `mcp_ports` through host connect.

The scripts do not unset proxy variables internally. On the current development host, OpenAI-facing tools may need a proxy, but mkosi/DNF access to `yum.tbsite.net` and deploy should run without proxy. Use the outer `env -u ...` wrapper shown above for full E2E runs.

Host prerequisites include the cagent/Shelter RPMs, `python3.11`, `jq`, `openssl`, and `ssh`. Add `/usr/libexec/cagent/bin` to PATH for native `aliyun` and signing tools. Cargo is only needed when building CA from source; Docker is not required for the standard image build/deploy flow. OpenClaw gateway probes additionally need the host `node` and `openclaw` clients. Explicit OCI-source cases may require Podman. `rekor-cli` is shared with the Shelter RPM.
The Trustee case also requires `setsid` and `terraform`. Both suite and standalone case entrypoints add the normal Shelter RPM's `/usr/libexec/shelter/bin` directory to the tool search path, so its packaged Terraform can satisfy this requirement. Timed-out deployments are stopped as a process group so Shelter/Terraform descendants cannot hold deployment locks during cleanup.
OpenClaw token probes use separate client state for valid and invalid credentials
and check the JSON RPC authentication result. A reachable WebSocket or a zero
probe exit code alone does not prove that the gateway accepted a token.

Trustee reboot tests default to `E2E_REBOOT_FORCE_STOP=true`. Set it to `false` to test a graceful reboot separately from crash recovery; the selected mode is recorded in the test results.
Interactive Codex remote use still requires a host `codex` CLI, but the E2E remote probe validates the protected WebSocket endpoint directly.

Legacy cases register service cleanup targets before building images, so a failed or interrupted build can recover its local service state even when deployment never started. Complete host upgrades before starting a suite; replacing the running Shelter executable invalidates in-flight build evidence.

Recovery skips services whose formal destroy already completed and checks both legacy and nested disposable Trustee records. If service cleanup fails, it keeps the Trustee available for a retry. Set `E2E_TRUSTEE_DEPLOY_TIMEOUT` above its 1200-second default when large concurrent image uploads need a longer deployment window.

Before destroying a service, the runner saves resource identifiers and imported-image snapshot associations in `cloud-resource-ledger.json`. With automatic cleanup enabled, it removes an import snapshot only after verifying the recorded image is absent (`Status=ALL`), the snapshot belongs to that import, and its usage is `none`. This covers snapshots retained by the bundled Terraform provider; cleanup errors fail the case. Connect cleanup also discovers ready files created by helpers running in shell command substitutions. Only the shell that installed the exit traps performs resource cleanup; inherited error traps in command substitutions do not destroy the deployment. HTTP connect probes poll for the full readiness budget and save redacted native tunnel diagnostics before removing a failed tunnel. Regression coverage is in `tests/connect-http-readiness.sh`, `tests/subshell-cleanup.sh`, and `tests/connect-cleanup.sh`.

Debug SSH probes keep host keys in the case's `debug-ssh-known-hosts` file. They accept a new host key on first use and reject changes on later connections, without changing the operator's global known-hosts file.

Common environment:

| Variable | Default |
|---|---|
| `E2E_WORK_DIR` | `.tmp/e2e/<case>-<timestamp>` |
| `E2E_STATE_DIR` | `<work-dir>/state` for single-state cases |
| `E2E_REFERENCE_VALUES` | `rekor` |
| `E2E_REGION` | `cn-beijing` |
| `E2E_ZONE_ID` | `cn-beijing-i` for `cn-beijing`, `cn-hongkong-d` for `cn-hongkong` |
| `E2E_INSTANCE_TYPE` | `ecs.g9i.xlarge` for `cn-beijing`, `ecs.g8i.xlarge` for `cn-hongkong` |
| `E2E_GPU_INSTANCE_TYPE` | GPU suite lanes use `ecs.gn8v-tee.4xlarge`, independently of the CPU instance setting |
| `E2E_GPU_ZONE_ID` | GPU suite lanes and standalone OpenClaw-vLLM use this when set; select a zone with GPU TEE stock |
| `E2E_ALLOWED_CIDR` | detected public `/24` |
| `E2E_DESTROY_ON_SUCCESS` | `1` |
| `E2E_DESTROY_ON_FAILURE` | `1` |
| `E2E_MATRIX_REAL_CLOUD` | `0`; set to `1` for the `cli-command-matrix` publish/deploy/unpublish cloud lane |
| `E2E_MATRIX_REPO_MIRROR` | Optional HTTPS Alinux repository base, e.g. `https://mirrors-ssl.aliyuncs.com/alinux` on ECS; uses existing `build.repos` with GPG verification |
| `DASHSCOPE_BASE_URL` | `https://dashscope.aliyuncs.com/compatible-mode/v1` |
| `DASHSCOPE_ANTHROPIC_BASE_URL` | `https://dashscope.aliyuncs.com/apps/anthropic` |
| `DASHSCOPE_MODEL` | `qwen3.8-max` |
| `E2E_CLI_AGENT_NPM_REGISTRY` | `https://registry.npmmirror.com/`; inherits `E2E_NPM_REGISTRY` when set; used by `claude-code` and `codex` CLI package installs |
| `E2E_CLAUDE_CODE_VERSION` | 固定清单中的 Claude Code 版本 |
| `E2E_CODEX_VERSION` | 固定清单中的 Codex 版本 |

Provider credentials:

- Aliyun: environment AK/SK or a usable active `aliyun` CLI profile.
- Bailian cases: `DASHSCOPE_API_KEY` or `BAILIAN_API_KEY`.
- Rekor mode: `E2E_COSIGN_KEY` or an auto-generated key under the work dir. Auto-generation uses `confidential-agent key generate-cosign` with the configured native tools directory.
- `a2a-data-collab`: defaults to unsigned AgentCards. Set `E2E_A2A_SIGNING=1` to exercise Sigstore keyless AgentCard signing; signed mode needs `CA_A2A_SIGSTORE_IDENTITY_TOKEN` or CI OIDC token request envs. `A2A_SIGNER_ISSUER` / `A2A_SIGNER_SUBJECT` may be set explicitly; otherwise a JWT `CA_A2A_SIGSTORE_IDENTITY_TOKEN` is decoded for `iss` / `sub`.

Relative `E2E_WORK_DIR`, `E2E_STATE_DIR`, and `E2E_COSIGN_KEY` inputs are normalized to absolute paths before init/rendering, so validation behaves the same from any caller working directory.
Empty environment values are treated as unset and fall back to defaults.

Keep local secret files outside the runner. Source or translate them into the `export ...` commands above before invoking `tools/e2e/run.sh`; the E2E scripts must not source secret files themselves.

Artifacts:

- `e2e-steps.md` records the exact user-like commands, redacted configs, status output, and probe results.
- Init-covered cases write the real `confidential-agent init` output under the work dir and run the generated AppSpec directly.
- Legacy non-init cases may still keep checked-in templates under `tools/e2e/cases/<case>/templates/`.

All CA image builds use mkosi. The runner verifies Shelter `initrd.binaries` support and records the selected binary SHA256. Cleanup failures make the case fail. Use fresh work/state directories when validating a Shelter upgrade; do not reuse old build artifacts.

### Shared-image consumers

`allinone-reuse.py` tests a published allinone image from a deployment host
without producer state or a local QCOW2. Prepare at least two private fixture
directories. Each contains `appspec.json` (a standard AppSpec written as JSON)
and `probe.json`; use distinct service IDs, `service.ssh: true`, and the same
`deploy.image` selection. Resource paths and release/key paths must be absolute.
The image configuration uses the harness's native provider/model settings.
An optional `runtime_command` in `probe.json` checks the native gateway or API.
For a service exposing one HTTP port through `service.connect`, an optional
`connect_http_path` also checks HTTP 200 through CA's protected connection.
Stable endpoint routing tests require two peers and the extra peer to expose
the same mesh port, excluded from `service.connect`. Connect ports are opened
to operator peerings; they do not grant guest-to-guest mesh access. The runner
rejects unsuitable stable-endpoint fixtures before deploying infrastructure.

`probe.json` supplies an SSH command and its expected model-response marker.
Keep the complete marker out of the prompt so echoed input cannot satisfy it:

```json
{"command": "cai-agent --print 'Join REUSE_ and OK without a space'", "expect": "REUSE_OK"}
```

If account credentials are unavailable, explicitly use `command: null`, a
`skip_reason`, and `--allow-model-skip`; installation and startup are still
checked, and the model call is recorded as skipped. Keep fixtures and credentials
outside the repository. Give parallel test lanes independent resource source
files, especially when enabling application error or resume fault injection.

```bash
python3.11 tools/e2e/allinone-reuse.py \
  --fixture /private/fixtures/agent-a --fixture /private/fixtures/agent-b \
  --work-dir /private/results/reuse-01 --operator-cidr YOUR_IP/32 \
  --parallel 2 --reboot
```

The work directory must be new. Consumers share one CA state directory, with
independent service identities and keys.
The test rejects altered signatures, wrong publisher keys and incorrect pins;
checks attestation, resource content, independent credentials and pinned SSH
host keys; optionally forces a reboot and verifies persisted data; and destroys all attempted
consumers, checking that the published image survives. Challenge reboot recovery
uses the existing `inject` command; Trustee recovery does not invoke it. Private
command output and any cleanup failure remain in the work directory.

`--console-isolation` requires guest function tracing. It checks application
stdout and stderr in the private journal and traces console/kernel-log writes,
using a direct `/dev/console` write from the same unit as a positive control.
ECS console output is also checked for application markers; this API returns
startup/restart/shutdown caches, so its output alone cannot prove isolation.

For Trustee fixtures, supply `--trustee-url`, `--trustee-admin-key`, and optionally
`--trustee-management-url`. Use a fresh disposable stock Trustee and restrict its
guest-facing port to the test instances. The runner explicitly adopts its
current policies, uses one shared CA state to own those policies, and serializes
deployment updates. Per-instance keys, secrets and guest data remain separate.
If that disposable Trustee runs on the runner host, `--trustee-outage-service
trustee-gateway.service` additionally checks that reboot cannot bypass unavailable key
release and that restoring Trustee allows recovery without CLI reinjection.
Select the unit serving the configured guest URL; the stock Trustee RPM exposes
that URL through `trustee-gateway.service`.

### Clean ECS build host

Use a new Alibaba Cloud Linux 3 system image and copy only the CA source tree
and the single Shelter RPM in `hack/`. Do not copy a workstation's Cargo cache,
package cache, built CA binaries, container images, or prebuilt guest images.
Run the normal installer, then repeat it to check idempotency:

```bash
one-click/install.sh install-only --non-interactive --yes --skip-host-openclaw
tools/e2e/prepare-host.sh
rpm -V shelter cryptpilot-fde-host
hack/check-shelter.sh /usr/bin/shelter
```

The Shelter RPM bundles the Cryptpilot guest pinned in its `PKGINFO`; check that
metadata for the source commit and any pending upstream fix. Its host tool and
convert/enhance scripts are installed from the system package repository through
the `cryptpilot-fde-host` dependency.

For the real cloud matrix, use the newly installed CA executables under
`/usr/bin` (RPM) or `/usr/local/bin` (source install), set `E2E_SKIP_CARGO_BUILD=1`, `E2E_SKIP_SHELTER_INSTALL=1`,
`E2E_MATRIX_REAL_CLOUD=1`, and `E2E_MATRIX_REFERENCE_VALUES=rekor`. The harness
also needs `aliyun` on `PATH`; add `/usr/libexec/cagent/bin` to PATH. Supply cloud credentials separately in a root-only file.
Builds and E2E use the public HTTPS repositories by default; running cloud tests
does not imply that the build host is an ECS instance. Only when the build host
can reach the ECS internal mirror, explicitly set
`E2E_MATRIX_REPO_MIRROR=https://mirrors-ssl.aliyuncs.com/alinux` to select it
through the existing signed HTTPS repository configuration. Leave this variable
unset on other build hosts. Record the base image ID, RPM and executable digests, installed
package inventory, native tool package versions, and cleanup results before deleting the host.

### Unified acceptance matrix

`run.sh <case>` remains available. The suite entry point schedules dependencies
and separates CPU and GPU concurrency:

```bash
tools/e2e/run.sh --list
tools/e2e/run.sh --suite full --jobs 3 --gpu-jobs 3 --build-jobs 2
# Focused execution includes the required image producer automatically:
tools/e2e/run.sh --case inference-reuse-nlb --gpu-jobs 1
```

Groups are `legacy`, `reuse`, `inference`, and `platform`. The full matrix runs
all eleven original cases, signed all-in-one image reuse with six harnesses under
both attestation modes, encrypted Qwen3.8-27B inference through single instances,
ALB/OHTTP and NLB/OHTTP or RATS-TLS, and release/debug
variants. Platform coverage includes simultaneous deployment, interrupted
infrastructure, delivery resume, frozen inputs, application failure, console
isolation, PTYs, and stable peer endpoints.

Image preparation is separate from deployment scenarios. `publish-reuse-images`
builds and publishes only all-in-one release. `prepare-inference-images` is the
only task building both release and debug in one invocation; it verifies both
artifacts and publishes/imports release once for all seven release inference
lanes. These lanes keep their topology, transport, attestation, one-click and
recovery checks. The debug lane deploys the original local build using its
original state and SSH key; it does not distribute debug keys or a signed debug
image. Only that lane mutates the prepared build's deployment state, and its
local images remain until all consumers finish. OpenClaw-vLLM builds debug in
its debug lane and release in its release lane. A2A data collaboration builds
only debug for each of its two different services.

The full matrix has 23 functional lanes and two fixture tasks. Metadata labels
fixture tasks separately. `--build-jobs` bounds image builds across both case
pools (default two); deploying, loading models and running assertions keep the
CPU/GPU case concurrency. Command timings are saved as `*.timing.json` or
`command-timings.jsonl`, without command arguments or environment values. Build
slot waits have separate timings, and Shelter reports native disk lock waits.
The suite report also records total elapsed time including final cleanup.
Concurrent suites on the same host can share their build budget by setting the
same `E2E_BUILD_SLOT_DIR` and `--build-jobs` value. The selected directory and
limit are recorded in suite metadata.

Image uploads default to `CA_IMAGE_OSS_ENDPOINT=auto`. Same-region ECS metadata
and a successful internal HTTPS probe select the internal endpoint; off-cloud,
cross-region, disabled metadata or an unreachable internal endpoint select
public. Selection uses bounded probes, bypasses proxies for metadata/internal
traffic and never sends cloud credentials to the probe. To override routing:

```bash
export CA_IMAGE_OSS_ENDPOINT=public    # or internal, auto, or an HTTPS endpoint
```

This applies to CA image publication and, with the updated Shelter, direct
deployment uploads. Standalone Shelter also supports `SHELTER_OSS_ENDPOINT` and
`deploy.oss_endpoint` (the explicit spec wins). Model download configuration
(`--oss-endpoint` / `CA_OSS_ENDPOINT`) is independent of image upload routing.

Prepare a fresh installed runner as above. Export `CA_BIN`, `CA_AGENTD_BIN`,
`CA_GATEWAY_BIN`, and `CA_PEP_BIN` for its installed executables. Supply
`E2E_MODEL_KEY_FILE` and `E2E_MODEL_OSS_BUCKET` for the encrypted model, and either
`E2E_QODER_NATIVE_CONFIG` (a directory containing native `.auth` and settings)
or `QODER_PERSONAL_ACCESS_TOKEN`. Qoder must have a fixed model selected through
its `/model` interface, or an explicit `E2E_QODER_MODEL` identifier. Automatic
routing and default tiers are rejected. Configure BYOK through Qoder's official
wizard; the fixture preserves those settings and passes `--model` explicitly.
Record whether the selected model uses BYOK or Qoder's native provider when
reporting acceptance results. Standard cloud/Bailian credentials are read
from the environment. Version defaults come from the root
[`software-versions.json`](../../software-versions.json); the six-harness tests
also inspect the actual guest versions and make real model calls.
Fresh OpenClaw builds pin the DingTalk plugin commit and its pnpm version in the
same manifest. The pnpm tarball can come from the configured npm mirror, but its
bytes must match the checked-in SHA-512 digest before installation. CLI agent
and Node bootstrap downloads use the mainland npm mirror by default.
Published consumers configure Rekor using the installed Shelter tool path,
without a build-time signing key. Their reference metadata is authenticated
through the signed image release. Image export binds runtime metadata to the
requested variant's saved build and includes its public Rekor entry identifiers.
Consumer reports can retrieve those entries without producer upload logs or
local SLSA files. To include native schema regression checks,
run `CA_BIN=/path/to/tested/confidential-agent python3.11 -m unittest discover -s tools/e2e/tests`.
Those checks validate every harness under both attestation modes and the
supported inference consumer specs with the real CLI; they also retain the
rejection of published debug images with build-time SSH credentials.
`prepare-host.sh` installs the exact Node/OpenClaw versions needed by the host
probes, using the existing one-click helpers. Missing host commands are reported
as blocked prerequisites before a lane creates cloud resources.

Every lane has a private work directory, evidence, a durable ownership registry,
and an explicit result. A missing credential, skipped deployment, failed
assertion, absent result, or cleanup failure prevents the suite from passing.
The suite records a fresh cloud run identity for each case, preventing concurrent
Trustee fixtures from sharing names or idempotency tokens. Standalone runs also
add a random suffix by default; an explicit `E2E_RUN_ID` is preserved for recovery.
Published fixtures are retained until consumers finish and then removed by the
producer. Disposable Trustees are isolated per lane. `--trustee-outage-control`
can invoke the owned Trustee lifecycle adapter with `stop` and `start` when it
runs on a separate ECS instance.

Publisher cleanup records exact imported-image/snapshot associations before
unpublishing, then removes only the recorded automatic snapshots after image
absence and unused status are confirmed. Failed inventory or snapshot deletion
fails cleanup. External signed-fixture consumers cannot run publisher cleanup.
Consumer ledgers are recorded before each destroy and merged across later
inventory passes, including services removed during isolation or interruption
tests. Shared resources referenced through Terraform data sources are excluded.

A failed image producer can continue in its original private work directory:

```bash
E2E_WORK_DIR=/path/to/failed-suite/publish-reuse-images \
  python3.11 tools/e2e/publish_fixtures.py --resume
# Retry inference publication while keeping its already verified local builds:
E2E_WORK_DIR=/path/to/failed-suite/prepare-inference-images \
  python3.11 tools/e2e/inference_fixtures.py --resume
```

The producer keeps prior results and logs under `attempts/`, preserves the
original specifications and signing material, and runs Shelter's native artifact
verification for every retained variant before publishing it. Changed specs,
deployed services, missing variants and verification failures prevent reuse.
Services without a completed build still run the normal build step. Cleanup
remains owned by that producer after all signed-image consumers finish.

GPU instance families and the vLLM service have a two-hour application readiness
budget, configurable with `CA_APP_READY_WAIT_SEC`. Image publication also waits
up to two hours for cloud import, configurable with `CA_IMAGE_IMPORT_TIMEOUT_SEC`.
These budgets accommodate model loading and large encrypted images; all required
readiness and inference assertions still have to pass.

Inference checks readiness on every expected replica and uses native vLLM
counters to prove requests reach each backend. The NLB reuse lane creates a
second service from the same image, compares independent identities and OHTTP
public key groups, waits for key rotation, and verifies the other service and
publisher image survive destruction of the replica group. The single Trustee
reuse lane rejects a wrong model key and an unavailable Trustee during cold
boot, then verifies recovery without reinjection after boot.

A service owns its identity and delivered credentials. Replicas of that service
share them and own separate disks; separate services retain independent state.
Replica count is fixed when deploying. Use recorded instance IDs with `ssh
--instance-id` or `connect start --instance-id` for individual node diagnostics.
ALB requires OHTTP, NLB supports both protocols, and the current OHTTP replica
group supports one connect port. Release inference checks use protected business
and management interfaces; debug SSH is used only for the debug variant.

For an independent clean consumer host, set `E2E_PUBLISHED_FIXTURES` to the
all-in-one producer directory containing `allinone/` and
`allinone-publisher-state.json`. Set `E2E_INFERENCE_FIXTURES` to the inference
producer directory containing `inference-release/`. The former combined export
directory is also accepted through `E2E_PUBLISHED_FIXTURES` for compatibility.
Copy only their envelopes, public keys, and publication state; do not copy guest
disk images, caches, producer private keys, or producer runtime state. Select
the two all-in-one lanes and the inference reuse lanes with repeated `--case`.
Imported fixtures stay owned by their original producer, which must retain and
clean the cloud images after this consumer run finishes.
Independent consumers receive only signed release fixtures. The debug lane
requires the original local producer and cannot run from public exports.
Create a lease file and pass its path to the producer suite with
`--fixture-consumer-lease`. Remove it only after the independent consumer has
finished resource cleanup. Both producers wait for that lease before unpublishing;
an interrupted or expired lease keeps cleanup incomplete and fails acceptance.
An incomplete local consumer cleanup also retains its producer's fixtures.

`E2E_REKOR_URL` can select a real self-hosted Rekor service when exercising an
isolated environment. The default remains `https://rekor.sigstore.dev`. The URL
is recorded in suite metadata and applied to generated native and legacy specs;
signing, required publication, and Trustee verification remain enabled. A test
backend must provide the subject-hash index and stored intoto attestations that
stock Trustee uses. Producer fixtures also install an explicit daemon allowlist
entry for that backend so guest peer checks keep enforcing URL trust. Set
`CA_TRUSTED_REKOR_URLS` to include the same service for host-side peer commands.
Record its version and report public-service failures
separately when comparing acceptance runs with different backends.
