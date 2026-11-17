# 宿主机依赖

cagent 已使用原生工具。工具容器、`--tools-image`、`CA_TOOLS_IMAGE` 和
宿主机 Docker 要求已移除。`--tools-dir` / `CA_TOOLS_DIR` 默认指向
`/usr/libexec/cagent/bin`；缺少工具时会继续查找宿主机 PATH。

| 组件 | 提供方式 | 用途 |
|---|---|---|
| confidential-agent / cagent | cagent RPM | Host CLI |
| confidential-agentd、cai-gateway、cai-pep | cagent RPM | 构建时放入 Guest，不作为宿主机服务启动 |
| attestation-challenge-client、DCAP/TPM 库 | 原生 RPM 依赖 | 远程证明、资源注入和报告；可与 Shelter 原生注入共享 |
| TNG 2.9.2 | 原生 RPM 依赖 | connect 前台或后台隧道 |
| cosign 3.0.6 | cagent 私有工具目录 | 签名、密钥生成及 Guest 验签资产 |
| aliyun 3.0.232、ossutil 1.7.18 | cagent 私有工具目录 | 云镜像发布；ALB 查询通过 Shelter 子进程 PATH 使用同一 aliyun |
| rekor-cli | Shelter 的 /usr/libexec/shelter/bin/rekor-cli | 参考值上传；cagent 通过符号链接复用 |
| Shelter、Terraform/providers | Shelter RPM | 镜像构建和云资源生命周期 |
| mkosi、QEMU、LVM、cryptsetup、文件系统工具 | Shelter RPM 及其依赖 | 磁盘镜像构建；现有 Shelter 包仍统一安装这些依赖 |
| OpenSSH、CA 证书、coreutils、bash、jq | RPM 依赖 | SSH、验证超时、脚本和 ALB 查询 |

只部署已发布镜像的调用路径不使用 mkosi/QEMU/LVM，不上传 QCOW2，也不
编译 Guest 应用。当前 Shelter 尚未拆出单独的部署 RPM，因此其打包依赖
仍会安装。Rust/Cargo/gcc 只用于从源码打包 CA，RPM 使用者不需要安装。

Node/npm、Python 应用包、CUDA/vLLM 等在 Guest rootfs 内安装；部分
one-click OpenClaw 场景还会安装宿主机 OpenClaw 客户端用于交互。
如果 AppSpec 显式使用 build.container.image，需要按该功能安装 Podman
或 Docker；这不是标准 all-in-one 构建的前置条件。Guest 内 PEP 的容器
隔离也不要求宿主机安装 Docker。

原生工具可以直接访问宿主机文件和代理配置。凭证仍通过进程环境或临时
0600 OSS 配置传递，临时文件使用后删除，不放入命令参数。
每个 CA state 保留独立的证明策略与参考值目录。后台 TNG 的日志为 0600，
PID、启动时刻与 boot ID 共同识别进程，并通过 pidfd 发送停止信号。

安装和完整构建部署步骤见 [RPM 使用说明](rpm.md)。
