# CLI 参考

`confidential-agent`（别名 `cagent`）是部署机上的命令行工具。本文按用途列出子命令和常用参数，完整参数以 `confidential-agent <子命令> --help` 为准。

## 全局参数

| 参数 | 环境变量 | 默认值 | 说明 |
|---|---|---|---|
| `--state-dir` | — | `~/.confidential-agent` | 状态目录。同一套环境的所有命令必须使用相同值 |
| `--tools-dir` | `CA_TOOLS_DIR` | `/usr/libexec/cagent/bin` | 原生工具目录，找不到时回退到 `PATH` |

`confidential-agent --version` 或 `confidential-agent version` 打印版本号。顶层 `--help` 标题会显示安装形态（`full` / `deploy-only`）。

## 命令总览

| 分类 | 命令 | 作用 |
|---|---|---|
| 帮助 | `docs <overview\|workflow\|appspec\|ops>` | 内置简要文档 |
| | `spec schema` / `spec validate` | AppSpec 字段速查 / 校验 AppSpec |
| 准备 | `init [<app>] [--stage build\|deploy]` | 生成共享镜像构建配置或应用实例配置 |
| | `key generate-cosign` | 生成 Rekor 签名密钥对 |
| 镜像 | `build` | 本地构建镜像（仅全功能版） |
| | `image export \| publish \| resolve \| list \| rm \| unpublish \| prune` | 镜像描述导出、导入 ECS、查询与清理 |
| 部署 | `deploy` | 创建实例并完成证明与资源注入 |
| | `inject` | 向已有实例重新注入资源，恢复重启后的 challenge 实例 |
| 访问 | `connect` / `connect start` / `connect stop` | 本地可信隧道 |
| | `ssh` | 实例 SSH |
| 观测 | `status` / `tui` / `report` | 状态、实时面板、证明报告 |
| 网络与协作 | `peering ...` | 入向网络授权 |
| | `a2a ...` | 跨组织 AgentCard |
| | `trustee ...` | Trustee 配置与同步 |
| | `mesh sync`（隐藏） | 重新下发 mesh bundle |
| 生命周期 | `destroy` / `forget` | 销毁实例 / 清除本地记录 |
| | `upgrade check \| bind-inputs` | 升级前兼容性检查 |
| | `migrate` | 把旧版 AppSpec 中的网络配置迁移为 peering |

## 准备

### `init`

```bash
confidential-agent init --stage build [--reference-values rekor] [--output-dir DIR]           # 共享 CPU 镜像构建配置
confidential-agent init <app> --image-release FILE --release-digest sha256:... [--image-id ID] \
  --service-id ID [--region R] [--zone-id Z] [--instance-type T] [--output-dir DIR]          # 应用实例配置
```

`<app>`：`openclaw`、`hermes`、`codex`、`opencode`、`dsh`、`claude-code`、`qodercli`、`pi`（共享 CPU 镜像），`openclaw-vllm`、`vllm-inference`（GPU 专用镜像）。

| 参数 | 默认 | 说明 |
|---|---|---|
| `--output-dir` | `./confidential-agent-init` | 在其下生成 `<service-id>/` 或 `allinone-builder/` |
| `--service-id` | 应用名 / `allinone-builder` | 服务 ID，也是输出子目录名 |
| `--non-interactive` | — | 不提问，缺参数时报错 |
| `--force` | — | 覆盖已有输出目录 |
| `--harness-mode` | 应用的标准模式 | `gateway`、`app-server`、`web` 或 `cli`（改为 SSH 使用） |
| `--model` | `qwen4-max` | 也读取 `DASHSCOPE_MODEL` |
| `--dashscope-api-key` | — | 也读取 `DASHSCOPE_API_KEY` |
| `--gateway-token` | 自动生成 | OpenClaw 应用令牌，也读取 `CA_GATEWAY_TOKEN` |
| `--disable-pep` / `--enable-dingtalk` | — | 仅 OpenClaw |
| `--replicas` | `1` | 网络服务的副本数 |
| `--reference-values` | — | `sample` 或 `rekor` |
| `--model-key-file` / `--oss-bucket` / `--oss-endpoint` | — | 仅 `vllm-inference`：模型解密密钥文件与 OSS 密文位置 |

### `spec`

```bash
confidential-agent spec validate --spec my-agent.yaml [--format markdown|json]
confidential-agent spec schema [--format markdown|json]
```

