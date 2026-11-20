# 宿主机依赖

`confidential-agent`（别名 `cagent`）直接使用宿主机原生工具，不依赖宿主机 Docker。`--tools-dir` / `CA_TOOLS_DIR` 默认指向 `/usr/libexec/cagent/bin`；缺少工具时会继续查找宿主机 `PATH`。

| 组件 | 提供方式 | 用途 |
|---|---|---|
| confidential-agent / cagent | cagent RPM | Host CLI |
| confidential-agentd、cai-gateway、cai-pep | cagent RPM | 构建时放入 Guest，不作为宿主机服务启动 |
| attestation-challenge-client、DCAP/TPM 库 | 原生 RPM 依赖 | 远程证明、资源注入和报告；可与 Shelter 原生注入共享 |
| TNG 2.9.2 | 原生 RPM 依赖 | connect 前台或后台隧道 |
| cosign 3.0.6 | cagent 私有工具目录 | 签名、密钥生成及 Guest 验签资产 |
| aliyun 3.0.232、ossutil 1.7.18 | cagent 私有工具目录 | 云镜像发布与导入；ALB 查询通过 Shelter 子进程 PATH 使用同一 aliyun |
| oras 1.3.0 | cagent 私有工具目录 | 按不可变 `@sha256` 引用拉取或推送 OCI 镜像产物 |
| rekor-cli | Shelter 的 `/usr/libexec/shelter/bin/rekor-cli` | 参考值上传；cagent 通过符号链接复用 |
| Shelter、Terraform/providers | Shelter RPM | 镜像构建、预构建产物校验和云资源生命周期 |
| mkosi、QEMU、LVM、cryptsetup、文件系统工具 | Shelter RPM 及其依赖 | 磁盘镜像构建与产物校验 |
| OpenSSH、CA 证书、coreutils、bash、jq、Python 3.11 | RPM 依赖 | SSH、脚本、发布元数据解析和 ALB 查询 |

导入预构建镜像时，Shelter 使用 QEMU 磁盘工具校验产物。已有 ECS 镜像 ID 时，部署直接使用云镜像和描述文件。Rust/Cargo/gcc 用于源码编译或打包 CA。

Node/npm、Python 应用包、CUDA/vLLM 等在 Guest 镜像内准备。RPM 与部署 ZIP 均不附带宿主机应用客户端。使用 one-click 的 OpenClaw TUI、Codex 远程 CLI 或 OpenCode attach 时，交互模式会询问是否在本机安装客户端及其 Node.js/npm 依赖，默认跳过。非交互安装需显式传入 `--install-host-client`；`--yes` 仅用于其他部署确认。显式配置 `build.container.image` 的构建需要宿主机 Podman 或 Docker。

原生工具可以直接访问宿主机文件和代理配置。凭证仍通过进程环境或临时
0600 OSS 配置传递，临时文件使用后删除，不放入命令参数。
每个 CA state 保留独立的证明策略与参考值目录。后台 TNG 的日志为 0600，
PID、启动时刻与 boot ID 共同识别进程，并通过 pidfd 发送停止信号。

安装和完整构建部署步骤见 [RPM 使用说明](rpm.md)。
