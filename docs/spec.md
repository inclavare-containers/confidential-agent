# AppSpec Reference (`confidential-agent/v1`)

## 1. Spec 顶层

```yaml
schema: confidential-agent/v1   # 必填，固定字符串
service:        { ... }         # 必填
build:          { ... }         # 本地构建必填；deploy.image 模式省略
deploy:         { ... }         # 必填
attestation:    { ... }         # 必填
secrets:        { ... }         # 可选，默认空
resources:      { ... }         # 必填，可以为空对象 {}
a2a:            { ... }         # 可选，跨组织 Agent 发现
```

> **严格模式**：`#[serde(deny_unknown_fields)]`。出现任何未知字段都会让解析直接失败——这是为了避免拼写错误把安全相关字段静默忽略。

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `schema` | string | ✅ | 必须等于 `confidential-agent/v1` |
| `service` | object | ✅ | 见 §2 |
| `build` | object | 条件 | 本地构建必填；与 `deploy.image` 互斥，见 §3 |
| `deploy` | object | ✅ | 见 §4 |
| `attestation` | object | ✅ | 见 §5 |
| `secrets` | object | ❌ | 默认 `{}`，见 §6 |
| `resources` | map | ✅ | 必须显式声明（即便为空 `{}`），见 §7 |
| `a2a` | object | ❌ | 可选，见 §8 |

---

## 2. `service` — 服务身份与端口

```yaml
service:
  id: openclaw                  # 服务唯一标识，作为 state-dir 子目录、安全组规则名前缀
  ports: [18789]                # 服务在 Guest 中真实监听的业务端口集合；可为 [] 表示 client-only/job
  connect: [18789]              # ports 子集；允许 host connect / A2A 单向 RA 接入
  mcp_ports: []                 # mesh_ports 子集；按 MCP 协议做 gateway 审计和 virtual tools 注入
  app_service: cai-openclaw-gateway.service  # 可选；daemon 用它判断应用 systemd unit + 端口是否 ready
  ssh: false                   # 可选；release 镜像的独立实例 SSH 身份
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `id` | string | ✅ | 仅允许 `[A-Za-z0-9_-]`；不能为空 |
| `ports` | `[u16]` | ✅ | 可为空；不能含 0、不能重复。空列表表示该 workload 不暴露入站业务端口，但仍作为 mesh identity 可访问其他 active 服务 |
| `connect` | `[u16]` | ❌ | 默认 `[]`；每个端口必须出现在 `ports` 中；不能重复 |
| `mcp_ports` | `[u16]` | ❌ | 默认 `[]`；每个端口必须属于 `service.ports - service.connect`（即 `mesh_ports`）；不能重复 |
| `app_service` | string | ❌ | 可选；设置后不能为空；应为 guest 内的 systemd unit 名，例如 `cai-openclaw-gateway.service` |
| `ssh` | bool | ❌ | 默认 false；使用 release 镜像，镜像必须支持下述 SSH 文件契约 |

`app_service` 会写入 daemon bootstrap。若设置，`confidential-agentd` 会启动并检查该 systemd unit，同时确认 `service.ports` 都在 guest 本地可连接；`status --live` 中的 `app_ready` 才会变成 ready。若不设置，`app_ready` 只表示资源和 TNG 层就绪，不证明用户应用已经启动。

`service.ssh: true` 在部署端生成每个实例独立的 Ed25519 client/host 密钥。CA 通过现有 challenge/Trustee 资源流程下发 `/opt/data/cai-ssh/authorized_keys` 和 `/opt/data/cai-ssh/host_key`，并仅向 peering 的 `ssh` scope 放行 22 端口；SSH 不属于应用 `service.ports`。镜像自行提供使用这两个文件的 SSH 服务，并在资源就绪后启动它（参见 allinone 示例）。`confidential-agent ssh <service> -- <command>` 使用私钥及固定的 host 公钥校验连接。私钥不进入共享镜像或发布元数据，且该模式与 debug 镜像的原有 SSH 机制互斥。

端口语义固定为：`service.ports` 是应用在 Guest 内真实监听的业务端口集合，也是 TNG 对外建立 ingress/egress 的端口全集；gateway 和 MCP 不会引入新的业务端口。`service.ports: []` 表示该 workload 是 client-only/job，不暴露入站业务端口，但仍会获得 gateway identity、进入 mesh bundle，并可通过 `/etc/cai/service-directory.json` 访问其他 active 服务。`service.connect` 是其中允许 host connect 和跨组织 A2A 以单向 RA 访问的子集，调用方只验证服务端 TEE。`mesh_ports = service.ports - service.connect` 是同 state-dir confidential-only mesh 端口，调用双方都会做 RA。敏感的 confidential-only API 只能放在 `mesh_ports` 上，不应暴露在 `connect` 端口上。

`service.mcp_ports` 不改变端口暴露契约，也不要求应用改监听端口，但它必须是 `mesh_ports` 子集。guest 内的 `cai-gateway` 只覆盖 `mesh_ports`：列入 `mcp_ports` 的端口按 MCP JSON-RPC 解析请求，给 `tools/list` 注入 `tee_attest`、`audit_status`、`audit_verify` 三个虚拟工具，并对 MCP 请求/响应追加哈希链审计记录；未列入 `mcp_ports` 的 mesh 端口按 raw TCP 透传，只做 service identity，不做 MCP tool 注入或 MCP audit 解析。`connect` / A2A / AgentCard 路径不经过 gateway，也不携带 gateway client identity。

**多服务约束**：跨服务的 `service.ports` 不允许冲突——`deploy/inject` 时 [`validate_mesh_port_conflicts`](../cli/src/app/workflows.rs) 会拒绝掉同 state-dir 下的端口重复。

---

## 3. `build` — 镜像构建

CA 使用 Shelter 主线的 mkosi 构建流程；普通构建允许安装脚本访问网络。`build.base_image` 和 `build.with_network` 已移除，旧配置需要删除这两个字段，CLI 会拒绝它们。rootfs 与 initrd 的网络由 CA 显式启用。

```yaml
build:
  image_name: openclaw-agent                  # 必填，构建产物名 + cloud 镜像名 prefix
  resize: 30G                                 # 可选，渲染为 Shelter disk.size
  kernel_cmdline_append: "swiotlb=4194304,any" # 可选，UKI cmdline 追加项
  cleanup:                                    # 可选，透传给 Shelter cleanup
    remove_static_libs: false                 # 可选，保留 *.a 静态库
  container:                                  # 可选，Shelter OCI workload payload
    image: nousresearch/hermes-agent:v2026.6.5
    mode: rootfs                              # runtime | rootfs
    runtime: containerd                       # runtime 模式使用：containerd | podman
    name: hermes-agent
    pull: missing                             # missing | always | never；archive 模式只能 missing
    env:
      API_SERVER_ENABLED: "true"
    working_dir: /opt/data
    network: true
  packages: [nodejs, npm, jq, ...]            # 可选，注入到镜像里的 RPM/DEB 包列表
  files:
    - source: ../../target/debug/cai-pep
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/cai-pep-default-policy.json
      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json
  scripts:
    - ./install-openclaw.sh                   # post-install 阶段执行
  variants:
    release:
      enabled: true
    debug:
      enabled: true                           # 不强制开 SSH，仅当 deploy.image_variant=debug 才生效
