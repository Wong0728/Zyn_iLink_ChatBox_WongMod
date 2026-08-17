// 内网穿透（SSH 隧道）管理模块
// 通过系统 ssh 客户端建立 Serveo/SSH 隧道，将本地服务暴露到公网。
//
// 可靠性契约（v2 修正）：
//   - ssh 参数追加 ExitOnForwardFailure=yes + ServerAliveInterval/CountMax：
//     远端绑定失败或链路死亡（NAT 超时/断网）都会让 ssh 进程退出，
//     从而驱动 reader 重连，而不是连接挂着但隧道假死。
//   - 转发目标固定 127.0.0.1（而非 localhost）：双栈机器上 localhost 可能
//     先解析到 ::1，若 ::1 被防火墙 DROP，每个转发连接都要等超时才回退。
//   - 重连为指数退避 + 抖动（5s → 封顶 120s），永不放弃；连接稳定 60s
//     以上清零退避计数，避免固定 5s 重连风暴被服务端限流。
//   - 每次拉起新 ssh 都清空 public_url 重新提取：serveo 随机子域名在
//     每次重连后都会变化，残留旧 URL 会误导用户访问已失效地址。
//   - generation 计数 + 按 PID 回收子进程：stop→start 快速切换时，
//     旧 reader 线程不会误杀/覆盖新一轮隧道。
//   - Windows 下 ssh 挂入 KILL_ON_JOB_CLOSE Job Object：主进程崩溃/
//     退出时 ssh 一并终止，不留守孤儿进程占住 serveo 子域名。

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::config;

/// 隧道配置文件名（放在 config::base_dir() 下，与其它配置一致）。
const TUNNEL_CONFIG_FILENAME: &str = "tunnel_config.json";

/// 默认本地端口（与 Web 服务端口一致）。
const DEFAULT_LOCAL_PORT: u16 = 8888;
/// 重连退避：基础间隔、上限（指数退避 + 抖动，替代固定 5s 重连风暴）。
const RECONNECT_BASE_SECS: u64 = 5;
const RECONNECT_MAX_SECS: u64 = 120;
/// 连接存活超过该时长视为"稳定连接"，重置退避计数。
const STABLE_UPTIME_SECS: u64 = 60;

/// 隧道是否活跃（有 ssh 进程在跑或重连等待中）。
/// web 层据此把 loopback 直连视为可信代理，从 X-Forwarded-For 还原隧道访客的
/// 真实 IP——否则所有公网访客都表现为 127.0.0.1，限流/封禁折叠到同一个桶
/// （任何人多输错几次密码即可锁死全站登录）。
static TUNNEL_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 当前隧道公网 origin（如 https://xxx.serveo.net）。
/// web 层 Origin/CSRF 校验据此放行经隧道访问的浏览器：其 Origin 是隧道域名，
/// 不在默认 loopback 白名单内；真实 IP 还原后这些访客不再表现为 loopback，
/// 不放行会误杀全部隧道访客的登录 / WebSocket。
static PUBLIC_ORIGIN: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

/// 隧道是否处于活跃状态（Running 或重连等待中）。
pub fn tunnel_active() -> bool {
    TUNNEL_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

/// 当前隧道公网 origin（https://xxx.serveo.net），未运行/未取得 URL 时为 None。
pub fn public_origin() -> Option<String> {
    PUBLIC_ORIGIN.lock().clone()
}

fn set_tunnel_active(active: bool) {
    TUNNEL_ACTIVE.store(active, std::sync::atomic::Ordering::Relaxed);
}

fn set_public_origin(origin: Option<String>) {
    *PUBLIC_ORIGIN.lock() = origin;
}

/// 隧道持久化配置（JSON 序列化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelConfig {
    pub port: u16,
    pub remote: u16,
    #[serde(default)]
    pub subdomain: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
}

fn default_true() -> bool {
    true
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_LOCAL_PORT,
            remote: 80,
            subdomain: String::new(),
            enabled: false,
            auto_reconnect: true,
        }
    }
}

/// 隧道状态
#[derive(Debug, Clone)]
pub enum TunnelState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error(()),
}

