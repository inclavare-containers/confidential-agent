---
name: tdx-remote-attestation
description: Use when 用户询问 OpenClaw/TDX/TEE/GPU TEE 运行环境是否可信、数据是否安全，或需要解释远程认证证据、启动度量和机密计算保护状态。
---

# Intel TDX 远程认证

本skill用于获取当前Intel TDX机密计算环境的远程认证信息，并以用户友好的方式解释环境的安全状态。

## 前置条件

假设环境已具备：
- `cai-pep` 已启用，且支持 `attest` 本机远程认证子命令
- Trustiflux API server 运行在 `http://localhost:8006`
- Attestation Agent socket 存在于 `/run/confidential-containers/attestation-agent/attestation-agent.sock`

如果下面的 `cai-pep attest collect-and-verify` 命令执行失败，必须直接告知用户远程认证未完成，并保留关键错误信息。不要改用 `tdx_guest` CPU flag、`/dev/tdx_guest`、系统日志、手写程序或其他启发式检查来推断认证通过；这些只能说明环境迹象，不能替代远程认证结果。

## 执行流程

### 步骤1：通过 `cai-pep` 本机 helper 获取并验证认证信息

必须执行以下命令。不要修改为 `curl localhost:8006`、直接读取设备文件或检查 CPU flags。

```bash
cai-pep attest collect-and-verify \
  --aa-url http://localhost:8006 \
  --tee tdx \
  --policy default \
  --claims
```

此命令输出包含：
1. 日志信息（INFO/WARN行）- **应忽略日志级别的警告**
2. JWT格式的认证结果
3. 解码后的JSON claims

**重要**：日志中的 WARN 信息（如 "collateral is out of date"、"GPU Attestation Evidence is null" 等）不要直接当作认证失败状态；硬件验证结果以 JSON claims 中的 `hardware` 字段和相关 evidence 字段为准。

使用该 helper 的原因：

- 认证需要访问 Guest 本机的 `attestation-challenge-client` 和 `localhost:8006`
- `localhost:8006` 是 Trustiflux API server 的 HTTP 入口；Attestation Agent 本身通过本机 Unix socket 被 Trustiflux 调用，不是 TCP 8006 服务
- 在启用 `cai-pep` 后，普通 `exec` 默认会进入 Docker sandbox，网络和本机二进制都可能不可见
- `cai-pep attest ...` 会由 `cai-pep` 在 Guest 本机直接执行受控远程认证流程，避免被通用 sandbox 限制

### 步骤2：解析关键信息

从JSON claims中提取以下关键字段：

| 字段路径 | 含义 |
|---------|------|
| `submods.cpu0["ear.trustworthiness-vector"].hardware` | 硬件可信度（判断标准：值 <= 32 为通过） |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.mr_td` | Trust Domain度量值 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_0` | RTMR[0] - 固件度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_1` | RTMR[1] - 启动配置度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_2` | RTMR[2] - 操作系统度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.uefi_event_logs` | UEFI事件日志 |

如果输出中没有 JSON claims、没有 `ear.trustworthiness-vector`，或无法提取 `mr_td`/`rtmr_*`/`measurement.uki.SHA-384` 等任一度量值，不要声称认证通过。应说明“远程认证命令已执行但返回内容不足，无法确认认证状态”。

**GPU TEE信息（如存在）**：

检查 `submods.cpu0["ear.veraison.annotated-evidence"].tdx` 下是否存在 `nvidia_gpu.0`（或 `nvidia_gpu.1` 等多GPU场景）。如果存在，说明当前实例为GPU TEE环境，需额外提取：

| 字段路径 | 含义 |
|---------|------|
| `nvidia_gpu.0.name` | GPU型号（如 "NVIDIA H20"） |
| `nvidia_gpu.0.cc_enabled` | GPU机密计算是否启用（应为 `true`） |
| `nvidia_gpu.0.driver_version` | GPU驱动版本（如 "550.144.03"） |
| `nvidia_gpu.0.vbios_version` | VBIOS版本（如 "96.00.CF.00.05"） |
| `nvidia_gpu.0.measurement` | GPU运行时度量值（SHA-384） |
| `nvidia_gpu.0.uuid` | GPU实例UUID |

> **注意**：GPU机密计算在实例级别启用，GPU与CPU共享同一TDX信任域。只有当 claims 中存在 GPU evidence 且验证字段正常时，才说明 GPU 驱动和 VBIOS 度量已通过对应完整性验证。

### 步骤3：识别启动方式并提取关键组件度量

系统支持两种启动方式，需要根据`uefi_event_logs`判断：

**判断逻辑**：
- 如果存在 `grubx64.efi` → **GRUB启动方式**
- 如果不存在 `grubx64.efi` 但存在 `BOOTX64.EFI` → **UKI启动方式**

**GRUB启动方式**需提取的组件：

| 组件 | 事件日志中的标识 | 说明 |
|-----|-----------------|------|
| Shim | `shimx64.efi` | 安全启动的第一阶段加载器 |
| GRUB | `grubx64.efi` | 引导加载程序 |
| Kernel | `vmlinuz-*` 或 `grub_linuxefi Kernel` | Linux内核 |
| Initrd | `initramfs-*.img` 或 `grub_linuxefi Initrd` | 初始内存盘 |
| Kernel Cmdline | `grub_kernel_cmdline` | 内核启动参数 |

**UKI启动方式**需提取的组件：

| 组件 | 事件日志中的标识 | 说明 |
|-----|-----------------|------|
| UKI | `BOOTX64.EFI` (device_paths中包含`\\EFI\\BOOT\\BOOTX64.EFI`) | 统一内核镜像（包含内核、initrd、cmdline） |

## 向用户解释结果

使用以下模板向用户解释认证结果。整体结构为：先说明本次命令直接验证了什么，再展示硬件和启动度量证据，最后把已知保护和未验证项分开说明。保持客观，不把缺少证据的能力表述为已验证结论。

### 模板

```
## 当前运行环境安全状态