```

| 字段 | 类型 | 必填 | 默认 | 校验 |
|---|---|:-:|---|---|
| `image_name` | string | ✅ | — | 仅 `[A-Za-z0-9_-]` |
| `resize` | string | ❌ | `None` | 例如 `30G`，渲染为 Shelter `disk.size` |
| `kernel_cmdline_append` | string | ❌ | `None` | 透传到 `security.disk.cryptpilot.uki_append_cmdline` |
| `cleanup.remove_static_libs` | bool | ❌ | Shelter 默认值 | 透传给 Shelter 顶层 `cleanup.remove_static_libs`；vLLM 等需要在运行期编译 Python 扩展的镜像可设为 `false` 保留静态库 |
| `container` | object | ❌ | `None` | 原样渲染为 Shelter 顶层 `container:`；用于 OCI workload |
| `repos` | `[object]` | ❌ | `[]` | 透传 Shelter `repos`；每项含唯一 `name`、HTTPS `baseurl` 和 HTTPS `gpgkey`，保持 RPM 签名校验 |
| `packages` | `[string]` | ❌ | `[]` | 透传给 Shelter `packages` |
| `files` | `[BuildFileSpec]` | ❌ | `[]` | 见下表 |
| `scripts` | `[path]` | ❌ | `[]` | 一律在 `post-install` 阶段执行，在 `confidential-agent-guest-setup.sh` 之后 |
| `variants.release` | `BuildVariantSpec` | ❌ | `enabled=true` | 见下 |
| `variants.debug` | `BuildVariantSpec` | ❌ | `None` | 见下 |

#### 3.1 `BuildFileSpec`

```yaml
- source: ./relative/or/abs.path        # 必填
  target: /absolute/path/in/guest       # 必填，必须以 / 开头
  executable: false                     # 可选，默认 false
```

#### 3.2 `BuildContainerSpec`

```yaml
container:
  image: docker.io/org/app:tag          # image 或 archive 至少一个
  archive: ./app.oci.tar               # 可选，本地 OCI archive
  mode: rootfs                         # 默认 runtime；可选 runtime/rootfs
  runtime: containerd                  # 默认 containerd；runtime 模式使用
  name: app                            # 仅允许 ASCII 字母、数字、.、_、-
  pull: missing                        # 默认 missing；archive 模式只能 missing
  command: ["/usr/local/bin/app"]      # 默认使用 image Entrypoint+Cmd
  env:
    APP_ENV: production                # key 必须是合法 shell 环境变量名
  working_dir: /srv/app                # 必须是绝对路径
  network: true                        # 显式要求 guest/container 网络
  mounts:                              # runtime 模式可选；guest path bind 到容器内
    - source: /var/lib/app/data        # guest 上的绝对路径
      target: /opt/data                # 容器内绝对路径
      read_only: false                 # 默认 false
```

`container` 是 workload payload。CA 仍然以 `mkosi + cryptpilot` 生成 TDX guest/rootfs，并把 OCI payload 交给 Shelter master 的 `container:` 机制处理。
`container.mounts` 只描述 runtime 容器的 bind mount；如果文件来自 CA runtime `resources`，仍然需要在顶层 `resources` 中显式声明资源注入到 guest 上的 `source` 路径，再把该 guest 路径 mount 到容器。

#### 3.3 `BuildVariantSpec`

```yaml
release:
  enabled: true                         # 默认 true
  ssh_public_key: ./debug.pub           # release 中**禁止**出现这一项；debug 中可选
