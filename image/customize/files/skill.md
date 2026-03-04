---
name: tdx-remote-attestation
description: 获取并解释Intel TDX机密计算环境的远程认证信息，向用户说明当前运行环境的机密性和完整性保护状态。当用户询问数据安全、环境可信度、机密计算、TEE、TDX、远程认证、硬件验证、启动链完整性、UKI、GRUB、RTMR、MR_TD、信任根、安全启动、内存加密、数据隐私、医疗数据、金融信息、个人隐私、AI安全、模型安全、OpenClaw安全、技能加载、环境验证、可信执行环境、机密虚拟机、Intel SGX、AMD SEV、零信任等问题时自动触发。
---

# Intel TDX 远程认证

本skill用于获取当前Intel TDX机密计算环境的远程认证信息，并以用户友好的方式解释环境的安全状态。

## 触发场景

在以下情况自动使用此skill：
- 用户询问"我的数据安全吗？"、"这个环境可信吗？"等安全相关问题
- 用户提到机密计算、TEE、TDX、远程认证等技术概念
- 用户处理医疗数据、个人隐私信息、金融数据等敏感内容
- 用户对AI运行环境的安全性表示担忧

## 前置条件

假设环境已具备：
- `attestation-challenge-client` 命令行工具
- Attestation Agent服务运行在 `http://localhost:8006`

如果命令执行失败，告知用户环境可能未正确配置机密计算组件。

## 执行流程

### 步骤1：获取认证证据

```bash
attestation-challenge-client get-evidence \
  --aa-url http://localhost:8006 \
  --output /tmp/evidence.json
```

### 步骤2：验证并获取claims

```bash
attestation-challenge-client verify \
  --evidence /tmp/evidence.json \
  --tee tdx \
  --policy default \
  --claims
```

此命令输出包含：
1. 日志信息（INFO/WARN行）- **应忽略日志级别的警告**
2. JWT格式的认证结果
3. 解码后的JSON claims

**重要**：日志中的 WARN 信息（如 "collateral is out of date"、"GPU Attestation Evidence is null" 等）不影响硬件验证结果，应予以忽略。仅关注 JSON claims 中的 `hardware` 字段值。

### 步骤3：解析关键信息

从JSON claims中提取以下关键字段：

| 字段路径 | 含义 |
|---------|------|
| `submods.cpu0["ear.trustworthiness-vector"].hardware` | 硬件可信度（判断标准：值 <= 32 为通过） |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.mr_td` | Trust Domain度量值 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_0` | RTMR[0] - 固件度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_1` | RTMR[1] - 启动配置度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.quote.body.rtmr_2` | RTMR[2] - 操作系统度量 |
| `submods.cpu0["ear.veraison.annotated-evidence"].tdx.uefi_event_logs` | UEFI事件日志 |

### 步骤4：识别启动方式并提取关键组件度量

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

使用以下模板向用户解释认证结果，保持中性客观的语气：

### 模板

```
## 当前运行环境安全状态

您的请求正在一个**Intel TDX（Trust Domain Extensions）机密计算环境**中处理。以下是通过远程认证获取的环境安全信息：

### 硬件安全保护

- **硬件可信状态**：[根据hardware值解释：<= 32 为通过]
- **TEE类型**：Intel TDX（Trust Domain）
- **硬件保护机制**：您的数据在处理过程中受到CPU硬件级别的内存加密保护，即使是云服务提供商也无法访问Trust Domain内的明文数据

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
> - UKI模式：使用统一内核镜像（Unified Kernel Image），将内核、initrd和启动参数打包为单一的UEFI可执行文件
> - 这些度量值可用于验证启动组件是否被篡改。如需验证，请将这些值与您信任的参考值进行比对。

### 认证状态

- **硬件验证结果**：[基于hardware值给出结果]
  - hardware <= 32：✅ 硬件验证通过，环境可信
  - hardware > 32：❌ 硬件验证未通过，建议谨慎

**注意**：不要将日志中的 WARN 信息作为"认证状态"向用户报告。

### 建议

[根据具体情况给出建议]
```

### 状态解释指南

**hardware 值解释**：
- `<= 32`：硬件验证通过，CPU确认运行在真实的TDX环境中，硬件级别的内存加密保护有效
- `> 32`：硬件验证未通过，环境可能存在问题

## 回复示例

### 场景：用户询问数据安全性

用户："我的医疗数据在这里处理安全吗？"

回复示例：

---

我理解您对医疗数据安全的关注。让我为您检查当前运行环境的安全状态。

[执行远程认证命令]

## 当前运行环境安全状态

您的请求正在一个**Intel TDX（Trust Domain Extensions）机密计算环境**中处理。

### 硬件安全保护

- **硬件可信状态**：已验证 ✓
- **TEE类型**：Intel TDX Trust Domain
- **保护机制**：您的数据在内存中全程加密，受CPU硬件保护。即使云服务提供商的管理员也无法访问您的明文数据。

### 软件完整性

当前环境的启动链已被完整度量：

**[GRUB启动方式示例]**

| 组件 | 度量值（SHA-384前16位） |
|-----|------------------------|
| 安全启动加载器(Shim) | `06647f7cd6b1f004...` |
| 引导程序(GRUB) | `1c6b41cc5f1e08df...` |
| Linux内核 | `fd4099ae8fdd9861...` |
| 初始化内存盘 | `cb2f7b75f918a1b7...` |

这些度量值记录了从固件到操作系统的完整启动过程，可用于验证系统未被篡改。

**[UKI启动方式示例]**

| 组件 | 度量值（SHA-384前16位） |
|-----|------------------------|
| UKI统一内核镜像 | `01785e4f19a71636...` |

UKI（Unified Kernel Image）将内核、initrd和启动参数打包为单一的UEFI可执行文件，提供了更简洁的启动链和更强的完整性保证。

### 认证状态

- **硬件验证结果**：通过 ✓
- **hardware 值**：2（<= 32，符合标准）
- **结论**：您的运行环境通过了硬件级别的可信验证

### 结论

您的医疗数据在此环境中受到硬件级别的机密性保护。TDX技术确保数据在处理过程中始终以加密形式存在于内存中，提供了强有力的隐私保障。

---

## 注意事项

1. **启动方式识别**：
   - 通过检查 `uefi_event_logs` 中是否存在 `grubx64.efi` 来判断启动方式
   - GRUB模式：展示完整的启动链（Shim → GRUB → Kernel → Initrd）
   - UKI模式：仅展示 BOOTX64.EFI 的度量值（从 device_paths 包含 `\\EFI\\BOOT\\BOOTX64.EFI` 的事件中提取）
2. **忽略日志警告**：
   - 验证命令的日志输出中可能包含 WARN 级别信息（如 "collateral is out of date"、"GPU Attestation Evidence is null"）
   - 这些警告不影响硬件验证的有效性，不应作为"认证状态 warning"向用户报告
   - 仅基于 JSON claims 中的 `hardware` 字段（<= 32 为通过）判断验证结果
3. **度量值无参考值**：当前环境未预设度量参考值，skill应引导用户自行保存或比对这些值
4. **保持客观中性**：如实描述认证结果，不夸大也不淡化安全状态
5. **解释技术术语**：用通俗语言解释TDX、RTMR、UKI等技术概念
6. **提供可操作建议**：针对 hardware > 32 的情况，给出具体的后续步骤建议
