# AppSpec 参考（`confidential-agent/v1`）

AppSpec 是一份 YAML，描述一个服务“构建什么镜像、部署到哪里、如何证明、注入哪些资源”。本文列出全部字段、默认值和校验规则。第一次编写 AppSpec 建议先读 [打包自定义 Agent](../guides/custom-agent.md)。

随时可以用 CLI 校验：

```bash
confidential-agent spec validate --spec my-agent.yaml               # 解析、字段约束、本地路径是否存在、安装脚本常见问题
confidential-agent spec validate --spec my-agent.yaml --format json
confidential-agent spec schema                                      # 字段速查
```

多数子命令的 `--spec` 默认值是当前目录下的 `confidential-agent.yaml`。

## 目录

1. [顶层结构](#1-顶层结构)
2. [`service`：服务身份与端口](#2-service服务身份与端口)
3. [`build`：镜像构建](#3-build镜像构建)
4. [`deploy`：云上部署](#4-deploy云上部署)
5. [`attestation`：远程证明](#5-attestation远程证明)
6. [`secrets`：部署机持有的机密](#6-secrets部署机持有的机密)
7. [`resources`：远程证明后注入的文件](#7-resources远程证明后注入的文件)
8. [`a2a`：发布 AgentCard](#8-a2a发布-agentcard)
9. [路径解析规则](#9-路径解析规则)
10. [完整示例](#10-完整示例)
11. [不再写在 AppSpec 中的配置](#11-不再写在-appspec-中的配置)

## 1. 顶层结构

```yaml
schema: confidential-agent/v1   # 固定值
service:     { ... }            # 必填
build:       { ... }            # 本地构建时必填；使用已发布镜像（deploy.image）时必须省略
deploy:      { ... }            # 必填
attestation: { ... }            # 必填
secrets:     { ... }            # 可选
resources:   { ... }            # 必填，没有资源时写 {}
a2a:         { ... }            # 可选
```

| 字段 | 必填 | 说明 |
|---|:-:|---|
| `schema` | ✅ | 必须等于 `confidential-agent/v1` |
| `service` | ✅ | 见 [§2](#2-service服务身份与端口) |
| `build` | 条件 | 本地构建时必填，与 `deploy.image` 互斥，见 [§3](#3-build镜像构建) |
| `deploy` | ✅ | 见 [§4](#4-deploy云上部署) |
| `attestation` | ✅ | 见 [§5](#5-attestation远程证明) |
| `secrets` | ❌ | 默认 `{}`，见 [§6](#6-secrets部署机持有的机密) |
| `resources` | ✅ | 必须显式写出，可以是 `{}`，见 [§7](#7-resources远程证明后注入的文件) |
| `a2a` | ❌ | 见 [§8](#8-a2a发布-agentcard) |

**严格模式**：任何层级出现未知字段都会直接解析失败，避免拼错的安全字段被悄悄忽略。

## 2. `service`：服务身份与端口

```yaml
service:
  id: openclaw                               # 服务 ID
  ports: [18789]                             # 应用在 Guest 内实际监听的全部业务端口
  connect: [18789]                           # 其中允许部署机 connect / A2A 访问的端口
  mcp_ports: []                              # 其中按 MCP 协议审计的端口（必须是 mesh 端口）
  app_service: cai-openclaw-gateway.service  # 判断应用就绪的 systemd unit
  ssh: false                                 # 是否启用实例级管理 SSH
```

| 字段 | 类型 | 必填 | 默认 | 校验 |
|---|---|:-:|---|---|
| `id` | string | ✅ | — | 只允许 `[A-Za-z0-9_-]`，非空。Trustee 模式下还要求长度 ≤ 64 且不能为 `default` |
| `ports` | `[u16]` | ✅ | — | 可以为空；不能含 0，不能重复 |
| `connect` | `[端口]` | ❌ | `[]` | 每项必须在 `ports` 中，不能重复。端口可写成整数，或 `{port: 8080, encryption: ohttp}` |
| `mcp_ports` | `[u16]` | ❌ | `[]` | 每项必须在 `ports` 中且**不在** `connect` 中 |
| `app_service` | string | ❌ | — | 设置时不能为空 |
| `ssh` | bool | ❌ | `false` | 只能用于 release 镜像 |

### 三类端口

`service.ports` 是应用实际监听端口的全集，TNG 只为这些端口建立入口；CA 不会额外引入业务端口。它被划分为两部分：

| 端口类别 | 来源 | 谁能访问 | 证明方向 |
|---|---|---|---|
| **connect 端口** | `service.connect` | 部署机 `connect` 隧道、跨组织 A2A 调用方 | 单向：调用方验证本服务 |
| **mesh 端口** | `ports - connect`（自动计算，不需要填写） | 同一状态目录下其他通过证明的服务 | 双向：双方都要通过证明 |

`mcp_ports` 是 mesh 端口的子集。Guest 内的 `cai-gateway` 只接管 mesh 端口：

- 普通 mesh 端口只校验调用方的服务身份，流量按原始 TCP 透传；
- `mcp_ports` 中的端口还会解析 MCP JSON-RPC，记录哈希链审计，并在 `tools/list` 中注入 `tee_attest`、`audit_status`、`audit_verify` 三个虚拟工具。详见 [MCP 网关](../design/mcp-gateway.md)。

connect 和 A2A 流量不经过 `cai-gateway`。**敏感接口只应放在 mesh 端口上，不要放进 `connect`。**

**多个服务使用相同端口**：同一状态目录下的服务可以监听相同的端口号（例如两个 OpenClaw 都用 18789）。前提是所有 mesh 成员的镜像都支持稳定的 mesh 客户端端点（运行时契约能力 `mesh-client-ports-v2`），否则 `deploy` 会报 `uses a legacy guest without stable mesh client endpoints`，需要先用新版重建这些镜像。

`ports: []` 表示这是只调用别人的 client/job 类服务：它不暴露入站端口，但仍会获得网关身份、进入 mesh，并能通过 `/etc/cai/service-directory.json` 访问其他服务。

### connect 端口加密方式

`connect` 中的端口默认使用 RATS-TLS（证明 + TLS）。写成对象形式可改用 OHTTP（基于 HPKE 的信封加密，适合负载均衡后的多副本服务）：

```yaml
service:
  ports: [8080]
  connect:
    - port: 8080
      encryption: ohttp     # rats-tls（默认）| ohttp
```

### `app_service`

设置后，`confidential-agentd` 会启动并检查该 systemd unit，同时确认 `service.ports` 都能在 Guest 本地连接，`status --live` 中的 `app_ready` 才会变为 true。不设置时，`app_ready` 只代表资源和 TNG 已就绪，并不证明应用已经启动。

### `ssh`：实例级管理 SSH

`ssh: true` 时，部署端为每个实例生成独立的 Ed25519 客户端密钥和主机密钥，经远程证明下发到 Guest 的 `/opt/data/cai-ssh/authorized_keys` 和 `/opt/data/cai-ssh/host_key`；安全组只向 peering 中带 `ssh` scope 的 CIDR 开放 22 端口。镜像需要自带一个使用这两个文件的 SSH 服务，并在资源就绪后启动它（参见 [allinone 示例](../../examples/allinone/README.md)）。

- `confidential-agent ssh <service> -- <command>` 使用对应私钥连接，并用固定的主机公钥校验对端。
- 私钥只保存在部署机状态目录，不进入镜像或发布元数据。
- 与 debug 镜像的构建期 SSH 互斥：`ssh: true` 不能与 `deploy.image_variant: debug` 同时使用。

## 3. `build`：镜像构建

CA 通过 Shelter 调用 mkosi 构建 Guest 根文件系统，生成 UKI 并启用 CryptPilot 全盘加密。构建过程中安装脚本可以访问网络。

```yaml
build:
  image_name: openclaw-agent                   # 构建产物名，也是云镜像名前缀
  resize: 30G                                  # 镜像虚拟磁盘大小
  qcow2_compression: zlib                      # 压缩最终 QCOW2
  kernel_cmdline_append: "swiotlb=4194304,any" # 追加到 UKI 内核参数
  runtime_contract: ./runtime-contract.json    # 镜像运行时契约（发布可复用镜像时使用）
  repos:                                       # 额外的签名 RPM 源
    - name: epel
      baseurl: https://mirrors.aliyun.com/epel/8/Everything/$basearch/
      gpgkey: https://mirrors.aliyun.com/epel/RPM-GPG-KEY-EPEL-8
  packages: [ca-certificates, curl, nodejs, npm]
  files:
    - source: ./files/app.conf
      target: /etc/my-agent/app.conf
  scripts:
    - ./install-my-agent.sh
  cleanup:
    remove_static_libs: false
  container: { ... }                           # 可选：OCI 工作负载，见 §3.2
  variants:
    release: { enabled: true }
    debug:   { enabled: true }
```

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `image_name` | string | — | **必填**（本地构建时）。只允许 `[A-Za-z0-9_-]` |
| `resize` | string | — | 例如 `30G`，对应 Shelter `output.disk_size` |
| `qcow2_compression` | string | — | 目前只支持 `zlib` |
| `kernel_cmdline_append` | string | — | 追加到 UKI 内核命令行（对应 `security.disk.cryptpilot.uki_append_cmdline`） |
| `runtime_contract` | path | — | 镜像运行时契约（`confidential-agent/image-runtime/v1`）：声明协议能力、根文件系统完整性、可写层加密、最小磁盘、`app_services` 等。构建时记录，`image export` 时随发布描述一起认证，部署端部署前由 CA 校验。示例见 [`examples/allinone/runtime-contract.json`](../../examples/allinone/runtime-contract.json) |
| `repos` | `[object]` | `[]` | 额外 RPM 源。每项含唯一的 `name`、HTTPS `baseurl` 和 HTTPS `gpgkey`，保持签名校验 |
| `packages` | `[string]` | `[]` | 通过 Shelter 安装的系统包，使用 AliLinux/RHEL 包名，尽量精简 |
| `files` | `[BuildFileSpec]` | `[]` | 复制进镜像的文件，见 §3.1 |
| `scripts` | `[path]` | `[]` | 部署机上的脚本路径，按顺序在镜像构建的 post-install 阶段执行（在 CA 自身的 Guest 配置脚本之后） |
| `cleanup.remove_static_libs` | bool | Shelter 默认 | 设为 `false` 保留 `*.a` 静态库，适合运行期需要编译 Python 扩展的镜像（如 vLLM） |
| `container` | object | — | Shelter OCI 工作负载，见 §3.2 |
| `variants` | object | 只启用 release | 见 §3.3 |

注意：

- `build.scripts` 写的是**部署机上的脚本路径**（例如 `./install.sh`），不是 Guest 内的 `files[].target`。
- 安装脚本中用到 `curl`、`git`、`tar` 等命令时，相应软件包要列在 `packages` 中，`spec validate` 会检查这类常见遗漏。
- 已移除的字段 `build.base_image`、`build.with_network` 会被拒绝，请删除。

### 3.1 `BuildFileSpec`

```yaml
- source: ./relative/or/abs.path    # 必填，部署机上的路径
  target: /absolute/path/in/guest   # 必填，必须以 / 开头
  executable: false                 # 默认 false
```

### 3.2 `container`：OCI 工作负载

`container` 原样渲染为 Shelter 顶层 `container:`。它是镜像中的一个工作负载，不能替代 mkosi 构建的基础系统。

```yaml
container:
  image: docker.io/org/app:tag     # image 与 archive 至少填一个
  archive: ./app.oci.tar           # 本地 OCI 归档
  mode: runtime                    # runtime（默认，容器运行时启动）| rootfs（展开进根文件系统）
  runtime: containerd              # runtime 模式使用：containerd（默认）| podman
  name: app                        # 只允许 ASCII 字母、数字、.、_、-
  pull: missing                    # missing（默认）| always | never；使用 archive 时只能 missing
  command: ["/usr/local/bin/app"]  # 默认使用镜像的 Entrypoint + Cmd
  env:
    APP_ENV: production            # key 必须是合法的 shell 变量名
  working_dir: /srv/app            # 必须是绝对路径
  network: true                    # 是否需要网络
  mounts:                          # 仅 runtime 模式
    - source: /var/lib/app/data    # Guest 上的绝对路径
      target: /opt/data            # 容器内绝对路径
      read_only: false
```

需要把运行时资源挂进容器时，先在顶层 `resources` 中把文件注入到 Guest 路径，再用 `mounts` 把该路径挂载进容器。使用 `container.image` 构建时，部署机需要 Podman 或 Docker。

### 3.3 `variants`：release 与 debug

```yaml
variants:
  release:
    enabled: true                  # 默认 true
  debug:
    enabled: true
    ssh_public_key: ./debug.pub    # 可选，只允许出现在 debug 中
```

- release 镜像不允许任何 SSH 入口，`release.ssh_public_key` 会导致解析失败。
- 一次 `build` 会构建所有 `enabled: true` 的变体，`deploy.image_variant` 选择部署哪一个。选择的变体未启用时报错。
- 部署 debug 镜像且未设置 `ssh_public_key` 时，CLI 会自动在 `<state-dir>/services/<id>/secrets/debug_ssh{,.pub}` 生成密钥对，部署完成后打印 `ssh -i ... root@<ip>` 命令。

## 4. `deploy`：云上部署

```yaml
deploy:
  provider: aliyun
  image_variant: release       # release（默认）| debug
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i        # 也可写作 availability_zone
  disk_gb: 120
  vpc_id: vpc-xxx              # 以下均可选，留空则自动创建
  vswitch_id: vsw-xxx
  security_group_id: sg-xxx
  private_ip: 10.0.1.20
  replicas: 1
  slb: { enabled: false, slb_type: nlb }
  image: { ... }               # 使用已发布镜像时填写，见 §4.2
```

| 字段 | 类型 | 必填 | 默认 | 说明 |
|---|---|:-:|---|---|
| `provider` | enum | ✅ | — | 目前只支持 `aliyun` |
| `instance_type` | string | ✅ | — | 支持 TDX 的 ECS 规格，如 `ecs.g9i.xlarge`、`ecs.g8i.xlarge`、`ecs.gn8v-tee.4xlarge` |
| `region` | string | ✅ | — | 地域 |
| `zone_id` | string | ✅ | — | 可用区，别名 `availability_zone` |
| `image_variant` | string | ❌ | `release` | `release` 或 `debug` |
| `disk_gb` | u32 | ❌ | 见下 | 系统盘大小（GiB） |
| `vpc_id` / `vswitch_id` / `security_group_id` | string | ❌ | 自动创建 | 复用已有网络资源 |
| `private_ip` | string | ❌ | — | 指定内网 IP |
| `replicas` | u32 | ❌ | `1` | 副本数，必须 > 0 |
| `slb.enabled` | bool | ❌ | `false` | 多副本前端负载均衡 |
| `slb.slb_type` | enum | ❌ | `nlb` | `nlb`（四层）或 `alb`（七层） |
| `image` | object | ❌ | — | 使用已发布镜像，见 §4.2 |

**磁盘大小**：未设置时默认 32 GiB，并会按 `build.resize` 向上调整；显式设置时不能小于 20 GiB，也不能小于镜像容量。使用已发布镜像时必须显式设置，并满足镜像的最低容量。

**多副本约束**：

- `replicas > 1` 必须同时设置 `slb.enabled: true`（TNG 没有客户端负载均衡）；反之启用 SLB 也要求 `replicas > 1`。
- 多副本服务目前最多支持一个 OHTTP connect 端口。
- `slb_type: alb`（七层）与 RATS-TLS（四层 TLS）不兼容，使用 ALB 时 connect 端口必须使用 `encryption: ohttp`。
- 同一服务的副本共享身份和下发的凭据，各自拥有独立磁盘。副本数在部署时确定。

**常用地域组合**：示例默认 `cn-beijing` + `cn-beijing-i` + `ecs.g9i.xlarge`；香港可用 `cn-hongkong` + `cn-hongkong-d` + `ecs.g8i.xlarge`；GPU TEE 常用 `cn-beijing-l` + `ecs.gn8v-tee.4xlarge`。实际可用性受账号库存和可用区影响。

### 4.1 安全组规则

入向安全组规则由 peering 和服务端口自动生成，不需要也不能在 AppSpec 中填写（见 [运维手册 · 网络授权](../guides/operations.md#网络授权peering)）：

| 规则名 | 端口 | 来源 | 条件 |
|---|---|---|---|
| `control_8006_peer_<cidr>` | 8006 | 含 `control` scope 的 CIDR | 始终 |
| `status_8088_peer_<cidr>` | 8088 | 含 `status` scope 的 CIDR | 始终 |
| `agent_card_8089_peer_<cidr>` | 8089 | 含 `agent_card` scope 的 CIDR | 始终 |
| `ssh_22_peer_<cidr>` | 22 | 含 `ssh` scope 的 CIDR | debug 镜像或 `service.ssh: true` |
| `connect_<port>_peer_<cidr>` | `service.connect` | 含 `connect` scope 的 CIDR | — |
| `mesh_<port>_peer_<cidr>` | mesh 端口 | 同状态目录其他活跃服务的公网 IP `/32`，或含 `mesh` scope 的 CIDR | — |

### 4.2 `image`：使用已发布镜像

部署端可以不构建镜像，而是引用一个已发布镜像的描述文件。此时必须省略整个 `build` 段（包括 `variants.debug`）。

```yaml
deploy:
  provider: aliyun
  region: cn-beijing
  zone_id: cn-beijing-i
  instance_type: ecs.g9i.xlarge
  disk_gb: 120
  image:
    release: ./image-release.json
    release_digest: sha256:<image-release.json 整个文件的 64 位 SHA-256>
    image_id: m-xxxxxxxx                       # 可选
    manifest: sha256:<64 位 manifest 摘要>     # 可选
```

| 字段 | 必填 | 说明 |
|---|:-:|---|
| `release` | ✅ | `image-release.json` 路径 |
| `release_digest` | ✅ | 完整的 `sha256:<64 位十六进制>`。来自官方 Release 的 `image-index.json`，或本机 `image export` 的输出 |
| `image_id` | ❌ | 当前地域可用的 ECS 镜像 ID（自定义、共享或云市场镜像）。省略时，CA 按 manifest 摘要、账号和地域查找本状态目录 `image publish` 的记录 |
| `manifest` | ❌ | 额外固定 manifest 版本；默认从已认证的描述文件读取 |

信任关系分两层，互相独立：

1. `release_digest` 保证描述文件和导入产物的字节没有被替换；
2. 运行时远程证明（TDX quote 加上描述文件中的 Rekor 或 sample 参考值）在下发磁盘密钥和应用资源之前**始终强制执行**。

使用已发布镜像时，部署端不需要生产端的状态目录、构建输入或本地 QCOW2。每个实例拥有独立的 `service.id`、磁盘口令、网关身份、管理 SSH 密钥和 Terraform 状态，`destroy` 不会删除共享镜像。部署前，CA 还会校验镜像运行时契约：Guest 协议能力、根文件系统完整性、可写层加密、最小磁盘、`app_service` 以及是否支持管理 SSH。

已发布镜像也可以是 debug 变体（描述文件的运行时元数据须标记 `image_variant: debug`）。debug 镜像内置的是构建时的 SSH 公钥，各部署端不会获得或重新生成对应私钥；需要 SSH 时，由操作员单独提供原始私钥：

```bash
confidential-agent ssh debug-consumer --identity-file /private/original-debug-key -- uptime
```

如何生成和导入已发布镜像，见 [手动部署 · 准备云镜像](../guides/manual-deployment.md#2-准备云镜像)。

## 5. `attestation`：远程证明

```yaml
attestation:
  tee: tdx
  mode: challenge                  # challenge | trustee
  reference_values: rekor          # sample（默认）| rekor
  rekor:                           # reference_values=rekor 且本地构建时必填
    artifact_id: openclaw-agent-release
    artifact_type: uki
    artifact_version: "20260430"
    rekor_url: https://rekor.sigstore.dev
    cosign_key: ./cosign.key
    slsa_generator: /usr/libexec/shelter/slsa/slsa-generator
    rv_name: measurement.uki.SHA-384
    required: true
```

| 字段 | 必填 | 默认 | 说明 |
|---|:-:|---|---|
| `tee` | ✅ | — | 目前只支持 `tdx` |
| `mode` | ✅ | — | `challenge`：部署机直接注入机密；`trustee`：由独立 Trustee 托管机密，见 [Trustee 模式](../guides/trustee.md) |
| `reference_values` | ❌ | `sample` | 参考值来源，见下表 |
| `rekor` | 条件 | — | 本地构建且 `reference_values: rekor` 时必填。使用 `deploy.image` 时从已认证的描述文件读取，不需要填写 |

`rekor` 子字段：

| 字段 | 默认 | 说明 |
|---|---|---|
| `artifact_id` | 构建 ID | Rekor 中的制品标识 |
| `artifact_type` | `uki` | 非空 |
| `artifact_version` | — | 发布方自定义的版本字符串 |
| `rekor_url` | `https://rekor.sigstore.dev` | 非空 |
| `cosign_key` | — | 签名私钥。`required: true` 时必填 |
| `slsa_generator` | `/usr/libexec/shelter/slsa/slsa-generator` | SLSA provenance 生成器 |
| `rv_name` | — | 参考值名称。Trustee 默认策略读取 `measurement.uki.SHA-384` |
| `required` | `false` | `true` 时签名或上传失败会让整个流程失败（CLI 会有限次重试） |

两种参考值模式：

| 模式 | 来源 | 适用 |
|---|---|---|
| `sample` | 构建时本地生成的参考值 | 开发自测 |
| `rekor` | Shelter 生成 SLSA provenance，由部署机 cosign 签名后写入 Rekor，再注册到验证服务 | 生产、需要第三方可审计时 |

生成 Rekor 签名密钥（部署机无需另装 cosign）：

```bash
confidential-agent key generate-cosign --output-key-prefix ./cosign   # 生成 ./cosign.key 与 ./cosign.pub
```

部署时，CLI 把 `mode` 作为不可变快照写入本地服务状态。后续的 mesh/A2A 同步和销毁都不会重新读取（可能已被修改的）AppSpec。

## 6. `secrets`：部署机持有的机密

```yaml
secrets:
  disk_passphrase: ./secrets/disk_passphrase   # 可选
```

`disk_passphrase` 是加密可写层的口令。不填时，CLI 在服务的 `secrets/` 目录下用随机数自动生成；文件已存在时不会覆盖。因此，接入 KMS/HSM 时可以先由外部脚本把口令写到这个路径，再调用 CLI。

口令永远不会进入镜像：challenge 模式由部署机注入，Trustee 模式预先同步到 KBS。每次启动时，Guest 在 initrd 中重新完成证明并取回口令，写到 tmpfs 中的 `/run/cai/secrets/disk_key`，只交给 CryptPilot 解锁可写层。

## 7. `resources`：远程证明后注入的文件

```yaml
resources:
  openclaw_config:                         # 资源 ID，只允许 [A-Za-z0-9_-]
    source: ./openclaw.json                # 部署机上的文件
    target: /root/.openclaw/openclaw.json  # Guest 内绝对路径
    owner: openclaw                        # 可选：用户名或 uid
    group: openclaw                        # 可选：组名或 gid
    mode: "0600"                           # 默认 "0600"
    required: true                         # 默认 true
    mutable: false                         # 默认 false
```

| 字段 | 默认 | 说明 |
|---|---|---|
| `source` | — | **必填**，部署机路径，单个文件最大 100 MiB |
| `target` | — | **必填**，Guest 内的绝对路径，不能指向非普通文件 |
| `owner` / `group` | — | 先按数字解析，否则查 `/etc/passwd`、`/etc/group` |
| `mode` | `"0600"` | 文件权限 |
| `required` | `true` | `true` 时资源缺失会让 Guest 停在 `waiting-resources` |
| `mutable` | `false` | 内容归谁管理，见下 |

工作过程：

1. CLI 计算每个资源的 sha256，写入 bootstrap 配置；
2. challenge 模式经远程证明注入 Guest；Trustee 模式把资源同步到 KBS 的 `<service-id>/local-resources/<id>`；
3. Guest 守护进程把资源原子写入 `target`，并强制校验 sha256、权限、属主；校验失败则拒绝写入，状态停在 `waiting-resources`。

**`mutable` 决定内容归谁管理，而不是更新开关：**

- `false`（默认，操作者托管）：Guest 内对文件的修改会被校正回已下发的内容。
- `true`（Guest 自主管理）：只在目标文件不存在时写入一次；之后应用可以自由改写，守护进程只维护路径、权限和属主，不会覆盖内容或重启 `app_service`。操作者修改 `source` 再次注入也不会覆盖已有内容；应用删除文件后会按原始内容重新写入。

适合标为 `mutable` 的是应用启动后会自己迁移或改写的初始化配置（例如 Hermes 的 `config.yaml`）。必须长期保持一致的机密不要标成 `mutable`。

## 8. `a2a`：发布 AgentCard

设置后，CLI 在 `deploy` / `inject` 时生成 AgentCard 并注入 Guest，守护进程在 `:8089/.well-known/agent-card.json` 发布它。应用负责提供卡片声明的接口。OpenClaw 通过 CA 扩展发现对端，应用层调用走 `/v1/responses`。见 [A2A 指南](../guides/a2a.md)。

```yaml
a2a:
  id: openclaw-agent                # 对端 service-directory 中的 key
  name: openclaw-agent
  version: "1.0.0"
  description: "OpenClaw AI Agent"
  cacheTtlSec: 300
  interfaces:
    - protocol_binding: JSONRPC
      port: 18789
      path: /a2a
  signing:
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

| 字段 | 必填 | 默认 | 说明 |
|---|:-:|---|---|
| `enabled` | ❌ | `true` | |
| `id` | ✅ | — | 只允许 `[A-Za-z0-9_-]` |
| `name` | ✅ | — | 非空 |
| `version` / `description` | ❌ | — | |
| `provider` | ❌ | — | 原样写入 AgentCard 的 `provider` 对象 |
| `cacheTtlSec` | ❌ | `300` | 必须 > 0；对端会限制在 60–3600 秒 |
| `interfaces` | ❌ | 由 `service.connect` 推导为 `JSONRPC` + `/a2a` | 每项 `port` 必须在 `service.connect` 中，`path` 以 `/` 开头 |
| `signing` | ❌ | 不签名 | `required: true` 时要求 `mode: sigstore-keyless` 且 `expected_issuer`、`expected_subject` 非空；可选 `oidc_issuer` 指定 keyless 签名使用的 OIDC 提供方 |
| `skills` | ❌ | `[]` | 每项需要合法的 `id` 和非空 `name`，可带 `tags`、`examples`、`input_modes`、`output_modes` |

前置要求：`attestation.reference_values: rekor`，且 `service.connect` 非空。AgentCard 只公开 connect 端口，不包含 mesh/MCP 端口。

## 9. 路径解析规则

以下字段中的路径：`build.runtime_contract`、`build.scripts[]`、`build.files[].source`、`build.container.archive`、`build.variants.debug.ssh_public_key`、`deploy.image.release`、`secrets.disk_passphrase`、`resources.*.source`、`attestation.rekor.cosign_key`、`attestation.rekor.slsa_generator`，

- 绝对路径直接使用；
- 相对路径**相对于 AppSpec 文件所在目录**解析；
- `.` 和 `..` 会被规范化。

## 10. 完整示例

以下与仓库中的 [`examples/openclaw/openclaw.yaml`](../../examples/openclaw/openclaw.yaml) 结构一致（为简洁省略了部分文件），它使用 RPM 安装的 `/usr/bin/cai-pep`，部署 release 镜像：

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
  packages: [ca-certificates, curl, findutils, git, jq, kernel, kmod, nodejs, npm, podman, tar, xz]
  files:
    - source: /usr/bin/cai-pep
      target: /usr/local/bin/cai-pep
      executable: true
    - source: ./files/cai-pep-default-policy.json
      target: /usr/local/share/confidential-agent/openclaw/cai-pep-default-policy.json
  scripts:
    - ./install-openclaw.sh
  variants:
    release: { enabled: true }
    debug:   { enabled: false }   # 排障时开启，并把 image_variant 改为 debug

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

使用已发布镜像的最小部署端 AppSpec（`init <app>` 生成的 `instance.yaml` 就是这种形式）：

```yaml
schema: confidential-agent/v1
service:
  id: my-agent-01
  ports: [8642]
  connect: [8642]
  app_service: cai-allinone.service
  ssh: true
deploy:
  provider: aliyun
  instance_type: ecs.g9i.xlarge
  region: cn-beijing
  zone_id: cn-beijing-i
  disk_gb: 120
  image:
    release: ./image-release.json
    release_digest: sha256:<64 位摘要>
attestation:
  tee: tdx
  mode: challenge
  reference_values: rekor
resources:
  runtime:
    source: ./secrets/runtime.json
    target: /opt/data/allinone/runtime.json
```

## 11. 不再写在 AppSpec 中的配置

| 配置 | 现在的位置 |
|---|---|
| 入向访问 CIDR（旧 `deploy.security.allowed_cidr` / `a2a_peer_cidrs`） | `<state-dir>/peerings.yaml`，用 `confidential-agent peering` 管理；旧 AppSpec 可用 `confidential-agent migrate <spec>` 迁移 |
| 要调用的外部 AgentCard | `<state-dir>/a2a.json`，用 `confidential-agent a2a add` 管理 |
| Trustee URL、证书和管理密钥 | `<state-dir>/trustee/`，用 `confidential-agent trustee configure` 管理；AppSpec 只用 `attestation.mode: trustee` 选择 |
| 构建基础镜像与网络开关（旧 `build.base_image` / `build.with_network`） | 已移除，由 CA 统一管理 |

CLI 和 Guest 守护进程内部还会生成若干派生文件（bootstrap、mesh bundle、service directory 等），它们不需要手写，见 [架构设计](../design/architecture.md#2-核心数据结构)。