debug:
  enabled: true
  ssh_public_key: ./debug.pub           # 仅 debug 允许，且 release variant 永远拿不到
```

约束：
- `release.ssh_public_key` 出现会让解析失败（[`validate_variant`](../core/src/spec.rs)）；release 镜像不允许有 SSH 入口，这是一条硬约束。
- 当 `deploy.image_variant=debug` 但 spec 没声明 `variants.debug` 或 `enabled=false` → 直接报错。
- 当 `image_variant=debug` 且没显式设 `ssh_public_key`，CLI 会**自动**在 `<state-dir>/services/<id>/secrets/debug_ssh{,.pub}` 生成 ed25519 密钥对（[`cli/src/app/debug_ssh.rs`](../cli/src/app/debug_ssh.rs)），并在 deploy 完成后输出 `ssh -i ... root@<ip>` 提示。
- `confidential-agent build` 会构建所有 `enabled=true` 的 variants，并在 `manifest.json` 的 `variants` map 中记录每个 variant 的 `shelter_build_id` 与 build result；`deploy.image_variant` 只决定当前 state 选择哪个 variant 作为默认部署目标。

---

## 4. `deploy` — 云部署

```yaml
deploy:
  provider: aliyun
  image_variant: release       # 可选，默认 release，可选值: release | debug
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i        # 也接受 alias `availability_zone`
  disk_gb: 200                 # 可选
  vpc_id: vpc-xxx              # 可选，留空让 Terraform 自建
  vswitch_id: vsw-xxx          # 可选
  security_group_id: sg-xxx    # 可选
  private_ip: 10.0.1.20        # 可选，固定内网 IP
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `provider` | enum | ✅ | 当前仅 `aliyun` |
| `instance_type` | string | ✅ | 非空 |
| `image_variant` | string | ❌ | `release` (默认) 或 `debug`；若是 `release`，必须 `variants.release.enabled=true`；若是 `debug`，必须 `variants.debug.enabled=true` |
| `disk_gb` | u32 | ❌ | 未设置时请求至少 32 GiB，并按 `build.resize` 向上调整；显式值不得小于 20 GiB 或构建镜像容量。已发布镜像必须显式设置并满足镜像最低容量 |
| `region` | string | ✅ | 非空 |
| `zone_id` | string | ✅ | 非空。可写成 `availability_zone` 别名 |
| `vpc_id` / `vswitch_id` / `security_group_id` | string | ❌ | 留空时由 Shelter 自动创建 |
| `private_ip` | string | ❌ | 期望分配的固定内网 IP |

`deploy` 不再包含 `security.allowed_cidr` / `security.a2a_peer_cidrs`。所有入向安全组规则都从 `<state-dir>/peerings.yaml` 派生，由 `confidential-agent peering ...` 管理；旧字段会被严格模式拒绝，可用 `confidential-agent migrate <spec>` 迁移。`build` 阶段只产出镜像和 build artifact，不读取 `peerings.yaml`；operator peering 应在 build 完成后、首次 deploy 前添加。

若先执行 `confidential-agent image publish <service> --spec <path>`，CLI 会在本地 service state 中记录 published image。后续 `deploy` 仅在 provider、region、variant、build id 和镜像内容 hash 均匹配，且本地发布状态为 `available` 时复用该 `image_id`，并在 Shelter deploy 配置中跳过本地镜像上传和 ImportImage。Published image 不由 `destroy` 统一删除；使用 `image unpublish` 删除指定服务的发布镜像，或使用 `image prune --dry-run --all` 审计后清理旧 build/failed 发布记录。

镜像与实例分别管理：`image list` 列出本地构建和云端发布（不包含消费者实例）；`status` / `tui` 展示实例（不包含仅构建的 builder）。镜像状态包括 `built`、`publishing`、`published`、`retiring`、`failed`、`local-missing`；发布状态是带时间戳的本地记录，不宣称实时云端状态。`image list --json` 返回完整 `publications`，包括各地域的状态、错误、所有权和待清理标记。本地文件缺失不会隐藏云镜像；`CURRENT` 按当前构建的所有 variant 判断。

实例依次经过 `deploying → deployed → activating → active`；销毁经过 `destroying → deleted`。`activating` 已参与资源和 mesh 配置，但尚未确认所有副本应用就绪，可以用原配置执行 `deploy --resume`。基础设施 apply 中断仍保留 `deploying`，不会盲目重放；`status` 保留资源名、Terraform 目录，并把云资源观测标记为 `unknown`。应用真正就绪后才写入 `active`；这不等于模型对话已成功。

清理动作的范围：

| 命令 | 删除内容 | 保留内容 |
|---|---|---|
| `destroy INSTANCE` | 该实例管理的基础设施 | 共享云镜像、本地记录 |
| `image rm BUILDER` | 本地镜像文件及其硬链接 | 发布台账、构建元数据、管理身份 |
| `image unpublish BUILDER` | 所选发布的云镜像、自有导入快照及剩余中转对象 | 本地产物、实例记录 |
| `forget SERVICE` | 无运行资源、无发布台账的本地管理目录 | 不调用云端删除 |