impl PartialEq for TunnelState {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (TunnelState::Stopped, TunnelState::Stopped)
                | (TunnelState::Starting, TunnelState::Starting)
                | (TunnelState::Running, TunnelState::Running)
                | (TunnelState::Stopping, TunnelState::Stopping)
                | (TunnelState::Error(_), TunnelState::Error(_))
        )
    }
}

/// 隧道信息
#[derive(Debug, Clone)]
pub struct TunnelInfo {
    pub state: TunnelState,
    pub local_port: u16,
    pub remote_port: u16,
    pub subdomain: String,
    pub public_url: Option<String>,
    pub pid: Option<u32>,
}

struct TunnelInner {
    child: Option<Child>,
    state: TunnelState,
    local_port: u16,
    remote_port: u16,
    subdomain: String,
    public_url: Option<String>,
    auto_reconnect: bool,
    /// 当前配置是否已保存过（stop 时写 enabled=false）
    persist: bool,
    /// 配置文件中是否启用（stop 时设 false，启动时从文件读取）
    enabled: bool,
    /// 用户是否主动调用 stop()，用来通知 reader 线程不要重连。
    stop_requested: bool,
    /// 连续失败次数（指数退避档位；连接稳定 60s 以上清零）。
    consecutive_failures: u32,
    /// 当前 ssh 子进程启动时刻（判断连接是否"稳定过"）。
    spawned_at: Option<std::time::Instant>,
    /// 启动代数：每次 start() 递增。旧 reader 线程发现代数不匹配即退出，
    /// 防止 stop→start 快速切换后旧重连线程与新隧道互相干扰。
    generation: u64,
    logs: Vec<String>,
    log_capacity: usize,
}

