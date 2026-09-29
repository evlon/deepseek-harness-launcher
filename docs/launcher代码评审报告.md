# DeepSeek Harness Launcher 代码评审报告（稳健性 / 可靠性）

> 评审范围：`deepseek-harness-launcher/src-tauri/src/`（30 个 Rust 文件，约 1.2MB）+ 服务端 `dsh-launcher-center/src/`。
> 评审方式：两个评审子代理分别对「安装与启动链路」（install/dsh_npm/workflow/download/matrix_setup）与「托盘/同步/配置/自更新/镜像/管理网关/激活」链路（tray/sync/config/self_update/mirror/admin_bridge/ops/activation）完整精读。
> 结论：客户端错误处理整体尚可（Result + 错误码、少裸 panic），但**进程管理、下载原子性、配置写回、自更新替换、管理网关并发**存在多处高风险问题，与你「launcher 问题较多」的判断一致。

---

## 一、高风险问题（会直接导致「更新不顺利 / 安装卡死 / 配置丢失 / 状态错乱」）

### 1.1 进程管理与安装

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| H1 | `dsh_npm.rs:378-381` | **pnpm 超时死锁**：`t.join()` 在 `kill_tree(pid)` 之前，超时后线程等子进程退出、子进程等被杀 → 永久卡死 | 先 kill_tree 再 join |
| H2 | `workflow.rs:523-543` | **stop() 探测错误端口**：无 RUNNING 记录时用默认 3180 探测，实例被分配到 3181+ 时误报「无进程」、文件锁不释放 | 恢复落盘的真实端口或范围探测 |
| H3 | `workflow.rs:302-323` | **profile 切换竞态**：stop() 后不等待进程退出就 spawn，新旧 Harness 并存 | stop 后轮询 pid_alive 再 spawn |
| H4 | `matrix_setup.rs:709-714` | **spawn_blocking 内 block_on(install_all)**：blocking 线程池饿死/死锁，激活向导卡死 | 改 async 上下文 await |
| H5 | `download.rs:74` | **Node sha256 校验被 .ok() 吞掉**：SHASUMS 抓取失败时 Node 包完全不校验就解压（唯一可跳过校验的组件）| 失败重试或硬编码 NODE_VERSION 的 sha256 |
| H6 | `download.rs:459-465` | **commit 非原子**：先删旧目录再 rename，中断丢组件无回滚 | 改「备份→rename→删备份」三段式 |

### 1.2 配置写回（本次评审最重要的发现）

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| **H7** | `config.rs:1115-1140` | **`save_config` 把「服务端下发的值」也写回用户文件，破坏「用户显式设置 vs 服务端下发」边界**：服务端下发 npmRegistry=A → save_config 落盘 → 重启后 `user_set_fields` 把 npmRegistry 误判为「用户显式设置」→ 服务端以后改成 B 也永远覆盖不了。**配置治理失效，直接影响你的版本管理方案落地** | 服务端下发值不写回用户文件；单独持久化「用户显式设置字段」清单，或 save_config 只写用户/内置来源字段 |
| **H8** | `config.rs:1130-1140` | **`save_config` 非原子写**：直接 `fs::write`，崩溃/断电留半截 JSON → 下次反序列化失败回退内置默认，**用户所有配置（serverUrl/dshHome/token）全部丢失**（而同文件域名迁移已用 tmp+rename，明显不一致）| 改 tmp+rename + 可选 fsync |

### 1.3 launcher 自更新

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| **H9** | `self_update.rs:298-323` | **exe 替换非原子 + 回滚吞错**：rename 旧→.old 后 rename 新失败，回滚用 `let _` 吞掉；最坏情况旧 exe 删了、新 exe 没就位，**launcher 直接丢失无法启动** | 确认新文件就位后才删 .old；回滚失败保留 .old 并告警 |
| H10 | `self_update.rs:285-346` | **固定 800ms sleep 等旧进程退出**，无可靠同步；替换失败 return 1 后注释称「下次启动重试」但**无对应恢复 .new 的代码** | 子进程轮询旧进程句柄 signaled，或用 .new 恢复逻辑 |

---

## 二、中风险问题

### 2.1 下载与安装

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M1 | `download.rs:446-456` | `remove_path_if_exists` 裸 remove_dir_all 无 junction 防护，可能误删运行时 | 复用 dsh_npm.rs 的 junction-safe 删除 |
| M2 | `download.rs:270-287` | 断点续传缓冲污染：服务器 Range 支持不稳时拼接损坏 | 校验 206 才 append |
| M3 | `dsh_npm.rs:216-228` | backup 残留不恢复不清理，崩溃后旧版本静默丢失 | 启动前扫描恢复/清理 .replaced-* |
| M4 | `dsh_npm.rs:355-383` | wait_with_output 全量缓冲 pnpm 输出，可撑爆内存 | 流式读取/限流 |
| M5 | `install.rs:386-429` | repair_missing_bundle_patches 手拼 YAML 非原子无锁，特殊字符生成非法 YAML | serde_yaml + atomic_write |
| M6 | `install.rs:503-525` | materialize_launcher_brand 非原子写，崩溃留半截误判已就绪 | tmp+rename |