`image prune --dry-run` 预览旧发布和中转对象清理；默认保留当前所有 variant、消费者引用和已导出的镜像，`--all` 仍不绕过引用/导出保护。OSS 中转对象清理独立于云镜像退休，失败后可通过再次 publish 或 prune 重试。自动发现并接管的已有镜像是引用，不自动取得删除权限；只有显式 `unpublish --force` 才能删除它。发布失败与退休进度分别记录，`publish` 不会隐式删除旧云镜像。导入请求使用持久化 ClientToken，响应丢失时复用同一个请求身份。自有导入快照在删除镜像前记录，逐个清理并保存进度；快照清理失败不会丢失台账。普通删除使用云平台的引用检查，显式 `--force` 才强制删除镜像；快照删除仍不强制绕过使用保护。


清理会检查当前工作区所有服务的部署和导入引用，不只检查发布者自身。`image export` 会记录保留标记，自动 `prune`（包括 `--all`）不再删除该镜像；旧版本已经导出的 release 没有此标记，升级后应重新 export 以登记保留。跨主机消费者无法由本地 state 自动发现。发布者应停止分发、遵守已公布的保留期并确认消费者不再依赖创建或恢复后，才通过 `image unpublish --image-id <id> --force` 显式退役。部分云删除失败会返回非零退出码，并保留失败记录供重试。

镜像上传的端点由 `CA_IMAGE_OSS_ENDPOINT` 控制，默认 `auto`：只有带 token 的 ECS 元数据确认同地域且内网 HTTPS 探测成功时使用内网，否则使用公网。可显式设置 `public`、`internal` 或 HTTPS 端点；显式配置不自动回退。CA 的 `image publish` 和新版 Shelter 的本地镜像部署上传都支持该设置。模型下载使用的 `CA_OSS_ENDPOINT` / `--oss-endpoint` 与镜像上传独立。

独立部署端可以省略 `build`，通过 `deploy.image` 选择发布者签名的镜像：

```yaml
deploy:
  provider: aliyun
  region: cn-beijing
  zone_id: cn-beijing-i
  instance_type: ecs.g9i.xlarge
  disk_gb: 80
  image:
    release: ./image-release.json
    trusted_key: ./publisher.pem
    # 可选：manifest: sha256:<完整的64位manifest摘要>
```

发布端先运行 `image publish`，再运行 `image export <service> --signing-key <Ed25519 PKCS8 PEM> --output image-release.json`。消费者仅需要 release、独立认可的发布者公钥以及自己的 AppSpec 和资源文件，不需要生产端 state、构建输入或 QCOW2。CA 在创建资源前验证签名和地域映射，自动读取并保存 manifest 摘要，将验证过的参考值交给现有 challenge/Trustee 流程。可选的 `manifest` 字段用于额外固定版本；显式指定但不匹配会失败。每个 service 使用自己的身份、磁盘口令和 Terraform state；`destroy` 不删除共享镜像。该模式只允许 release 变体；需要内置调试密钥的 debug 镜像不能作为共享发布镜像。

当前 E2E 默认示例使用 `cn-beijing-i + ecs.g9i.xlarge`。香港 region 的 TDX 默认组合为 `cn-hongkong-d + ecs.g8i.xlarge`；实际可用性仍可能受账号库存和可用区限制影响，必要时显式覆盖 `region`、`zone_id` 和 `instance_type`。

**安全组规则的自动构造**（详见 [`shelter/src/lib.rs::security_group_rules`](../shelter/src/lib.rs)）：

| 规则名 | 入向端口 | 来源 | 何时启用 |
|---|---|---|---|
| `control_8006_peer_<cidr>` | 8006 | `peerings` 中含 `control` scope 的 CIDR | host CLI 通过 `attestation-challenge-client inject-resource` 注入资源 |
| `status_8088_peer_<cidr>` | 8088 | 含 `status` scope 的 CIDR | `confidential-agentd` 只读状态 HTTP |
| `agent_card_8089_peer_<cidr>` | 8089 | 含 `agent_card` scope 的 CIDR | 外部 peer 拉取 `/.well-known/agent-card.json` |
| `ssh_22_peer_<cidr>` | 22 | 含 `ssh` scope 的 CIDR | 仅当 `image_variant=debug` |
| `connect_<port>_peer_<cidr>` | `service.connect[]` | 含 `connect` scope 的 CIDR | host connect / A2A client 通过 RATS-TLS 接入本服务，单向 RA |
| `mesh_<port>_peer_<cidr>` | `service.ports[] - service.connect[]` | 同组织 active 服务公网 IP `/32` 或含 `mesh` scope 的 CIDR | 同组织 confidential-only mesh 数据面，双向 RA；gateway 只覆盖这些端口 |

**Shelter 运行目标映射**（独立的 V2 YAML 由 [`render_runtime_config`](../shelter/src/lib.rs) 产生）：

```yaml
version: 2
default_target: deploy
targets:
  deploy:
    provider: terraform-alicloud
    trust: verified
    attestation: disabled
    provider_options:
      name: openclaw-20260429201011 # = sanitize(service.id) + "-" + run_id
      backend: terraform
      cloud: alicloud
      region: cn-beijing
      zone_id: cn-beijing-i
      instance_type: ecs.g9i.xlarge
      disk_size: 32                # 示例 GiB 值，必须容纳已构建镜像
      cc_mode: tdx
      attestation_authority: external
      security_group:
        rules: []                 # 根据 peerings 和服务端口生成
      tags:
        confidential-agent-service: <id>
        confidential-agent-image-variant: <variant>
```