/// 隧道管理器
pub struct TunnelManager {
    inner: Arc<Mutex<TunnelInner>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TunnelInner {
                child: None,
                state: TunnelState::Stopped,
                local_port: DEFAULT_LOCAL_PORT,
                remote_port: 80,
                subdomain: String::new(),
                public_url: None,
                auto_reconnect: false,
                persist: false,
                enabled: false,
                stop_requested: false,
                consecutive_failures: 0,
                spawned_at: None,
                generation: 0,
                logs: Vec::new(),
                log_capacity: 200,
            })),
        }
    }

    pub fn start(&self, local_port: u16, remote_port: u16, subdomain: &str) -> Result<(), String> {
        // 子域名校验：serveo 子域名为 DNS 标签，提前拒绝非法字符（尤其 ':'——
        // 它会改变 ssh -R 转发 spec 的结构），给出明确错误而非让 ssh 解析报错。
        let subdomain = subdomain.trim().to_lowercase();
        if !subdomain.is_empty()
            && !subdomain
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err("子域名仅允许小写字母、数字和连字符（-）".to_string());
        }

        // 检测 ssh 可用性（在持锁外执行，避免阻塞其他调用）。
        let ssh_check = Command::new("ssh")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if ssh_check.is_err() || !ssh_check.unwrap().success() {
            return Err("未找到 ssh 客户端，请安装 OpenSSH".to_string());
        }

        {
            let mut inner = self.inner.lock();
            if matches!(inner.state, TunnelState::Running | TunnelState::Starting) {
                return Err("隧道已在运行中".to_string());
            }
            inner.stop_requested = false;
            inner.state = TunnelState::Starting;
            inner.local_port = local_port;
            inner.remote_port = remote_port;
            inner.subdomain = subdomain.to_string();
            inner.public_url = None;
            inner.consecutive_failures = 0;
            inner.spawned_at = None;
            // 旧 reader 线程以此识别自己已被取代（stop→start 快速切换场景）。
            inner.generation += 1;
            // 必须在 spawn 之前置位：若 ssh 秒退（如无网络），reader 线程读取该
            // 标志决定是否重连；置位过晚会读到 false 而放弃重连，且子进程已被
            // 回收、state 停留 Running，隧道永久假死。
            inner.auto_reconnect = true;
            push_log(&mut inner, "正在启动隧道...".to_string());
        }

        spawn_ssh(Arc::clone(&self.inner))?;

        // 启动成功后持久化（save_config_inner 会依据 Running/Starting 兜底 enabled=true）。
        {
            let mut inner = self.inner.lock();
            inner.persist = true;
            save_config_inner(&inner);
        }
        tracing::info!(
            "[TUNNEL] 隧道已启动: {}:127.0.0.1:{} -> serveo.net",
            remote_port,
            local_port
        );

        Ok(())
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut inner = self.inner.lock();
        if matches!(inner.state, TunnelState::Stopped | TunnelState::Stopping) {
            return Err("隧道未在运行".to_string());
        }
        // 保存配置（enabled=false，仅 persist=true 时写入）
        if inner.persist {
            inner.enabled = false;
            save_config_inner(&inner);
        }
        // 通知 reader 线程不要重连。
        inner.auto_reconnect = false;
        inner.stop_requested = true;
        inner.consecutive_failures = 0;
        inner.spawned_at = None;
        inner.state = TunnelState::Stopping;
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        inner.state = TunnelState::Stopped;
        inner.persist = false;
        // 1.5 fix：清空 public_url，避免前端状态切换瞬时残留旧 URL。
        inner.public_url = None;
        // 通知 web 层：隧道已停，loopback 直连不再视为可信代理、origin 不再放行。
        set_tunnel_active(false);
        set_public_origin(None);
        push_log(&mut inner, "隧道已停止".to_string());
        tracing::info!("[TUNNEL] 隧道已停止");
        Ok(())
    }

    pub fn status(&self) -> TunnelInfo {
        let mut inner = self.inner.lock();
        // 检查子进程是否存活
        if let Some(ref mut child) = inner.child {
            if let Ok(Some(_)) = child.try_wait() {
                if matches!(inner.state, TunnelState::Running) {
                    inner.state = TunnelState::Error(());
                }
            }
        } else if matches!(inner.state, TunnelState::Running) {
            // 防御：child 已被 reader 回收但状态仍停留 Running（状态机失步），
            // 纠正为 Error，避免 UI 永远显示"运行中"。
            inner.state = TunnelState::Error(());
        }
        TunnelInfo {
            state: inner.state.clone(),
            local_port: inner.local_port,
            remote_port: inner.remote_port,
            subdomain: inner.subdomain.clone(),
            public_url: inner.public_url.clone(),
            pid: inner.child.as_ref().map(std::process::Child::id),
        }
    }

    pub fn logs(&self, count: usize) -> Vec<String> {
        let inner = self.inner.lock();
        let c = count.min(inner.logs.len());
        inner.logs[inner.logs.len() - c..].to_vec()
    }

    /// 从本地 JSON 文件加载持久化配置
    pub fn load_config_from_file() -> TunnelConfig {
        let path = tunnel_config_path();
        if !path.exists() {
            return TunnelConfig::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!("[TUNNEL] 解析配置失败: {}，使用默认配置", e);
                TunnelConfig::default()
            }),
            Err(e) => {
                tracing::warn!("[TUNNEL] 读取配置失败: {}，使用默认配置", e);
                TunnelConfig::default()
            }
        }
    }

    /// 尝试从持久化配置自动恢复隧道（启动时调用）
    /// 如果配置中 enabled=true，自动以保存的参数启动隧道。
    pub fn try_restore(&self) {
        let cfg = Self::load_config_from_file();
        if !cfg.enabled {
            tracing::info!("[TUNNEL] 持久化配置未启用，跳过自动恢复");
            return;
        }
        tracing::info!(
            "[TUNNEL] 检测到持久化配置已启用，自动恢复隧道: port={}, remote={}, subdomain={}",
            cfg.port,
            cfg.remote,
            cfg.subdomain
        );
        // 先设置 inner 中的 enabled 标志，让 start 中能读取到
        {
            let mut inner = self.inner.lock();
            inner.enabled = true;
            inner.auto_reconnect = cfg.auto_reconnect;
        }
        if let Err(e) = self.start(cfg.port, cfg.remote, &cfg.subdomain) {
            tracing::error!("[TUNNEL] 自动恢复隧道失败: {}", e);
        }
    }
}

