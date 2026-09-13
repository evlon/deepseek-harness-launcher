# Windows Defender 误报处置记录（launcher）

## 现象

同事从浏览器下载 launcher 时，Windows Defender 直接报病毒并删除文件，下载中断。

## 判定结论

**误报（false positive）**，判定类型为**机器学习启发式**，非病毒特征码命中。

```
ThreatID         : 251873
ThreatName       : Program:Win32/Contebrew.A!ml
                   └─ 后缀 !ml = machine learning 启发式判定
SeverityID       : 4
DidThreatExecute : False   ← 从未执行
CategoryID       : 23
```

`!ml` 表示该判定由统计模型对整个文件的字节分布打分产生，而非匹配已知恶意代码特征。

## 证据链

### 1. 命中记录（本机 Defender 日志 `Get-MpThreatDetection`）

| 时间 | 对象 | 结果 |
|---|---|---|
| 09-13 17:53:54 | 浏览器下载 `conf.ai.ict.cmcc/api/launcher/download?file=launcher-0.3.3.exe`（.crdownload） | 命中 |
| 09-13 17:55:06 | 同上，第二次 | 命中 |
| 09-13 17:57:34 | 本机编译产物 `target\release\deepseek-harness-launcher.exe` | 命中 |
| 09-13 17:59/18:00 | 手动复现下载 | 命中 |

被拦文件上 `Get-FileHash` / `certutil` 直接返回 `0x800700e1 ERROR_VIRUS_INFECTED`。

### 2. 对照组（关键）

| 版本 | 复制+CustomScan | HTTP 下载+certutil | 结论 |
|---|---|---|---|
| 0.3.0 | 干净 | — | 干净 |
| 0.3.1 | 干净 | 干净 | 干净 |
| 0.3.3 | **命中** | **命中** | 命中 |

→ 不是"未签名 exe 普遍被拦"，也不是"下载方式"问题，而是**特定二进制内容**触发。

### 3. 触发画像（推断，未做字节级定位）

0.3.3 相对 0.3.1 新增了：
- `env_defaults.rs`：**硬编码一批内网域名**（`roster/market/job.ai.ict.cmcc`、`im-ipm.ict.cmcc`）
- `default-config.json`：serverUrl 切到 `conf.ai.ict.cmcc`
- 叠加既有行为：HTTP 下载 exe → rename 替换自身 → 重启；`netstat` 扫端口；`taskkill` 杀进程

**「内嵌硬编码地址 + HTTP 下载可执行文件并自我替换 + 无代码签名」**这一组合是 ML 模型的典型可疑画像。

## ⚠️ 重要更正：判定消失的真实原因

排查过程中发现 **0.3.3 后来也不再被拦**。必须区分两个变量：

```
18:03:56  Defender 安全智能更新（事件 ID 2000，特征库 → 1.459.187.0）
18:27     我编译 0.3.4
18:3x     复测：0.3.4 干净，且未改动的 0.3.3 也变干净
```

**决定性对照**：重新下载**未做任何修改的 0.3.3**，`Get-FileHash` 现在可以正常读取
（sha256 `be396bbe…`，与最初发布值一致）。

→ **判定消失的原因是微软特征库更新放行，不是代码改动生效。**

因此**不能声称"0.3.4/0.3.5 修复了报毒"**。这类 ML 误报具有间歇性：同一二进制可能今天被拦、
明天放行，换台机器（特征库版本不同）结论也可能相反。

## 处置措施

| 措施 | 状态 | 说明 |
|---|---|---|
| 微软误报申诉（WDSI） | 待提交 | 提交样本 + 判定信息，请求从模型中剔除。入口：https://www.microsoft.com/en-us/wdsi/filesubmission （实测可达） |
| 代码签名 | 暂缓 | 用户决定先靠申诉+重编绕过；证书未到位 |
| 重编发布 | 已完成 | 0.3.4 → 0.3.5，正常发版流程 |
| 同事应急 | 见下 | |

### 同事应急指引

1. 优先让同事**重新下载**（特征库更新后大概率已放行）
2. 若仍被拦：Windows 安全中心 → 病毒和威胁防护 → 排除项，添加下载目录（需管理员）
3. 兜底：管理员直接拷贝 exe 给同事（避开浏览器下载路径）

## 长期建议

1. **代码签名是根本解法**：有签名 = 有发布者身份，ML 打分大幅放宽，SmartScreen 也不再提示"未知发布者"
2. **发版前加一道 Defender 扫描**：`Start-MpScan -ScanType CustomScan -ScanPath <exe>`，命中则先申诉再发
3. **同一二进制保留多版本备份**：服务端 `cleanupLauncherReleases` 只保留最新一个 exe，
   本次 0.3.1 就是被它清理后才想备份，已丢失（只能从 GitHub Release 重新取）

## 复现与验证命令

```powershell
# 查命中记录
Get-MpThreatDetection | Where-Object { $_.ThreatID -eq 251873 } |
  Select-Object InitialDetectionTime, Resources

# 查判定详情
Get-MpThreat | Where-Object { $_.ThreatID -eq 251873 } | Format-List *

# 对候选 exe 主动扫描（复现判定）
Start-MpScan -ScanType CustomScan -ScanPath <exe路径>

# 检查文件是否被 AV 锁定（被拦时报 0x800700e1）
certutil -hashfile <exe路径> SHA256

# 特征库版本与更新事件
Get-MpComputerStatus | Select-Object AntivirusSignatureVersion, AntivirusSignatureLastUpdated
Get-WinEvent -LogName 'Microsoft-Windows-Windows Defender/Operational' |
  Where-Object { $_.Id -eq 2000 } | Select-Object -First 5 TimeCreated, Message
```

## 环境事实

- 判定时点特征库：`1.459.182.0`（2026-09-13 03:35:46）
- 放行后特征库：`1.459.187.0`（2026-09-13 11:13:56，事件时间 18:03:56）
- 引擎版本：`1.1.26080.3`
- 本机非域环境（`WORKGROUP`），Defender 非集中策略管理
- 所有版本均**无代码签名**