Build 输出使用显式 `version: 1` 的 Shelter 主线构建配置：`build.resize` 渲染为顶层 `disk.size`，`build.cleanup` 渲染为顶层 `cleanup`，cryptpilot 渲染为 `security.disk.engine: cryptpilot` 和 `security.disk.cryptpilot.*`，保留 `initrd.binaries`、Rekor 与容器挂载字段。部署使用上面的独立 V2 运行目标，构建快照不会随部署或网络更新改写。CA 验证 TDX 证据并释放密钥，因此目标明确声明外部认证方。

---

## 5. `attestation` — 远程证明

```yaml
attestation:
  tee: tdx                       # 当前仅 tdx
  mode: challenge                # challenge（默认工作流）| trustee
  reference_values: rekor        # sample（默认） | rekor
  rekor:                         # 仅 reference_values=rekor 时必需
    artifact_id: openclaw-agent-release   # 可选，默认 = build_id
    artifact_type: uki                    # 默认 uki
    artifact_version: "20260430"          # 可选
    rekor_url: https://rekor.sigstore.dev # 默认
    cosign_key: ./cosign.key              # required=true 时必填
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    rv_name: openclaw-agent-rv            # 可选
    required: true                        # 默认 false；true 时验签失败即整个流程失败
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `tee` | enum | ✅ | `tdx` |
| `mode` | enum | ✅ | `challenge` 或 `trustee`；Trustee URL 是 CLI 全局运行时配置，不写入 AppSpec 或镜像 |
| `reference_values` | enum | ❌ | 默认 `sample`；可选 `rekor`。若选 `rekor` 则 `attestation.rekor` 必填 |
| `rekor` | object | 条件必填 | `artifact_type` 与 `rekor_url` 非空；`required=true` 时 `cosign_key` 必填 |

可用 CLI 生成本地 Rekor 签名 key，部署机不需要安装 host 版 `cosign`：

```bash
confidential-agent key generate-cosign --output-key-prefix ./cosign
```

两种 reference value 模式的差异：

| 模式 | 来源 | 适用场景 | 失败行为 |
|---|---|---|---|
| `sample` | Shelter build 时本地生成的 sample reference value | 开发自验、QEMU 本地、policy=trustee-opa-local-dev | 验证不过，连接失败 |
| `rekor` | Shelter 产出 in-toto SLSA provenance → 宿主机的 cosign/rekor-cli 签名并推 Rekor → 注册到验证服务 | 生产、对外可证明 | `required=true` 时整链失败；CLI 在 reference-value 阶段会做有限重试 |

Trustee 的全局配置、接管、资源 namespace、重启与销毁语义见 [`docs/trustee.md`](trustee.md)。一个 CLI state directory 对应一个 Trustee，AppSpec 只通过 `mode` 选择 provider。
部署时，CLI 会把该 mode 作为不可变快照写入本地 service state；后续 mesh/A2A 同步和销毁不重新读取可能被移动或修改的 AppSpec，并会在 Trustee 快照与 Trustee state entry 不一致时 fail closed。

---

## 6. `secrets` — Host 持有的敏感物料

```yaml
secrets:
  disk_passphrase: ./secrets/disk_passphrase   # 可选；不写时 CLI 在 secrets_dir 下用 /dev/urandom 自动生成
```

`disk_passphrase` 是 cryptpilot FDE 的可写层密钥。它不会出现在镜像里：challenge 通过直接注入提供，Trustee 先同步到持久 KBS；每次启动时，CryptPilot 先在 Guest initrd 校时，再通过统一 `exec` provider 调用 `confidential-agentd initrd-fetch` 重新证明并取得密钥。密钥落到 `/run/cai/secrets/disk_key`（Guest tmpfs，断电即失），且只有文件原始字节会输出给 CryptPilot 来解锁持久 delta。详见 [`cli/src/app.rs::cryptpilot_fde_config`](../cli/src/app.rs) 与 [`docs/trustee.md`](trustee.md)。

> 实践建议：如果你接 KMS / 自建 HSM，可以让外部脚本提前把密钥写到 `secrets.disk_passphrase` 指向的文件再调 CLI；CLI 不会再覆盖一次。

---

## 7. `resources` — 通过远程证明注入的应用资源

```yaml
resources:
  openclaw_config:                              # key 用作 resource id（仅 [A-Za-z0-9_-]）
    source: ./openclaw.json                     # host 路径
    target: /root/.openclaw/openclaw.json       # 必须绝对路径
    owner: openclaw                             # 可选；优先解析为 uid，再去 /etc/passwd 查
    group: openclaw                             # 可选；同 /etc/group
    mode: "0600"                                # 可选；默认 "0600"
    required: true                              # 默认 true；false 时 daemon 找不到该资源不会 fail
    mutable: false                              # 默认 false；true 表示首次写入后应用可改写内容