impl Default for TunnelManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 构造并启动 SSH 子进程，挂上 reader 线程。
/// 由 start() 与 reader 重连路径共同调用，避免重复构造逻辑。
fn spawn_ssh(inner: Arc<Mutex<TunnelInner>>) -> Result<(), String> {
    let (subdomain, remote_port, local_port) = {
        let guard = inner.lock();
        // stop 与 start 竞态防护：用户已主动停止时不拉起新进程。
        if guard.stop_requested || !guard.auto_reconnect {
            return Err("隧道已被停止，取消拉起".to_string());
        }
        (guard.subdomain.clone(), guard.remote_port, guard.local_port)
    };
    // 转发目标显式用 127.0.0.1 而非 localhost：双栈机器上 localhost 可能先解析
    // 到 ::1，若服务仅监听 IPv4 需等连接失败后才回退；::1 被防火墙 DROP 时
    // 每个转发连接都要等超时，公网访问整体变慢。
    let forward = if subdomain.is_empty() {
        format!("{}:127.0.0.1:{}", remote_port, local_port)
    } else {
        format!("{}:{}:127.0.0.1:{}", subdomain, remote_port, local_port)
    };

    let mut cmd = Command::new("ssh");
    // 避免首次连接时交互式提示卡住进程
    cmd.arg("-o").arg("StrictHostKeyChecking=accept-new");
    cmd.arg("-o").arg("BatchMode=yes");
    // 远端端口/子域名绑定失败时直接退出（OpenSSH 默认是保持连接但隧道假死），
    // 退出会被 reader 线程感知并进入重连循环，状态机得以自愈。
    cmd.arg("-o").arg("ExitOnForwardFailure=yes");
    // 探测死链路：NAT 超时/网络切换造成的半开连接在 ~90s 内被判定断开，
    // 否则 stdout 永不关闭、重连逻辑永不触发（TCP keepalive 默认约 2h 不可靠）。
    cmd.arg("-o").arg("ServerAliveInterval=30");
    cmd.arg("-o").arg("ServerAliveCountMax=3");
    cmd.arg("-R").arg(&forward);
    cmd.arg("serveo.net");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    match cmd.spawn() {
        Ok(mut child) => {
            // Windows：挂入 KILL_ON_JOB_CLOSE Job Object，主进程退出时一并终止。
            assign_child_to_kill_on_close_job(&child);
            let reader_stdout = child.stdout.take();
            let reader_stderr = child.stderr.take();
            let my_child_pid = child.id();
            let my_generation;
            {
                let mut guard = inner.lock();
                // spawn 系统调用期间用户可能已调用 stop()：立即回收，不置 Running。
                if guard.stop_requested || !guard.auto_reconnect {
                    drop(guard);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("隧道已被停止，取消拉起".to_string());
                }
                my_generation = guard.generation;
                guard.child = Some(child);
                guard.state = TunnelState::Running;
                // 每次拉起都清空旧 URL：serveo 分配的（尤其是随机）子域名在重连后
                // 可能变化，残留旧 URL 会误导用户访问已失效地址。
                guard.public_url = None;
                guard.spawned_at = Some(std::time::Instant::now());
                // 通知 web 层：隧道访客真实 IP 需从 XFF 还原；origin 待 reader 提取。
                set_tunnel_active(true);
                set_public_origin(None);
            }
            // 启动 reader 线程：写日志 + 提取 URL + stdout 关闭后自管重连。
            if let Some(stdout) = reader_stdout {
                let inner_clone = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name("ilink-tunnel-reader".into())
                    .spawn(move || {
                        tunnel_reader_loop(inner_clone, stdout, my_generation, my_child_pid);
                    })
                    .ok();
            }
            // 启动 stderr reader 线程，读取 SSH 错误输出并记入日志。
            //   stderr 若被 piped 但不读取，错误完全不可见，且缓冲区填满后会
            //   阻塞子进程。关键错误同时写入 UI 日志（inner.logs）。
            if let Some(stderr) = reader_stderr {
                let inner_clone = Arc::clone(&inner);
                std::thread::Builder::new()
                    .name("ilink-tunnel-stderr".into())
                    .spawn(move || {
                        tunnel_stderr_loop(inner_clone, stderr);
                    })
                    .ok();
            }
            Ok(())
        }
        Err(e) => {
            let mut guard = inner.lock();
            guard.state = TunnelState::Error(());
            push_log(&mut guard, format!("启动失败: {}", e));
            Err(format!("启动隧道失败: {}", e))
        }
    }
}