### 2.2 进程与矩阵向导

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M7 | `workflow.rs:546-572` | port_listener_pid 用 netstat 文本解析，`:3180` 误匹配 `:31801` | 精确匹配端口 |
| M8 | `workflow.rs:574-593` | Unix kill -pid 杀进程组但 spawn 未 setsid，插件子进程残留 | setsid 或声明仅 Windows |
| M9 | `workflow.rs:43,66-85` | PID 复用竞态，恢复的 pid 被复用误杀无辜进程 | 恢复时校验进程镜像名 |
| M10 | `matrix_setup.rs:562-576` | atomic_write 固定 tmp 名，并发写 settings.yaml 冲突 | tmp 名加 pid+随机 |
| M11 | `matrix_setup.rs:667+` | scheme handler 无鉴权，任意本地网页可触发激活/写配置 | 写端点加来源校验 |
| M12 | `matrix_setup.rs:592-634` | wait_matrix_ready 用日志字符串 contains 判成功，插件升级改文案即永远超时 | mtime + 结构化状态 |

### 2.3 同步（sync.rs）

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M13 | `sync.rs:1196,1208` | `LAST_NOTIFIED.lock().unwrap()` 毒化即 panic，**杀死常驻同步循环且无自愈** | 改毒化容忍 unwrap_or_else |
| M14 | `sync.rs:917-1068` | apply_server_defaults 与 save_state 非原子，save_state 直接 fs::write 丢 server_seen_plugins 历史 | tmp+rename 原子写 |
| M15 | `sync.rs:161-191` | 自造伪 UUID 作 client_id，熵不足跨进程碰撞 | 用 getrandom（依赖已就绪）|
| M16 | `sync.rs:347-438` | 插件版本查询串行，20 插件×3 源最坏数分钟，卡住同步 | futures::join_all 限并发 |
| M17 | `sync.rs:1102-1141` | envDefaults/jobPresets 写 settings.yaml 不参与「是否变化」判断 | 语义澄清或补判定 |

### 2.4 自更新（self_update.rs）

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M18 | `self_update.rs:461-469` | sha256 为空时既无校验也无防循环更新 | sha256 空时拒绝更新 |
| M19 | `self_update.rs:79-98` | has_newer 解析失败「保守视为需要更新」→ 非法版本号无限下载-替换循环 | 解析失败视为「跳过」 |

### 2.5 镜像与管理网关

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M20 | `mirror.rs:646-683` | tmp 目录名只含 PID 跨包复用，find_tarball 找第一个 tgz，残留会 pack 错包 | 每包唯一 tmp + finally 清理 |
| M21 | `mirror.rs:640-683` | npm view 文本首行精确匹配，npm 版本差异误判「已存在」 | 用 --json 解析 |
| M22 | `admin_bridge.rs:32-49,109` | CURRENT 锁在 stop() 里持锁 join 死锁 + 裸 unwrap | stop 先 take 再 join；毒化容忍 |
| M23 | `admin_bridge.rs:158-183` | 请求超时后 worker 线程 detached 永久泄漏，常驻机反复超时线程爆炸 | 可取消 IO/信号量限制并发 |
| M24 | `admin_bridge.rs:625-665` | exec_script 的 timeoutMs 被 `let _` 丢弃，脚本可无限执行 | 实现真超时 kill |
| M25 | `admin_bridge.rs:226-233` | X-Bridge-Token 头声明了却不校验，只查 query token | 实现 header 校验 |

### 2.6 托盘与激活

| # | 位置 | 问题 | 建议 |
|---|---|---|---|
| M26 | `tray.rs:211-391` | **菜单项用运行时索引映射点击处理**（plugin-uninstall-{i} 等），构建与点击间索引漂移会**卸载/安装错插件** | ID 用插件名而非索引 |
| M27 | `tray.rs:76-99` | pending_uninstall 全局单槽，跨菜单项互相覆盖待确认态 | 改 HashMap |
| M28 | `activation.rs:130` | getrandom 熵源异常 panic | 返回 Result 给可读错误 |
| M29 | `activation.rs:207,365` | 完整 auth_url（含 state/code_challenge）落日志 | 日志打码敏感参数 |
| M30 | `activation.rs:684-697` | token 端点响应体前 200 字符拼进错误消息展示给用户 | 错误消息不拼响应体 |

---

## 三、低风险 / 可维护性问题（摘要）