```

工作过程：

1. CLI 把每个 resource 文件计算 sha256，作为 bootstrap.json 中 `GuestResource.sha256` 写下来。
2. challenge 通过 `attestation-challenge-client inject-resource` 写入 Guest CDH；Trustee 把同一逻辑路径映射为 `<service-id>/local-resources/<id>` 后同步到 KBS。
3. Guest `confidential-agentd` 监测到 bootstrap 后，把 provider staging 中的资源原子复制到 `target`，并强制校验 sha256 / mode / owner / group（[`daemon/src/app.rs::apply_resource_once`](../daemon/src/app.rs)）。
4. Daemon 校验失败会拒绝写文件并把状态停在 `waiting-resources`。

`mutable: true` 用于应用会在启动后迁移或改写的初始化文件。daemon 仍会校验注入源文件的 sha256，并在首次缺失时写入 `target`；写入成功后只维护目标路径、mode、owner、group，不再因为目标内容被应用改写而覆盖文件或重启 `app_service`。如果应用删除了目标文件，daemon 会按原始注入内容重新 seed。不要把长期必须强一致的 secret 标成 mutable。

这是内容所有权，不是更新开关：默认 `mutable: false` 为“操作者托管”，guest 中的改写会被校正回已交付的内容；`mutable: true` 为“guest 自主管理”，操作者修改 source 并再次注入也不会覆盖已经存在的目标内容。`runtime.json` 通常应由操作者托管；使用原生配置向导维护的文件按需要选择 guest 自主管理。

约束：
- `target` 必须绝对路径。
- 单文件最大 100 MiB（`MAX_RESOURCE_BYTES`）。
- daemon 不允许 sha256 不匹配，不允许覆盖到非常规文件。

---

## 8. `a2a` — 本机 AgentCard 发布

```yaml
a2a:
  id: openclaw-agent              # 必填，写入 CA extension，作为对端 service-directory key
  name: openclaw-agent            # 必填，标准 A2A Agent 名称
  version: "1.0.0"                # 可选
  description: "OpenClaw AI Agent" # 可选
  cacheTtlSec: 300                # 可选，默认 300
  interfaces:                     # 可选；默认从 service.connect 推导 JSONRPC /a2a
    - protocol_binding: JSONRPC
      port: 18789
      path: /a2a
  signing:                        # 可选；required=true 时 deploy/inject 会用 cosign keyless 签卡
    mode: sigstore-keyless
    required: true
    expected_issuer: https://token.actions.githubusercontent.com
    expected_subject: repo:org/repo:ref:refs/heads/main
  skills:
    - id: chat
      name: Chat
      description: "General conversation"
      tags: [chat]
```

| 字段 | 类型 | 必填 | 校验 |
|---|---|:-:|---|
| `enabled` | bool | ❌ | 默认 `true` |
| `id` | string | ✅ | 仅允许 `[A-Za-z0-9_-]` |
| `name` | string | ✅ | 非空 |
| `version` | string | ❌ | — |
| `description` | string | ❌ | — |
| `cacheTtlSec` | u64 | ❌ | 默认 `300`，必须 > 0 |
| `interfaces` | array | ❌ | 默认由 `service.connect[]` 推导；每项 `port` 必须在 `service.connect` 中，`path` 必须以 `/` 开头 |
| `signing` | object | ❌ | `required=true` 时 `mode=sigstore-keyless` 且 `expected_issuer` / `expected_subject` 非空 |
| `skills` | array | ❌ | 默认 `[]`；每项 `id` 必须合法、`name` 非空；可带 `tags` / `examples` / `input_modes` / `output_modes` |

设置 `a2a` 后，CLI 在 `deploy` / `inject` 阶段生成 AgentCard 并注入 guest。daemon 在 `:8089/.well-known/agent-card.json` 发布 AgentCard；`:8088` 只用于 `/status` / `/health`，两者端口和安全组 scope 分离。

AgentCard 按 A2A v1 发布：顶层包含 `protocolVersion`、`supportedInterfaces`、`capabilities`、`skills` 和可选 `signatures`。confidential-agent 扩展放在 `capabilities.extensions[]`，URI 为 `https://confidential-agent.dev/extensions/tee-rekor/v1`，其中包含本机 `publicIp`、`service.connect` 端口、TEE 类型和 Rekor 指针。AgentCard 只发布 `connect` 接入面，不发布 `mesh_ports` / `mcp_ports`，也不携带 gateway client identity。`a2a` 需要 `attestation.reference_values=rekor` 且 `service.connect` 非空，否则没有可公开验证的 reference value metadata 或可公开接入端口，注入会失败。该 AgentCard 形态与旧版 top-level CA extension 不兼容；混合版本部署时应先升级发布 AgentCard 的服务端，再让调用方执行 `a2a add` / `a2a sync`。

**AgentCard 签名**：`a2a.signing.required=true` 时，CLI 通过 宿主机的 `cosign sign-blob` keyless flow 对去除 `signatures` 后的 canonical AgentCard JWS signing input 签名，并把 Sigstore bundle 写入 `signatures[].header`。对端 `a2a add` 可通过 `--signer-issuer` / `--signer-subject` 配置 signer pin；daemon 在配置了 pin 时先用随镜像注入 guest 的 `cosign verify-blob` 验签，再继续 Rekor allowlist 与 RATS-TLS reference value 处理。

`CA_A2A_SIGSTORE_IDENTITY_TOKEN` 可选传给签名流程。设置后，CLI 会把它作为 `cosign sign-blob --identity-token` 使用，适合 CI 中复用 OIDC/JWT，避免 keyless signing 进入交互式登录。`a2a.signing.expected_issuer` / `expected_subject` 描述本服务发布 AgentCard 时预期使用的签名身份；对端消费这个 AgentCard 时，应把同一组值填入 `a2a add --signer-issuer` / `--signer-subject`。

## 9. 外部 Peering 与 A2A 连接

跨组织接入不再写进 spec。网络入向授权由 `<state-dir>/peerings.yaml` 管理，协议层 desired state 由 `<state-dir>/a2a.json` 管理。

`a2a.json` 当前版本为 `2`。新 CLI 不再自动迁移旧版 `version: 1` state；读取到旧 state 会直接失败。没有存量服务时应删除旧 `a2a.json` 后重新执行 `a2a add`，不要让新旧 CLI 混用同一个 `<state-dir>`。

