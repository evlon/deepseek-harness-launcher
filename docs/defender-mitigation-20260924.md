# Defender 误报缓解：证书导入去静默化 + 自签代码签名

> 2026-09-24 更新。本文是 `defender-false-positive.md`（2026-09-13）的**续篇**，
> 记录本轮两项落地：**B-1 证书导入去静默化** 与 **A 自签代码签名**。
> 前篇结论（`!ml` 启发式误报、间歇性、代码签名是根本解法）依然成立。

## 一、本轮新证据：威胁升级了

9-13 命中的是 `Program:Win32/Contebrew.A!ml`（Severity 4，高）。**9-24 命中的是更严重的：**

```
ThreatID   : 2147780195
ThreatName : Trojan:Win32/Sabsik.FL.A!ml     ← 标成"木马"，Severity 5 = 严重
对象        : target\release\deepseek-harness-launcher.exe
              + 从 conf.ai.ict.cmcc 下载的 launcher-0.5.0.exe
```

仍是 `!ml` 后缀（机器学习启发式），但分类从「程序」升级到「木马」。**这说明
未签名的 exe 每次发版哈希变化后重新被 ML 打分，误报在加重而非缓解。** 必须从
「打地鼠（申诉+重编）」转向「去特征（软化行为）+ 加签名」。

## 二、触发 Defender 启发式的高危行为（源码证据）

ML 模型按行为画像打分。launcher 以下 5 条行为叠加，是典型「可疑程序」画像：

| # | 行为 | 源码位置 |
|---|---|---|
| 1 | **自我替换 + 进程再生**：下载新 exe → 把运行中的自己 rename 成 `.old` → 新文件改名成正式名 → 重新拉起自己 | `self_update.rs:269`（`CREATE_NO_WINDOW`） |
| 2 | **静默提权 + 改系统信任库**：`ShellExecuteExW("runas")` 弹 UAC，`SW_HIDE` 隐藏窗口执行 `certutil -addstore -f Root` | `install.rs` `elevate_certutil_addstore`（`nShow: SW_HIDE`） |
| 3 | `taskkill /T /F` 杀进程树 | `workflow.rs` 头部注释、`dsh_npm.rs`、`mirror.rs` |
| 4 | 静默下载并执行 node/pnpm/git 二进制 | `download.rs`、`install.rs` |
| 5 | 到处 `CREATE_NO_WINDOW` / `SW_HIDE` 隐藏窗口 | `workflow.rs:380`、`mirror.rs:694`、`dsh_versions.rs:266` 等 |

单独任何一条都正常，**五条同时出现在一个未签名 exe 里**，启发式置信度才飙高。
「未签名」是压垮的最后一根稻草——签名程序即使行为相似，Defender 也会因
「有发布者」而大幅降级告警。

## 三、B-1：证书导入去静默化（已落地）

**改动**：`src-tauri/src/install.rs` 的 `install_root_ca()` 里，在 `elevate_certutil_addstore`
之前插入 `confirm_root_ca_import()` —— 一个原生 `MessageBoxW`（`MB_OKCANCEL + MB_ICONINFORMATION`）弹窗：

- 说明「接下来要装内网根证书、需要管理员权限、下一步会弹 UAC」
- 讲清「为什么需要」（否则浏览器红锁）和「点否的后果」（可稍后在托盘重试）
- 用户点「确定」才走 runas 提权；点「取消」则友好返回，不再静默提权

**效果**：去掉「静默提权」这个最刺眼的可疑动作——用户在被提权前**先知情**，
减少「一闪而过的 UAC」造成的困惑，也降低被安全软件误判为「隐蔽提权」的可能。

**注意**：这只覆盖「导入根证书」这一处提权动作；自我替换、下载执行等属于
核心功能（同事零操作升级），本轮未动（见第六节取舍）。

## 四、A：自签代码签名（已落地）

### 4.1 证书套件（放在仓库外，绝不入库）