`validate` 检查解析错误、字段约束、本地路径是否存在，以及安装脚本中的常见问题（例如用到 `curl` 却没有安装对应软件包）。

### `key generate-cosign`

```bash
confidential-agent key generate-cosign --output-key-prefix ./cosign [--force]   # 生成 ./cosign.key 与 ./cosign.pub
```

## 镜像

```bash
confidential-agent build --spec build.yaml
confidential-agent image export <builder> --output image-release.json [--image-id ID --region R] [--variant V]
confidential-agent image publish <builder> --spec build.yaml [--region R] [--variant V] [--output published.json] [--no-wait]
confidential-agent image publish --artifact DIR --image-release FILE --release-digest sha256:... --region R [--verify-disk] [--output published.json]
confidential-agent image resolve --image-release FILE --release-digest sha256:... --region R
confidential-agent image list [--json]
confidential-agent image rm <builder> [--force]
confidential-agent image unpublish <builder | sha256:manifest> [--region R] [--variant V] [--image-id ID] [--force]
confidential-agent image prune [--dry-run] [--all]
```

| 命令 | 说明 |
|---|---|
| `image export` | 导出可移植的镜像描述 `image-release.json`，并打印其摘要 `release_digest`。指定 `--image-id` 时才写入云镜像映射 |
| `image publish` | 上传 OSS 并导入为 ECS 自定义镜像，按 manifest 摘要、账号和地域记录 `image_id`。`--no-wait` 只创建导入任务；再次运行（不加 `--no-wait`）会等待同一镜像完成 |
| `image resolve` | 查找当前地域与某个发布描述匹配的可用镜像 |
| `image list` | 本地构建与云端发布（不含实例）。`--json` 输出各地域的状态、错误、所有权和待清理标记 |
| `image rm` | 删除本地镜像文件，保留发布记录 |
| `image unpublish` / `image prune` | 退役云镜像，见 [运维手册 · 清理资源](../guides/operations.md#清理资源) |

## 部署

```bash
confidential-agent deploy --spec instance.yaml [--image-id ID] [--resume] [--skip-peering-check]
confidential-agent inject --spec instance.yaml --target-ip IP [--skip-peering-check]
```

- `--image-id`：直接使用当前地域已有的自定义、共享或云市场镜像。
- `--resume`：基础设施已就绪（`deployed`/`activating`）时，校验原 AppSpec 的路径和摘要、冻结的部署输入以及实例身份，然后续做资源注入和就绪流程。
- `--skip-peering-check`（或 `CA_SKIP_PEERING_CHECK=1`）：跳过 operator peering 预检查。
- `inject`：为已管理的实例重新交付机密和应用资源，然后同步 mesh、A2A 及网络配置。它复用部署时绑定的镜像与身份，允许移动内容未变的密钥文件和项目目录。具体步骤见 [challenge 模式重启后恢复](../guides/operations.md#challenge-模式重启后恢复)。

## 访问

```bash
confidential-agent connect [--service ID] [--from-card URL]            # 前台隧道，Ctrl-C 结束
confidential-agent connect start [--service ID] [--instance-id ID] \
  [--ready-json connect-ready.json] [--wait-ready 120] [--log-file FILE]
confidential-agent connect stop [--ready-json connect-ready.json]

confidential-agent ssh <service> [--tty] [--instance-id ID] [--identity-file KEY] -- <command...>
```

- 不指定 `--service` 时，`connect` 覆盖所有暴露 connect 端口的活跃服务。
- `--from-card` 根据 AgentCard URL 连接外部服务。
- `ready-json` 的 `client_endpoints[]` 记录实际映射的本地地址。
- `ssh` 不带 `--tty` 时适合脚本，保留标准输出和退出码；`--identity-file` 用于已发布 debug 镜像的原始私钥。

## 观测

```bash
confidential-agent status [--service ID] [--live] [--json]
confidential-agent tui [--service ID] [--refresh 2] [--attestation-refresh 60]
confidential-agent report [--service ID] [--include-a2a] [--json] [--out FILE]
```

字段含义见 [运维手册 · 查看状态与证明结果](../guides/operations.md#查看状态与证明结果)。

## 网络与协作

```bash
confidential-agent peering add --role operator|peer --cidr CIDR --label NAME [--scope s1,s2] [--note TEXT]
confidential-agent peering list | show <label> | remove <label>
confidential-agent peering apply [--dry-run]

confidential-agent a2a add <agent-card-url> [--alias ID] [--service s1,s2] [--signer-issuer I --signer-subject S]
confidential-agent a2a list | show <alias-or-url> | remove <alias-or-url>
confidential-agent a2a sync [--alias ID] [--all]

confidential-agent trustee configure --url URL --admin-key KEY [--management-url URL] [--ca-cert PEM] [--force]
confidential-agent trustee show | doctor | status [--json]
confidential-agent trustee adopt --attestation-policy-sha256 H --resource-policy-sha256 H
confidential-agent trustee sync [--service ID]
confidential-agent trustee prune [--apply]
```

详见 [运维手册 · 网络授权](../guides/operations.md#网络授权peering)、[A2A 指南](../guides/a2a.md)、[Trustee 模式](../guides/trustee.md)。

## 生命周期

```bash
confidential-agent destroy <service>
confidential-agent forget <service>
confidential-agent upgrade check
confidential-agent upgrade bind-inputs --service ID      # 用历史 Trustee 磁盘摘要和已记录身份冻结旧版部署输入
confidential-agent migrate <old-spec> [--out NEW_SPEC] [--peerings-out FILE]
```

## 环境变量

### 凭据

| 变量 | 用途 |
|---|---|
| `ALICLOUD_ACCESS_KEY` / `ALICLOUD_SECRET_KEY` | 阿里云凭据。也支持 `aliyun configure` 的 profile（`ALIBABA_CLOUD_PROFILE` / `ALICLOUD_PROFILE` 选择 profile，`ALIBABA_CLOUD_CREDENTIALS_FILE` / `ALICLOUD_SHARED_CREDENTIALS_FILE` 指定文件） |
| `DASHSCOPE_API_KEY` / `DASHSCOPE_MODEL` | 百炼 API Key 与默认模型 |
| `DINGTALK_BOT_CLIENT_ID` / `DINGTALK_BOT_CLIENT_SECRET` | OpenClaw 钉钉集成 |
| `CA_GATEWAY_TOKEN` | OpenClaw 应用令牌 |
| `CA_A2A_SIGSTORE_IDENTITY_TOKEN` | AgentCard keyless 签名使用的 OIDC token，避免交互登录 |

### 网络

| 变量 | 默认 | 用途 |
|---|---|---|
| `CA_GITHUB_PROXY_URL` | `https://gh-proxy.org/` | GitHub 下载代理，设为空字符串禁用 |
| `CA_GITHUB_PROXY_FIRST` | `1` | `1` 代理优先，`0` 官方优先 |
| `CA_IMAGE_OSS_ENDPOINT` | `auto` | 镜像上传的 OSS 端点。`auto`：同地域 ECS 且内网 HTTPS 探测成功时走内网，否则公网；也可设为 `public`、`internal` 或 HTTPS 地址（显式值不会回退） |
| `CA_OSS_ENDPOINT` | — | 模型下载端点（与 `--oss-endpoint` 相同），与镜像上传无关 |
| `CA_TRUSTED_REKOR_URLS` | `https://rekor.sigstore.dev` | A2A 可信 Rekor 列表，逗号分隔 |
| `CA_OPERATOR_EGRESS_IP` | 自动探测 | 指定部署机公网出口 IPv4，用于 peering 预检查 |
| `CA_DEPLOYER_CIDR` | — | 部署时记录的部署机 CIDR，后续 `deploy --resume`、`peering apply` 会复用 |

### 超时

| 变量 | 默认 | 用途 |
|---|---|---|
| `CA_BOOTSTRAP_WAIT_TIMEOUT_SEC` / `CA_BOOTSTRAP_RETRY_INTERVAL_SEC` | 600 / 5 | challenge 注入等待 Guest 就绪的总时长和重试间隔 |
| `CA_CHALLENGE_INJECT_ATTEMPT_TIMEOUT_SEC` | 90 | 单次注入尝试超时 |
| `CA_DAEMON_STATUS_WAIT_SEC` | 180 | 等待 Guest 守护进程 `:8088` 可达 |
| `CA_APP_READY_WAIT_SEC` | 300；GPU 规格（`ecs.gn*`、`ecs.ebmgn*`）或 `cai-vllm.service` 为 7200 | 等待应用就绪 |
| `CA_IMAGE_IMPORT_TIMEOUT_SEC` | 7200 | 等待 ECS 镜像导入完成 |

Guest 侧 initrd 取密超时（`CA_SECRET_WAIT_TIMEOUT_SEC` 等）由镜像固定配置，见 [Trustee 模式 · 启动和重启](../guides/trustee.md#7-启动和重启)。