### 9.1 `peering` — 入向网络授权

```bash
confidential-agent build --spec confidential-agent.yaml
confidential-agent peering add --role operator --cidr <ops-cidr>/32 --label ops
confidential-agent deploy --spec confidential-agent.yaml
confidential-agent peering add --role peer --cidr <peer-vm-ip>/32 --label beta
confidential-agent peering apply
```

`peerings.yaml` 示例：

```yaml
version: 1
peerings:
  - label: ops
    role: operator
    cidr: 203.0.113.10/32
  - label: beta
    role: peer
    cidr: 198.51.100.20/32
```

默认 scope：

| role | 默认 scope |
|---|---|
| `operator` | `control`, `status`, `ssh`, `agent_card`, `connect` |
| `peer` | `agent_card`, `connect` |

可以用 `--scope control,status` 显式收窄，也可以显式加入 `mesh` 来开放 `mesh_ports`。`peering add/remove` 只改本地 state，不自动改云上安全组；`peering apply` 对所有 active services 重新渲染并应用安全组。

`deploy` / `inject` 会 fail-fast 检查是否存在包含 `control+status` 的 operator peering，并尽量确认当前机器出口 IP 被 `control` scope 覆盖。特殊网络下可用 `--skip-peering-check` 跳过。

### 9.2 `a2a` — 外部 AgentCard desired state

```bash
confidential-agent a2a add --alias beta \
  --signer-issuer https://token.actions.githubusercontent.com \
  --signer-subject repo:org/repo:ref:refs/heads/main \
  http://198.51.100.20:8089/.well-known/agent-card.json
confidential-agent a2a list
confidential-agent a2a sync --all
confidential-agent a2a remove beta
```

`a2a add` 至少需要一个 AgentCard URL。peer 的 public IP、`connect` 端口、Rekor metadata 都由 daemon 从 AgentCard 获取；本地不再重复填写 peer 端口/CIDR/reference values。生产跨组织接入应同时提供 signer pin；未提供 pin 时只执行 v1 schema、publicIp 和 Rekor trust 校验。

CLI 会 best-effort 拉取 AgentCard 做预览；失败不会阻塞，因为真实场景里对端可能只允许本服务 VM 访问 `:8089`，不允许 operator 笔记本访问。CLI 会把失败分类记录为 `unreachable` / `unsigned` / `signature_failed` / `host_mismatch` / `rekor_untrusted` / `invalid`，供 `a2a list` 和 `a2a show` 查看。daemon 侧拉取是权威结果，会按 AgentCard `cacheTtlSec` 刷新，并写入 `status --live` 的 `a2a_peers`。`cacheTtlSec` 会在 guest 本地 clamp 到 60..3600 秒。

daemon 接收 `cagent_a2a_bundle` 后会：

1. 拉取并校验 AgentCard：URL path 固定、content-type 为 JSON、body 有大小上限。
2. 如 peer 配置了 signer pin，校验 `signatures[]` 中至少一个 Sigstore keyless 签名满足 issuer/subject pin。
3. 校验 `capabilities.extensions[]` 中的 CA extension 存在，且 `publicIp` 与 URL host 的 IPv4 解析结果一致。
4. 校验 `rekorUrl` 落在 trusted Rekor allowlist，默认只信任 `https://rekor.sigstore.dev`，可用 `CA_TRUSTED_REKOR_URLS` 扩展。
5. 用 AgentCard 中的 Rekor metadata 生成 TNG reference values。
6. 为 AgentCard 声明的 connect 端口生成本地 TNG ingress，并写入 `/etc/cai/service-directory.json`；该路径不经过 gateway，也不携带 gateway client identity。

A2A 按 `connect` 模型处理：本地调用方验证对端服务端 TEE，不要求对端显式 `a2a add` 本服务，也不会触发 MCP gateway parse/audit/virtual tools；如果业务需要双向应用调用，双方各自添加一条出向 A2A desired state。

`status --live` 中 A2A peer 状态含义：

| state | 含义 |
|---|---|
| `ok` | 最近一次 AgentCard 拉取和 TNG ingress 生成成功 |
| `stale` | 当前拉取遇到 transport/HTTP 5xx 这类临时可达性失败，但 daemon 仍有上一份成功 AgentCard cache，并继续使用旧 ingress |
| `error` | 没有可用 AgentCard cache，或 schema/trust/signature 校验失败，或 peer id 与本地 service-directory 冲突 |

`stale` 不覆盖 AgentCard 已返回但校验失败的情况，也不覆盖 HTTP 4xx、host 解析失败、body 超限或 content-type 非 JSON；`publicIp`、`rekorUrl`、signer pin 或 schema 错误会直接进入 `error`，即使此前有旧 cache。

OpenClaw A2A 插件不再从配置里读取对端 URL/端口，而是从 `/etc/cai/service-directory.json` 解析 peer alias/card id；配置里只需要保留 token 或 `defaultPeerToken`。

## 10. 路径解析规则

所有路径字段（`build.scripts[]`, `build.files[].source`, `build.variants.debug.ssh_public_key`, `secrets.disk_passphrase`, `resources.*.source`, `attestation.rekor.cosign_key`, `attestation.rekor.slsa_generator`）都遵循：