您的请求正在一个**Intel TDX（Trust Domain Extensions）机密计算环境**中处理。本次远程认证命令直接验证的是 CPU TDX quote、启动度量 claims，以及在 GPU TEE evidence 存在时的 GPU 相关 claims。

**1. 硬件级内存加密** — Intel TDX 的内存加密引擎（MEE）对 Guest OS 的全部内存进行透明加密，数据在 CPU 和内存总线层面始终以密文存在。即使云厂商管理员或 Hypervisor 也无法读取明文。

**2. 启动度量可观测** — 从 UEFI 引导程序到内核、initrd，每个启动组件的哈希值都被记录在 CPU 的 RTMR 寄存器中。本次报告可以展示这些度量值；是否与构建时参考值一致，需要有 reference value 后才能下结论。

**3. 运行时策略执行** — 当前 OpenClaw 镜像集成了策略执行点（PEP），会把 exec 工具调用转发到受限 sandbox，并基于策略拦截网络外联、容器操作和敏感路径访问。若 PEP 服务不可用或未启用，应明确说明不能确认这层保护。

**4. 磁盘密钥注入** — Confidential Agent 设计为在 initrd 阶段基于远程证明获取可写层磁盘密钥。只有在启动链路已成功完成密钥挑战和磁盘解锁时，才能说明这层保护已生效。

**5. 可信连接** — Confidential Agent connect/mesh 设计使用 TNG RATS-TLS。只有当当前访问路径确认为 TNG 连接时，才能说明本次通信经过 RATS-TLS 保护。

以下远程认证报告验证了硬件 TDX quote，并提供启动度量 claims：

### 硬件安全保护

- **硬件可信状态**：[根据hardware值解释：<= 32 为通过]
- **TEE类型**：Intel TDX（Trust Domain）
- **内存加密**：Intel TDX MKE（Memory Encryption Engine）对全部 Guest 内存透明加密，数据离开 CPU 封装时自动加密
- **保护效果**：您的对话内容、工具执行上下文和 Agent 记忆在内存中始终以密文形式存在

**[如果存在GPU TEE信息，增加GPU安全保护段落]**

### GPU安全保护

- **GPU型号**：[nvidia_gpu.0.name]
- **GPU机密计算**：[cc_enabled为true则"已启用 ✓"，否则"未启用"]
- **GPU度量值**：[measurement的前16位]
- **驱动版本**：[driver_version]
- **VBIOS版本**：[vbios_version]

> GPU与CPU共享同一TDX信任域。只有当 claims 中存在 GPU evidence 且字段显示验证通过时，才能说明 GPU 驱动和 VBIOS 度量已通过对应验证；如果缺少 GPU evidence，不要推断 GPU TEE 已生效。

### 软件完整性

当前环境的启动链组件已被度量并记录：

**[如果是GRUB启动方式，显示以下表格]**

| 组件 | 度量值（SHA-384） |
|-----|------------------|
| Shim引导加载器 | [digest值] |
| GRUB引导程序 | [digest值] |
| Linux内核 | [digest值] |
| 初始化内存盘 | [digest值] |

**[如果是UKI启动方式，显示以下表格]**

| 组件 | 度量值（SHA-384） |
|-----|------------------|
| UKI统一内核镜像 | [BOOTX64.EFI的digest值] |