- `install.rs:798-811` MessageBoxW 无父窗口、无记住选择；`install.rs:643-710` UAC 提权无限等待无超时；`install.rs:932-948` pnpm shim 内容固定；`install.rs:199-251` 版本缓存空时重复安装。
- `workflow.rs:245-254` find_available_port 无上限/随机化；`workflow.rs:664-673` 每次 launch 读盘 semver parse。
- `matrix_setup.rs:915-995` /jobs 四次读改写非事务；`matrix_setup.rs:772-826` /resume-auto-activation 废弃死代码。
- `config.rs:346` base_dir expect 级联 panic；`config.rs:912-925` load_cached 未初始化返回空配置非内置默认；`config.rs:990-1010` merge_user_into_builtin 字段白名单易漏接线。
- `sync.rs:543` pending_plugins 排版缺陷（`{` 与 `let` 同列）；`sync.rs:426` insert 后 unwrap。
- `tray.rs` 多处 load_cached 重复调用；`tray.rs:117-123` open_url 失败静默。
- `ops.rs:326-334` state_path 依赖 load_cached；`ops.rs:381` persist 两次独立加锁混合快照。
- `mirror.rs:573-609` done_pkgs 跳过也算完成语义混乱；`mirror.rs:462-466` tokio runtime 创建失败 expect。
- `admin_bridge.rs:416-432` 手写 percent_decode 不处理 `+`。
- `activation.rs:441-447` owner_from_user_id 只处理 @ai- 前缀无校验。
- `dsh_npm.rs:389-403` npm_registry_for_install 未读 clientDefaults.dshRegistry（与项目记忆一致的已知 bug）。
- 多处 `RUNNING.lock().unwrap()` / `CURRENT.lock().unwrap()`（约 15+ 处），建议统一毒化容忍。

---

## 四、做得好的地方（评审正面结论）

1. 错误处理普遍 `Result<(), String>` + 具体错误码，运行时路径几乎无裸 panic。
2. `dsh_npm.rs` 的「原地安装 + 回滚」机制（解决 staging+rename 链接断裂）设计扎实，注释详尽。
3. `dsh_npm.rs:415-430` junction-safe remove_dir_all；`install.rs` 对 symlink/junction 处理有详细注释。
4. `workflow.rs` PID 落盘恢复 + token 惰性补抓 + 端口就绪轮询设计周到。
5. `ops.rs` 状态机测试覆盖全（状态迁移、失败归档、日志截断、旧格式兼容）。
6. 大量历史踩坑有注释记录，可维护性意识强。
7. `config.rs` 域名迁移已用 tmp+rename 原子写（可惜 save_config 自己没对齐）。

---

## 五、TOP 10 最该修的问题（跨两组合并排序）

| 优先级 | 问题 | 为何最该修 |
|---|---|---|
| 1 | **config.rs save_config 写回破坏「用户显式 vs 服务端下发」边界（H7）** | 配置治理失效，直接影响三版本方案落地 |
| 2 | **config.rs save_config 非原子写（H8）** | 崩溃丢全部用户配置 |
| 3 | **dsh_npm.rs pnpm 超时死锁（H1）** | 安装卡死后 launcher 无响应 |
| 4 | **self_update.rs exe 替换非原子+回滚吞错（H9）** | 最坏 launcher 丢失无法启动 |
| 5 | **download.rs Node sha256 被吞（H5）** | 供应链/完整性风险 |
| 6 | **workflow.rs stop() 探测错误端口（H2）** | 文件锁不释放，更新/切换失败 |
| 7 | **workflow.rs profile 切换竞态（H3）** | 新旧 Harness 并存 |
| 8 | **admin_bridge.rs 锁死锁 + 线程泄漏（M22/M23）** | 常驻服务卡死/线程爆炸 |
| 9 | **sync.rs 同步循环裸 unwrap 毒化停摆（M13）** | 同步一旦停摆无自愈 |
| 10 | **tray.rs 菜单索引漂移卸错插件（M26）** | 点了 A 卸载 B |

---

## 六、与你「版本管理方案」直接相关的评审结论

1. **H7（配置写回污染边界）是版本方案的头号障碍**：三版本方案里「服务端下发版本组合、客户端不覆盖用户显式设置」这个前提，当前 `save_config` 实现**已经破坏了**——必须先修 H7，否则服务端改版本组合，历史下发过的字段永远覆盖不了。
2. **H8（save_config 非原子）会让「下发版本组合」在崩溃时丢配置**，等于版本方案的地基不稳。
3. **M26（托盘菜单索引漂移）** 在「版本切换」菜单场景尤其危险——用户切换版本时可能切到错误的版本。
4. 现有 `dshChannel`（rc/alpha）升级为三通道时，要注意 `apply_server_overrides`（config.rs:1069-1098）里 dshChannel/dshVersion 是「企业统一管理、不遵循用户显式不覆盖」的**特例**，三通道也应延续这个语义，但要与 H7 的「不写回」机制配合好。

---

> 完整的问题清单（含精确行号）共 50+ 条，本报告汇总了高风险 10 条、中风险 30 条、低风险 20+ 条。如需针对某条展开修复方案或补测试，请告知。