1. 如果是绝对路径 → 直接使用。
2. 如果是相对路径 → 相对于 **spec YAML 所在目录** 解析。
3. `..` 与 `.` 会被规范化（[`normalize_path`](../core/src/spec.rs)）。

---

## 11. 完整示例

最小可运行 spec（参见 [`examples/openclaw/openclaw.yaml`](../examples/openclaw/openclaw.yaml)）：

```yaml
schema: confidential-agent/v1

service:
  id: openclaw
  ports: [18789]
  connect: [18789]
  app_service: cai-openclaw-gateway.service

build:
  image_name: openclaw-agent
  resize: 30G
  packages: [ca-certificates, curl, git, jq, nodejs, npm, podman, tar, xz]
  files:
    - source: ../../target/debug/cai-pep
      target: /usr/local/bin/cai-pep
      executable: true
  scripts:
    - ./install-openclaw.sh
  variants:
    release: { enabled: true }
    debug:   { enabled: true }

deploy:
  provider: aliyun
  image_variant: release
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
  disk_gb: 200

attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
  rekor:
    cosign_key: ./cosign.key
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    required: true

resources:
  openclaw_config:
    source: ./openclaw.json
    target: /root/.openclaw/openclaw.json
    mode: "0600"
    required: true
```

---

## 12. 衍生 schema（只读）

下面这些 schema 由 CLI / daemon 生成，**不要手写**。它们是 spec → runtime 的中间产物。

| Schema 字符串 | 来源 | 生成位置 | 消费位置 |
|---|---|---|---|
| `confidential-agent/v1` | AgentSpec | 用户 | CLI |
| `confidential-agent/service-state/v1` | LocalServiceState | CLI | CLI（`status`/`mesh sync`/`destroy`） |
| `confidential-agent/bootstrap/v1` | BootstrapConfig | CLI（[`render_bootstrap`](../cli/src/app.rs)） | Guest daemon |
| `confidential-agent/mesh-bundle/v1` | MeshBundle | CLI（[`render_mesh_bundle`](../cli/src/app/workflows.rs)） | Guest daemon |
| `confidential-agent/services/v1` | ServiceDirectory | Guest daemon | Guest TNG / 应用 |
| `confidential-agent/daemon-status/v1` | DaemonStatus | Guest daemon `:8088` | CLI `status --live` |
| `confidential-agent/agent-card/v1` | AgentCard | CLI → BootstrapConfig → Guest daemon `:8089` | 跨组织 peer agent |

如果手动编辑了 state 文件，注意保持 schema 字符串完全匹配——CLI 会显式校验，不一致直接拒绝。

Published CA images carry an authenticated `confidential-agent/image-runtime/v1`
contract. CA checks guest/bootstrap capabilities, root integrity, encrypted
writable storage with CA key release, minimum disk size, application service and
instance SSH support before creating infrastructure. `build.runtime_contract`
optionally selects an image-owned JSON contract; it is captured at build time.
The guest binary reports its protocol capabilities through `describe-runtime`.
Older image exports without this contract require rebuilding before using the
published-image entry point; ordinary local artifact deployment remains available.

`ca deploy --resume` resumes resource delivery only for confirmed `deployed`
infrastructure with the original configuration at its canonical path. Deployment
records canonical resource paths and digests and delivers private snapshots of
user resources and the disk secret. Resume rejects changed resource contents,
retargeted paths, or changed instance credentials; restore the original inputs
before resuming. Run the candidate host CLI's `upgrade check` against the existing
state directory before upgrading or rebooting; one-click performs this check
before replacing installed binaries. Legacy deployments without frozen inputs
require explicit `upgrade bind-inputs --service SERVICE`. This reads unchanged
committed configuration, verifies disk bytes against historical Trustee disk/data
digests, verifies recorded gateway and SSH identities, and saves a private input
snapshot without changing remote resources. A missing or conflicting historical
binding is not replaced by current files. Legacy challenge deployments without
disk evidence must retain the previous host toolchain for resource delivery and
back up/export data before migration; destroying data-bearing instances is not
an upgrade procedure. Resume does not apply Terraform or
replay an interrupted infrastructure operation. Same-service build/deploy/inject/
destroy operations are mutually exclusive; independent services may proceed in
parallel. `ca ssh --tty SERVICE -- COMMAND` explicitly allocates a terminal;
script execution retains its existing output and exit-code behavior.

Ordinary `inject` permits moving unchanged secret files and project directories.
It compares secret contents and instance identity, uses saved release/public-key
authorization and the committed disk copy, and never applies build or infrastructure
changes. Supplied release/key files that still exist must match saved copies;
their original download locations need not remain available. `--resume` retains
its stricter original-path requirement.

If persistent guest daemon state is unreadable, the daemon enters
`management-recovery`: it stops business and mesh services, leaves endpoint state
untouched, and only installs provider-specific, hash-verified management credentials.
Failed stop requests are reported without withholding authenticated SSH. The daemon
does not start or reconfigure business services during recovery. Instance SSH can
restore the original state after a cold boot.
Status reports both application and mesh unready. Restoring valid state lets the
daemon resume with the saved endpoint reservations.

发布元数据不绑定部署账号。`deploy.image.image_id` 可覆盖地域中的镜像位置（共享或云市场镜像），不改变参考值。
默认校验 release 签名；显式设置 `deploy.image.skip_signature_verification: true` 或传 `deploy --skip-signature-verification` 后可省略 `trusted_key`。
该选项不关闭运行时证明和资源释放策略。已有 UID 字段兼容读取并忽略。