> **说明**：
> - GRUB模式：启动链包含独立的引导加载器、内核和初始化内存盘
> - UKI模式：使用统一内核镜像（Unified Kernel Image），将内核、initrd和启动参数打包为单一的UEFI可执行文件，提供更简洁的启动链和更强的完整性保证
> - 这些度量值由 CPU 硬件在引导过程中自动记录到 RTMR 寄存器，可用于验证启动组件是否被篡改

### 认证状态

- **硬件验证结果**：[基于hardware值给出结果]
  - hardware <= 32：✅ 硬件验证通过，CPU 确认运行于真实的 Intel TDX 硬件环境
  - hardware > 32：❌ 硬件验证未通过，建议谨慎

**[如果存在GPU TEE信息，增加GPU验证状态]**
- **GPU验证结果**：[仅当 claims 中存在 GPU evidence 且验证字段正常时报告"✅ GPU TEE evidence 已验证"，否则报告"❌ 未看到可证明 GPU TEE 通过验证的 evidence"]

**注意**：不要将日志中的 WARN 信息作为"认证状态"向用户报告。

### 总结

[根据具体情况总结，参考下方"总结生成指南"]
```

### 状态解释指南

**hardware 值解释**：
- `<= 32`：硬件验证通过，CPU确认运行在真实的TDX环境中，硬件级别的内存加密保护有效
- `> 32`：硬件验证未通过，环境可能存在问题

### 总结生成指南

认证通过后，根据场景生成总结段落，应覆盖以下要点（用通俗语言，非简单罗列技术术语）：

- **数据在内存中加密**：TDX 的 MEE 引擎确保数据在物理内存中始终是密文，云厂商无法读取
- **启动度量可观测**：远程证明提供启动组件度量值；是否与构建参考值一致，需要 reference value 验证结果支撑
- **GPU 安全保护（如适用）**：GPU 与 CPU 共享 TDX 信任域；仅在 GPU evidence 存在且验证字段正常时说明 GPU 侧通过验证
- **运行时沙箱保护**：如果 PEP 服务正常启用，Agent exec 工具调用会进入受限 sandbox，策略执行点拦截危险操作
- **密钥自主管理**：如果磁盘密钥挑战和解锁状态可确认，再说明密钥由用户侧持有、云厂商不参与

示例总结（有 GPU）：
> 您的数据在此环境中受到 CPU TDX 硬件级别的内存机密性保护。本次远程认证报告证明 CPU quote 验证通过，并提供了启动组件度量值。报告中如包含 GPU evidence 且验证字段正常，可以进一步说明 GPU TEE 保护已启用。构建参考值匹配、TNG/RATS-TLS 连接状态和磁盘密钥注入状态需要结合对应证据另行说明。

示例总结（无 GPU）：
> 您的数据在此环境中受到 CPU TDX 硬件级别的内存机密性保护。本次远程认证报告证明 CPU quote 验证通过，并提供了启动组件度量值。构建参考值匹配、TNG/RATS-TLS 连接状态和磁盘密钥注入状态需要结合对应证据另行说明。

## 回复示例

### 场景：用户询问数据安全性（含GPU TEE）

用户："我的医疗数据在这里处理安全吗？"

回复示例：

---

我理解您对医疗数据安全的关注。让我为您检查当前运行环境的安全状态。

[执行远程认证命令]

## 当前运行环境安全状态

您的请求正在一个**Intel TDX（Trust Domain Extensions）机密计算环境**中处理。本次远程认证命令直接验证 CPU TDX quote，并返回启动度量 claims：

**1. 硬件级内存加密** — Intel TDX 的内存加密引擎（MEE）对 Guest OS 的全部内存进行透明加密，即使云厂商管理员也无法读取内存中的明文数据。

**2. 启动度量可观测** — 从引导程序到内核的每个组件都被度量；是否与构建参考值一致，需要 reference value 验证结果。

**3. 运行时策略执行** — 如果 PEP 服务正常启用，Agent exec 工具调用会进入受限 sandbox，策略执行点会按策略拦截网络外联和敏感操作。

**4. 零信任密钥管理** — 如果启动链路中的密钥挑战和磁盘解锁状态可确认，才说明磁盘密钥注入和可写层解锁保护已生效。

**5. 可信连接** — 如果当前连接来自 Confidential Agent connect/mesh，则 TNG RATS-TLS 会在连接阶段验证实例身份并保护通信。

以下远程认证报告验证了硬件 quote，并提供启动度量：

### 硬件安全保护

- **硬件可信状态**：已验证 ✓
- **TEE类型**：Intel TDX Trust Domain
- **内存加密**：Intel TDX MKE 对全部 Guest 内存透明加密，数据离开 CPU 封装时自动加密
- **保护效果**：您的医疗数据、对话记录和 Agent 记忆在内存中始终以密文形式存在

### GPU安全保护

- **GPU型号**：NVIDIA H20
- **GPU机密计算**：已启用 ✓
- **GPU度量值**：8230c5ff3d991d99...
- **驱动版本**：550.144.03
- **VBIOS版本**：96.00.CF.00.05

> GPU与CPU共享同一TDX信任域。本段仅在 GPU evidence 存在且验证字段正常时展示；否则不要声称 GPU TEE 已通过验证。

### 软件完整性

当前环境的启动链已被完整度量：

**[GRUB启动方式示例]**

| 组件 | 度量值（SHA-384前16位） |
|-----|------------------------|
| 安全启动加载器(Shim) | `06647f7cd6b1f004...` |
| 引导程序(GRUB) | `1c6b41cc5f1e08df...` |
| Linux内核 | `fd4099ae8fdd9861...` |
| 初始化内存盘 | `cb2f7b75f918a1b7...` |

这些度量值由 CPU 硬件在引导过程中自动记录到 RTMR 寄存器。它们证明组件已被度量；是否未被篡改，需要和 sample/rekor reference value 对比后判断。

**[UKI启动方式示例]**

| 组件 | 度量值（SHA-384前16位） |
|-----|------------------------|
| UKI统一内核镜像 | `01785e4f19a71636...` |

UKI（Unified Kernel Image）将内核、initrd和启动参数打包为单一的UEFI可执行文件，提供更简洁的启动链和更强的完整性保证。

### 认证状态

- **硬件验证结果**：通过 ✓
- **hardware 值**：2（<= 32，符合标准）
- **GPU验证结果**：仅当 GPU evidence 存在且验证字段正常时报告为通过
- **结论**：CPU TDX quote 验证通过；GPU 只在 evidence 存在且验证字段正常时说明通过；构建参考值匹配、RATS-TLS 连接状态和磁盘密钥状态需要分别基于对应证据说明

### 总结

您的医疗数据在此环境中受到 CPU TDX 硬件级别的内存机密性保护。本次远程认证报告证明 CPU quote 验证通过，并提供了启动组件度量值。若报告中包含 GPU evidence 且验证字段正常，可以进一步说明 GPU TEE 保护已启用；若未看到 GPU evidence，则不要声称 GPU 侧已验证。构建参考值匹配、TNG/RATS-TLS 连接状态和磁盘密钥注入状态需要结合对应状态或配置另行说明。

---

## 注意事项

1. **GPU TEE检测**：
   - 检查 `tdx` 对象下是否存在 `nvidia_gpu.*` 字段判断是否为GPU TEE环境
   - GPU与CPU共享同一TDX信任域，一次远程认证同时覆盖CPU和GPU
   - 仅当 claims 中包含 GPU evidence 且验证字段正常时，才说明 GPU 驱动和 VBIOS 已通过对应完整性验证
   - 如果实例无GPU或运行在QEMU dev环境，则不展示GPU安全保护段落

2. **启动方式识别**：
   - 通过检查 `uefi_event_logs` 中是否存在 `grubx64.efi` 来判断启动方式
   - GRUB模式：展示完整的启动链（Shim → GRUB → Kernel → Initrd）
   - UKI模式：仅展示 BOOTX64.EFI 的度量值（从 device_paths 包含 `\\EFI\\BOOT\\BOOTX64.EFI` 的事件中提取）

3. **忽略日志警告**：
   - 验证命令的日志输出中可能包含 WARN 级别信息（如 "collateral is out of date"、"GPU Attestation Evidence is null"）
   - 这些警告不影响硬件验证的有效性，不应作为"认证状态 warning"向用户报告
   - 仅基于 JSON claims 中的 `hardware` 字段（<= 32 为通过）判断验证结果

4. **度量值和参考值分开表述**：claims 中的度量值只能证明“已被度量并上报”；只有拿到 sample/rekor reference value 并验证匹配后，才能说“与构建参考值一致”

5. **保持客观中性**：如实描述认证结果，不夸大也不淡化安全状态。TDX、PEP、RATS-TLS、磁盘密钥注入分别依据各自证据说明，不能用 CPU quote 结果替代其他层的验证结果

6. **解释技术术语**：用通俗语言解释TDX、RTMR、UKI、RIM、MEE等技术概念

7. **提供可操作建议**：针对 hardware > 32 的情况，给出具体的后续步骤建议