/// reader 线程主循环：
///   1. 逐行读 stdout 写日志，提取 serveo.net 公网 URL。
///   2. stdout 关闭（即 SSH 子进程退出）后，若 auto_reconnect 且未 stop_requested
///      且未被新一轮 start() 取代（generation 匹配），按指数退避重连，永不放弃。
fn tunnel_reader_loop(
    inner: Arc<Mutex<TunnelInner>>,
    stdout: impl std::io::Read + Send + 'static,
    my_generation: u64,
    my_child_pid: u32,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                {
                    let mut guard = inner.lock();
                    push_log(&mut guard, line.clone());
                }
                tracing::debug!("[TUNNEL/stdout] {}", &line);
                let already_has_url = { inner.lock().public_url.is_some() };
                if already_has_url {
                    continue;
                }
                if let Some(url) = extract_serveo_url(&line) {
                    let mut guard = inner.lock();
                    guard.public_url = Some(url.clone());
                    push_log(&mut guard, format!("公网地址: {}", &url));
                    tracing::info!("[TUNNEL] 公网地址: {}", url);
                    // web 层 Origin 校验放行该隧道域名的浏览器请求。
                    set_public_origin(Some(url));
                }
            }
            Err(_) => break,
        }
    }

    // stdout 关闭 → SSH 子进程已退出。记录退出码以辅助诊断。
    {
        let mut guard = inner.lock();
        // 仅回收自己拉起的子进程：stop→start 快速切换时，inner.child 可能已经
        // 属于新一代隧道，误杀会导致新隧道被旧 reader 关闭。
        let is_my_child = guard
            .child
            .as_ref()
            .map(|c| c.id() == my_child_pid)
            .unwrap_or(false);
        if is_my_child {
            if let Some(ref mut child) = guard.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "(signal)".to_string());
                        push_log(&mut guard, format!("SSH 子进程退出，退出码={}", code));
                        tracing::warn!("[TUNNEL] SSH 子进程退出，退出码={}", code);
                    }
                    Ok(None) => {
                        // 子进程还在运行但 stdout 关闭 — 异常情况，强制 kill
                        let _ = child.kill();
                        let _ = child.wait();
                        push_log(
                            &mut guard,
                            "SSH stdout 关闭但子进程未退出，已强制终止".to_string(),
                        );
                        tracing::warn!("[TUNNEL] SSH stdout 关闭但子进程未退出，已强制终止");
                    }
                    Err(e) => {
                        tracing::warn!("[TUNNEL] wait SSH 子进程失败: {}", e);
                    }
                }
            }
            guard.child = None;
        }
        // 退避计数（仅当前代 reader 才更新，被取代的旧线程不污染新会话状态）：
        // 连接存活超过 STABLE_UPTIME_SECS 视为一次成功连接（清零），
        // 否则（秒退/绑定失败/链路死亡）计数 +1，重连间隔指数增长。
        if guard.generation == my_generation {
            if guard
                .spawned_at
                .map(|t| t.elapsed().as_secs() >= STABLE_UPTIME_SECS)
                .unwrap_or(false)
            {
                guard.consecutive_failures = 0;
            } else {
                guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
            }
            guard.spawned_at = None;
        }
    }

    // 重连主循环（指数退避，永不放弃；被新一轮 start() 取代时静默退出）。
    {
        let mut guard = inner.lock();
        if guard.generation == my_generation {
            guard.state = TunnelState::Starting;
        }
    }
    loop {
        let (failures, proceed) = {
            let guard = inner.lock();
            (
                guard.consecutive_failures,
                guard.auto_reconnect && !guard.stop_requested && guard.generation == my_generation,
            )
        };
        if !proceed {
            // 永久退出：同步清除全局活跃标志（loopback 不再视为可信代理）。
            // 被新一代 start() 取代时除外——标志属于新隧道，不能清。
            if inner.lock().generation == my_generation {
                set_tunnel_active(false);
                set_public_origin(None);
            }
            // 用户主动停止时 stop() 已置 Stopped；此处兜底：既非用户停止、
            // 也未被新一代取代（auto_reconnect 被置 false）时标记 Error，
            // 避免状态停留在 Starting（原实现会停留在 Running）。
            let mut guard = inner.lock();
            if guard.generation == my_generation
                && !guard.stop_requested
                && !matches!(guard.state, TunnelState::Stopped | TunnelState::Stopping)
            {
                guard.state = TunnelState::Error(());
            }
            tracing::info!("[TUNNEL] reader 线程退出（已停止或被新一轮启动取代）");
            return;
        }
        let delay = backoff_delay_secs(failures);
        {
            let mut guard = inner.lock();
            push_log(&mut guard, format!("连接断开，{}s 后自动重连", delay));
        }
        // 分片休眠：每秒检查一次 stop/generation，保证退避期间 stop() 秒级生效。
        if !sleep_while_active(&inner, delay) {
            tracing::info!("[TUNNEL] 重连等待期间被 stop，退出 reader");
            return;
        }
        // 休眠期间可能发生了 stop→start：新一代隧道已由 start() 拉起，本线程退出。
        if inner.lock().generation != my_generation {
            tracing::info!("[TUNNEL] 重连等待期间隧道被重新启动，旧 reader 退出");
            return;
        }
        match spawn_ssh(Arc::clone(&inner)) {
            // spawn_ssh 成功时已启动新 reader 线程，本线程使命完成直接返回。
            Ok(()) => return,
            Err(e) => {
                // spawn 本身失败（如 ssh 被卸载/系统资源不足）：计数 +1，继续退避重试。
                let mut guard = inner.lock();
                guard.consecutive_failures = guard.consecutive_failures.saturating_add(1);
                guard.state = TunnelState::Error(());
                push_log(&mut guard, format!("拉起 SSH 失败: {}，退避后继续重试", e));
                tracing::warn!("[TUNNEL] 自动重连失败: {}，退避后重试", e);
            }
        }
    }
}