```
E:\ai-works\.launcher-signing\     ← 不在 git 内
├── signing-ca.key / signing-ca.crt   自签代码签名根 CA（10 年）
├── codesign.key / codesign.crt       签名者证书（3 年，EKU=codeSigning）
└── codesign.pfx                      供 signtool 用的 PKCS#12（密码 ICT@2026）
```

证书指纹（SHA1，signtool 用）：`5FFC6129CF2C26BE34FC42BCFCD92F52CF2ED27C`

### 4.2 签名脚本

`scripts/sign-launcher.sh` —— 用 signtool `/f` 方式签名（可移植、可进 CI）：

```bash
COD_SIGN_PFX=/e/ai-works/.launcher-signing/codesign.pfx \
COD_SIGN_PW=ICT@2026 \
  bash scripts/sign-launcher.sh [exe路径]
```

- 缺省签 `src-tauri/target/release/deepseek-harness-launcher.exe`
- 用 SHA256 摘要 + RFC3161 时间戳（DigiCert；构建机无外网可去掉 `/tr /td`）
- **未设环境变量时跳过签名（exit 0），不影响任何人/CI 正常构建**

`tauri.conf.json` 加了 `bundle.windows.signCommand` 指向该脚本（可选，未配证书时静默跳过）。

### 4.3 ⚠️ 如实说明：自签签名的边界

**自签签名 ≠ 消除 Defender 误报。** 必须讲清楚，避免误以为问题已解决：

| 事实 | 说明 |
|---|---|
| Defender 信誉主要来自 **EV/OV 证书**（购买 + KYC 审核）+ 累计下载量 | 自签证书无第三方背书，SmartScreen 仍显示「未知发布者」 |
| 但自签**去掉了「无签名」红旗** | 比「完全未签名」好，`!ml` 打分可能略降 |
| **内网场景有真实价值** | 若 IT 把 `signing-ca.crt` 统一部署到同事机器的「受信任的发布者」存储，自签即被信任 |
| **终极方案是 EV/OV 证书** | DigiCert/GlobalSign 等，约 $200-400/年，需企业资质 |

**结论**：自签是「比没有好」的正确第一步，但**根治仍需正规证书 + 申诉 + 软化行为三管齐下**。

## 五、待办 / 下一步

| # | 项 | 状态 |
|---|---|---|
| 1 | **微软 WDSI 误报申诉**（提交 `Sabsik.FL.A!ml` 样本） | 待提交，每次发版新哈希都要重提 |
| 2 | **软化解更多高危行为**（自我替换、下载执行、taskkill） | 需权衡「同事零操作升级」取舍，见下 |
| 3 | **EV/OV 证书** | 需企业决策（预算 + 资质），长期根治 |
| 4 | **内网部署签名根 CA 到「受信任的发布者」** | 让自签真正生效，需 IT 配合 |

## 六、一个需要你权衡的取舍

最触发 ML 的是「**自我替换 + 进程再生**」（自动更新自己）。这是你**最想要的
能力**（同事零操作升级）。要彻底去掉这个特征，就得把自更新改成「下载后提示
用户手动确认重启」——失去静默升级。这个是否要动，等你拍板。

本轮只动了「证书导入」这一处提权（B-1），因为它是**纯提权、可去静默化、不损
功能**；自我替换/下载执行属于核心功能，未动。

## 七、验证命令

```powershell
# 查当前命中（本机）
Get-MpThreatDetection | Select-Object ThreatID,InitialDetectionTime,Resources
Get-MpThreat | Where-Object ThreatID -eq 2147780195 | Format-List ThreatName,SeverityID

# 签名后确认
Get-AuthenticodeSignature <exe> | Format-List Status,StatusMessage,SignerCertificate

# 手动签名
COD_SIGN_PFX=E:\ai-works\.launcher-signing\codesign.pfx COD_SIGN_PW=ICT@2026 bash scripts/sign-launcher.sh
```
