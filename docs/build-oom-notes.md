# 本机 Rust release 构建 OOM/崩溃备忘（0xc0000409）

> 场景：deepseek-harness-launcher `cargo build --release` 编译巨型 Windows 绑定 crate
> （windows / windows-sys，tauri 栈传递依赖）时 rustc 崩溃 `0xc0000409 STATUS_STACK_BUFFER_OVERRUN`，
> 且间歇性——有时 -j 1 能过，有时连 opt-level=1 也崩。

## 结论（实测，非猜测）

1. **不是"全 feature"问题**：windows/windows-sys 的 feature 是 tauri 生态按需声明的子集
   （tao 窗口 + webview2-com + winrt 通知所需几十个 Win32/Data/UI 子模块）。
   编译命令里那串巨型 `--check-cfg 'cfg(feature, values("AI", ...))'` 只是 rustc **允许的
   feature 名清单**，不是启用清单——实际启用的只有命令开头 `--cfg feature="..."` 那几十个。
   **不能砍**（都是 Tauri 运行必需），砍了 launcher 就编不过/跑不了。

2. **真正根因 = 构建期间内存压力**：
   - 机器 30GB 物理内存，页面文件 C: 14.5GB（F: 有残留 `0 0` 配置但未用）；
   - 后台多任务（node 4GB + 多个并行构建 job + 系统）把可用内存压到 ~8GB 时，
     rustc 编译 windows 巨型 crate 分配失败 → 崩；
   - 空闲（可用 17GB+）时同样命令能成功（实测 11m36s 完成 exit 0）。

3. **rust-lld 对本次崩溃无效**：崩在 rustc **编译期**（crate 内部分配/栈），非链接期；
   且改 linker（RUSTFLAGS 或 .cargo/config.toml）会触发**全量重编缓存失效**，
   反而增加崩溃机会——已实验并回退。

## 可靠构建姿势

```bash
cd src-tauri
# 关键：串行 + 确保系统内存空闲（别同时跑多个构建/大进程）
cargo build --release -j 1
```

- `-j 1` 串行：同一时刻只有一个 rustc，峰值内存最低（并行会叠加多个巨型 crate）。
- 构建前确认可用内存充足（`Get-CimInstance Win32_OperatingSystem` 看 FreePhysicalMemory ≥ 12GB）。
- 失败后直接重跑同命令即可（cargo 续编已完成部分；间歇性崩溃多试 1-2 次能过）。
- 不要用 RUSTFLAGS 改 linker / .cargo/config.toml 配 rust-lld 来"解决"——会全量重编且不解决问题。

## 若想进一步降低峰值（可选，未采用）

对 windows/windows-sys 等巨型 crate 单独降 opt 或调 codegen-units 理论上可降内存，
但实测 opt-level=1 仍崩（内存压力是主因非 opt），且会让 Cargo.lock/profile 指纹变化
触发全量重编——收益不确定，暂不启用。最稳仍是 `-j 1` + 内存空闲。