/// SSH stderr reader：逐行读 stderr，写入 tracing 日志。
/// 关键错误（转发失败/拒绝访问/连接类错误）同时写入 UI 日志（inner.logs）：
/// ExitOnForwardFailure 触发退出前，用户需要在前端看到真实原因，
/// 而不是只看到"连接断开，重连中"。其余行仅进 tracing，避免刷屏占满日志容量。
fn tunnel_stderr_loop(inner: Arc<Mutex<TunnelInner>>, stderr: impl std::io::Read + Send + 'static) {
    let reader = BufReader::new(stderr);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }
                let lower = line.to_lowercase();
                let is_forward_failure = lower.contains("remote port forwarding failed");
                let is_important = is_forward_failure
                    || lower.contains("permission denied")
                    || lower.contains("connection refused")
                    || lower.contains("timed out")
                    || lower.contains("connection reset")
                    || lower.contains("connection closed");
                if is_important {
                    let mut guard = inner.lock();
                    push_log(&mut guard, format!("SSH: {}", line));
                    if is_forward_failure {
                        push_log(
                            &mut guard,
                            "提示：80 端口在 serveo.net 上是共享的，建议在管理面板填写一个唯一的 subdomain 后再启动"
                                .to_string(),
                        );
                    }
                }
                if is_forward_failure {
                    tracing::warn!(
                        "[TUNNEL/stderr] SSH 远程端口转发失败（端口可能被占用）：{}",
                        line
                    );
                } else {
                    tracing::warn!("[TUNNEL/stderr] {}", line);
                }
            }
            Err(_) => break,
        }
    }
}

/// 将 TunnelInner 的当前配置写入 JSON 文件
fn save_config_inner(inner: &TunnelInner) {
    let config = TunnelConfig {
        port: inner.local_port,
        remote: inner.remote_port,
        subdomain: inner.subdomain.clone(),
        enabled: inner.enabled
            || matches!(inner.state, TunnelState::Running | TunnelState::Starting),
        auto_reconnect: inner.auto_reconnect,
    };
    let path = tunnel_config_path();
    match serde_json::to_string_pretty(&config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                tracing::warn!("[TUNNEL] 保存配置失败: {}", e);
            }
        }
        Err(e) => tracing::warn!("[TUNNEL] 序列化配置失败: {}", e),
    }
}

/// 隧道配置文件完整路径（走 config::base_dir，与其他配置一致）。
fn tunnel_config_path() -> PathBuf {
    config::base_dir().join(TUNNEL_CONFIG_FILENAME)
}

/// 指数退避：5s × 2^(n-1)，封顶 120s，叠加 ±20% 抖动。
/// 抖动避免多实例/多部署同步重连形成风暴，被服务端限流封禁。
fn backoff_delay_secs(failures: u32) -> u64 {
    let shift = failures.saturating_sub(1).min(5);
    let base = RECONNECT_BASE_SECS
        .saturating_mul(1u64 << shift)
        .min(RECONNECT_MAX_SECS);
    let jitter_factor = 0.8 + 0.4 * rand::random::<f64>(); // [0.8, 1.2)
    ((base as f64) * jitter_factor).round().max(1.0) as u64
}

/// 分片休眠并保持响应：每秒检查一次是否仍应重连（未被 stop、未被新一代取代）。
/// 返回 false 表示休眠期间隧道已被停止。
fn sleep_while_active(inner: &Arc<Mutex<TunnelInner>>, total_secs: u64) -> bool {
    let mut remaining = total_secs;
    while remaining > 0 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let guard = inner.lock();
        if !guard.auto_reconnect || guard.stop_requested {
            return false;
        }
        remaining = remaining.saturating_sub(1);
    }
    true
}

/// Windows：把 ssh 子进程挂进 KILL_ON_JOB_CLOSE 的 Job Object，
/// 主进程退出/崩溃时内核自动终止 job 内进程，避免孤儿 ssh 留守并
/// 持续占用 serveo 子域名（重启后新旧隧道冲突）。
/// job 句柄存入 OnceLock 故意不关闭：句柄随主进程生命周期保活，
/// 进程退出句柄销毁时触发 job 内进程终止。非 Windows 平台为空操作。
#[cfg(windows)]
fn assign_child_to_kill_on_close_job(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // HANDLE 为裸指针（非 Send/Sync），以 usize 存放；整个进程复用同一个 job。
    static SSH_JOB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let job = *SSH_JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            tracing::warn!("[TUNNEL] CreateJobObject 失败，孤儿 ssh 进程防护未生效");
            return 0;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            tracing::warn!(
                "[TUNNEL] SetInformationJobObject 失败：{}，孤儿 ssh 进程防护未生效",
                std::io::Error::last_os_error()
            );
            return 0;
        }
        job as usize
    });
    if job == 0 {
        return;
    }
    let ok = unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    if ok == 0 {
        tracing::warn!(
            "[TUNNEL] AssignProcessToJobObject 失败：{}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(not(windows))]
fn assign_child_to_kill_on_close_job(_child: &Child) {}

/// 从一行文本中提取 serveo.net 公网 URL。
/// 查找 "https://" 后跟字母、数字、点、连字符，包含 "serveo.net" 的完整 URL。
/// 注意：定位与切片必须在同一个字符串上进行——原实现把 to_lowercase() 后的
/// 偏移量用于切片原文（两者字节长度可能不同，如 'İ'），会 panic。
fn extract_serveo_url(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    let marker = "https://";
    let mut search_start = 0;
    while let Some(pos) = lower[search_start..].find(marker) {
        let abs_start = search_start + pos;
        // 从 https:// 之后查找 URL 结束位置（空白字符或行尾）
        let remaining = &lower[abs_start..];
        let end = remaining
            .find(|c: char| c.is_whitespace())
            .unwrap_or(remaining.len());
        let candidate = &remaining[..end];
        // 检查是否包含 serveo.net
        if candidate.contains("serveo.net") || candidate.contains(".serveo.") {
            return Some(
                candidate
                    .trim_end_matches('/')
                    .trim_end_matches(':')
                    .to_string(),
            );
        }
        search_start = abs_start + marker.len();
    }
    None
}

fn chrono_local_now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// 写日志并裁剪到 log_capacity。
fn push_log(inner: &mut TunnelInner, msg: String) {
    let cap = inner.log_capacity;
    inner.logs.push(format!("[{}] {}", chrono_local_now(), msg));
    let overflow = inner.logs.len().saturating_sub(cap);
    if overflow > 0 {
        inner.logs.drain(0..overflow);
    }
}
