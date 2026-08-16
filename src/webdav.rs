// WebDAV 远程媒体存储客户端
// 把媒体二进制卸载到 WebDAV，节省小服务器磁盘

use base64::Engine;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::Method;
use std::collections::HashSet;
use std::time::Duration;

use crate::bot::safe_truncate;

#[derive(Debug, thiserror::Error)]
pub enum WebDavError {
    #[error("{0}")]
    Msg(String),
    #[error("网络错误: {0}")]
    Network(#[from] reqwest::Error),
}

/// 上传结果（带 MD5 校验信息）
#[derive(Debug, Clone)]
pub struct UploadResult {
    pub remote_path: String,
    /// true = 远端已有相同 MD5 文件，跳过上传
    pub skipped: bool,
    /// true = 远端有同名但 MD5 不同文件，已覆盖
    pub overwritten: bool,
}

pub struct WebDavClient {
    base_url: String,
    username: String,
    password: String,
    base_path: String,
    timeout: Duration,
    mkcol_done: parking_lot::Mutex<HashSet<String>>,
    client: Client,
}

const RETRY_BACKOFF: &[u64] = &[1, 2, 4];

impl WebDavClient {
    pub fn new(
        url: &str,
        username: &str,
        password: &str,
        base_path: &str,
        timeout_secs: u64,
    ) -> Result<Self, WebDavError> {
        let base_path = Self::normalize_path(base_path);
        // SSRF DNS 重绑定防护——用 ssrf_safe_resolve 解析 URL +
        //   DNS 解析 + 内网校验 + 返回校验通过的 IP，然后用 reqwest::ClientBuilder::resolve()
        //   把 host 固定到该校验过的 IP。彻底关闭原 is_ssrf_safe_url 校验通过后到实际请求
        //   之间的 TOCTOU 窗口（攻击者可在校验通过后切换 DNS 到内网 IP）。
        //   校验失败（None）仍创建 client（向后兼容 reload_webdav_client 等未做预校验的调用），
        //   但 log warn 提示调用方应先 is_ssrf_safe_url 校验。
        let resolved = crate::bot::ssrf_safe_resolve(url).ok_or_else(|| {
            WebDavError::Msg("WebDAV URL 未通过 SSRF 校验，已拒绝创建客户端".to_string())
        })?;
        let mut builder = Client::builder()
            // S48: 锁定最低 TLS 1.2，拒绝 TLS 1.0/1.1
            .min_tls_version(reqwest::tls::Version::TLS_1_2)
            // 禁止跟随重定向：resolve() 固定的 IP 只对初始 host 生效，
            // 302 跳向内网地址会绕过 SSRF 校验（审计 M-2）
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(timeout_secs));
        let (host, port, ip) = resolved;
        let socket_addr = std::net::SocketAddr::new(ip, port);
        tracing::info!(
            "[WEBDAV] SSRF 防护：固定 {} → {} (DNS 重绑定防护)",
            host,
            socket_addr
        );
        builder = builder.resolve(&host, socket_addr);
        let client = builder
            .build()
            .map_err(|e| WebDavError::Msg(format!("无法创建 WebDAV HTTP client: {}", e)))?;
        Ok(Self {
            base_url: url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
            base_path,
            timeout: Duration::from_secs(timeout_secs),
            mkcol_done: parking_lot::Mutex::new(HashSet::new()),
            client,
        })
    }

    fn normalize_path(path: &str) -> String {
        let mut p = path.to_string();
        if !p.starts_with('/') {
            p = format!("/{}", p);
        }
        if p.len() > 1 && p.ends_with('/') {
            p = p.trim_end_matches('/').to_string();
        }
        p
    }

    fn build_url(&self, remote_path: &str) -> String {
        let remote_path = if !remote_path.starts_with('/') {
            format!("/{}", remote_path)
        } else {
            remote_path.to_string()
        };
        // 对每段做 URL 编码，同时过滤 . 与 .. 段以防跨目录访问
        let parts: Vec<String> = remote_path
            .split('/')
            .filter(|p| !p.is_empty() && *p != "." && *p != "..")
            .map(|p| utf8_percent_encode(p, NON_ALPHANUMERIC).to_string())
            .collect();
        format!("{}/{}", self.base_url, parts.join("/"))
    }

    fn auth_header(&self) -> Option<String> {
        if self.username.is_empty() {
            return None;
        }
        let token = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", self.username, self.password));
        Some(format!("Basic {}", token))
    }

    /// 根据 cache_key 计算远程相对路径（旧接口，不加扩展名，向后兼容）
    #[allow(dead_code)]
    pub fn remote_path_for(&self, cache_key: &str) -> String {
        self.remote_path_for_ext(cache_key, None)
    }

    /// 根据 cache_key + 可选扩展名计算远程相对路径
    /// ext 形如 ".jpg"、".mp4"；传 None 则不加扩展名（兼容旧文件）
    pub fn remote_path_for_ext(&self, cache_key: &str, ext: Option<&str>) -> String {
        let bucket = if cache_key.len() >= 2 {
            safe_truncate(cache_key, 2).to_lowercase()
        } else {
            "00".to_string()
        };
        let key = match ext {
            Some(e) if !e.is_empty() => {
                let e = if e.starts_with('.') {
                    e.to_string()
                } else {
                    format!(".{}", e)
                };
                format!("{}{}", cache_key, e)
            }
            _ => cache_key.to_string(),
        };
        format!("{}/{}/{}", self.base_path, bucket, key)
    }

    fn request(
        &self,
        method: Method,
        remote_path: &str,
        data: Option<Vec<u8>>,
        extra_headers: Vec<(String, String)>,
        timeout: Option<Duration>,
    ) -> Result<Response, WebDavError> {
        let url = self.build_url(remote_path);
        let mut req = self.client.request(method, &url);
        if let Some(auth) = self.auth_header() {
            req = req.header(AUTHORIZATION, auth);
        }
        for (k, v) in extra_headers {
            req = req.header(k, v);
        }
        if let Some(d) = data {
            req = req.body(d);
        }
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        req.send().map_err(WebDavError::Network)
    }

    /// 递归 MKCOL 创建目录（已存在则忽略）
    pub fn ensure_dir(&self, dir_path: &str) {
        let dir_path = Self::normalize_path(dir_path);
        let mut done = self.mkcol_done.lock();
        if done.contains(&dir_path) {
            return;
        }
        let parts: Vec<&str> = dir_path.split('/').filter(|p| !p.is_empty()).collect();
        let mut cur = String::new();
        for p in parts {
            cur = format!("{}/{}", cur, p);
            if done.contains(&cur) {
                continue;
            }
            match self.request(
                Method::from_bytes(b"MKCOL").unwrap(),
                &cur,
                None,
                vec![],
                Some(self.timeout),
            ) {
                Ok(_) => {}
                Err(WebDavError::Network(e)) => {
                    if let Some(status) = e.status() {
                        let code = status.as_u16();
                        if ![301, 405, 409, 423].contains(&code) {
                            tracing::warn!("[WebDAV] MKCOL {} 失败: HTTP {}", cur, code);
                        }
                    } else {
                        tracing::warn!("[WebDAV] MKCOL {} 异常: {}", cur, e);
                    }
                }
                Err(e) => tracing::warn!("[WebDAV] MKCOL {} 异常: {}", cur, e),
            }
            done.insert(cur.clone());
        }
    }

    /// 测试连通性 + 鉴权
    pub fn test_connection(&self) -> serde_json::Value {
        // 用 PROPFIND 请求 base_path
        match self.request(
            Method::from_bytes(b"PROPFIND").unwrap(),
            &self.base_path,
            None,
            vec![("Depth".to_string(), "0".to_string())],
            Some(Duration::from_secs(15)),
        ) {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if (200..300).contains(&code) {
                    serde_json::json!({"ok": true, "message": "连接成功", "status": code})
                } else {
                    serde_json::json!({"ok": false, "message": format!("HTTP {}", code), "status": code})
                }
            }
            Err(WebDavError::Network(e)) => {
                if let Some(status) = e.status() {
                    let code = status.as_u16();
                    if code == 404 {
                        self.ensure_dir(&self.base_path);
                        return serde_json::json!({"ok": true, "message": "已创建基础目录", "status": 201});
                    }
                    if code == 401 || code == 403 {
                        return serde_json::json!({"ok": false, "message": "认证失败，请检查账号密码", "status": code});
                    }
                    // 回退 OPTIONS
                    match self.request(
                        Method::OPTIONS,
                        "/",
                        None,
                        vec![],
                        Some(Duration::from_secs(10)),
                    ) {
                        Ok(resp) => {
                            serde_json::json!({"ok": true, "message": "连接成功", "status": resp.status().as_u16()})
                        }
                        Err(_) => {
                            serde_json::json!({"ok": false, "message": format!("HTTP {}", code), "status": code})
                        }
                    }
                } else {
                    serde_json::json!({"ok": false, "message": format!("网络错误: {}", e), "status": 0})
                }
            }
            Err(e) => {
                serde_json::json!({"ok": false, "message": format!("未知异常: {}", e), "status": 0})
            }
        }
    }

    /// 上传媒体（旧接口，不加扩展名，不做 MD5 校验）。返回远程相对路径。失败抛 WebDavError。
    #[allow(dead_code)]
    pub fn upload(
        &self,
        cache_key: &str,
        data: Vec<u8>,
        content_type: &str,
    ) -> Result<String, WebDavError> {
        let result = self.upload_with_check(cache_key, None, data, content_type, "")?;
        Ok(result.remote_path)
    }

    /// HEAD 探测远端文件
    /// 返回值：
    /// - `None`：文件不存在（404/410）
    /// - `Some(None)`：文件存在，但没有 Content-MD5 头
    /// - `Some(Some(md5))`：文件存在且带 Content-MD5（小写 hex）
    ///
    /// 注意：很多 WebDAV 服务器不返回 Content-MD5，此时返回 `Some(None)`。
    /// 也尝试读 ETag 作为弱对账依据（部分服务器用 ETag 而非 Content-MD5）。
    pub fn head_check(&self, remote_path: &str) -> Option<Option<String>> {
        match self.request(
            Method::HEAD,
            remote_path,
            None,
            vec![],
            Some(Duration::from_secs(15)),
        ) {
            Ok(resp) => {
                let code = resp.status().as_u16();
                if code == 404 || code == 410 {
                    return None;
                }
                if !(200..300).contains(&code) {
                    // 其他非成功码视为不存在，保守返回 None
                    tracing::debug!(
                        "[WebDAV] HEAD {} 返回 HTTP {}，视为不存在",
                        remote_path,
                        code
                    );
                    return None;
                }
                // 优先 Content-MD5
                if let Some(md5) = resp
                    .headers()
                    .get("Content-MD5")
                    .and_then(|v| v.to_str().ok())
                {
                    let md5 = md5.trim().to_lowercase();
                    if !md5.is_empty() {
                        return Some(Some(md5));
                    }
                }
                Some(None)
            }
            Err(WebDavError::Network(e)) => {
                if let Some(status) = e.status() {
                    let code = status.as_u16();
                    if code == 404 || code == 410 {
                        return None;
                    }
                }
                tracing::debug!("[WebDAV] HEAD {} 网络异常: {}", remote_path, e);
                None
            }
            Err(e) => {
                tracing::debug!("[WebDAV] HEAD {} 异常: {}", remote_path, e);
                None
            }
        }
    }

    /// 上传媒体（带 MD5 校验 + 扩展名支持）。
    /// - `ext`：文件扩展名（如 ".jpg"），传 None 不加扩展名
    /// - `local_md5`：本地数据 MD5 hex（小写）；空字符串表示不校验，直接 PUT
    ///
    /// 流程：
    /// 1. 若 local_md5 非空，先 HEAD 探测远端
    /// 2. 若远端存在且 Content-MD5 匹配 → 跳过上传，返回 skipped=true
    /// 3. 若远端存在但无 Content-MD5 或 MD5 不匹配 → 覆盖上传（记 warn 日志），返回 overwritten=true
    /// 4. 若远端不存在 → 正常 PUT
    pub fn upload_with_check(
        &self,
        cache_key: &str,
        ext: Option<&str>,
        data: Vec<u8>,
        content_type: &str,
        local_md5: &str,
    ) -> Result<UploadResult, WebDavError> {
        let remote_path = self.remote_path_for_ext(cache_key, ext);
        let parent = remote_path.rsplit_once('/').map(|x| x.0).unwrap_or("");
        self.ensure_dir(parent);

        // MD5 校验阶段（仅在提供了 local_md5 时进行）
        let mut was_overwrite = false;
        if !local_md5.is_empty() {
            match self.head_check(&remote_path) {
                None => {
                    // 远端不存在，正常上传
                }
                Some(Some(cloud_md5)) => {
                    was_overwrite = true;
                    if cloud_md5.eq_ignore_ascii_case(local_md5) {
                        tracing::info!(
                            "[WebDAV] MD5 命中跳过 {} (cloud_md5={})",
                            remote_path,
                            cloud_md5
                        );
                        return Ok(UploadResult {
                            remote_path,
                            skipped: true,
                            overwritten: false,
                        });
                    } else {
                        tracing::warn!(
                            "[WebDAV] MD5 不一致将覆盖 {} (local={} cloud={})",
                            remote_path,
                            local_md5,
                            cloud_md5
                        );
                        // 继续走 PUT 覆盖
                    }
                }
                Some(None) => {
                    was_overwrite = true;
                    // 远端存在但无 Content-MD5 头，保守覆盖（记 warn）
                    tracing::warn!(
                        "[WebDAV] 远端 {} 已存在但无 Content-MD5 头，按覆盖策略上传",
                        remote_path
                    );
                }
            }
        }

        let data_len = data.len();
        let mut last_err: Option<String> = None;
        for (attempt, &backoff) in RETRY_BACKOFF.iter().enumerate() {
            let headers = vec![
                (CONTENT_TYPE.to_string(), content_type.to_string()),
                (CONTENT_LENGTH.to_string(), data_len.to_string()),
            ];
            // 注：每次重试 clone 一次 Vec。request 签名消费 Option<Vec<u8>>，
            // 真正消除 clone 需重构 request 为 &[u8]，但波及 download 等调用点，
            // 权衡后保留现状；重试最多 3 次，可接受。
            match self.request(
                Method::PUT,
                &remote_path,
                Some(data.clone()),
                headers,
                Some(Duration::from_secs(self.timeout.as_secs().max(120))),
            ) {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if (200..300).contains(&code) {
                        tracing::info!(
                            "[WebDAV] 上传成功 {} ({} bytes, HTTP {})",
                            remote_path,
                            data_len,
                            code
                        );
                        return Ok(UploadResult {
                            remote_path,
                            skipped: false,
                            overwritten: was_overwrite,
                        });
                    }
                    let msg = format!("PUT 返回 HTTP {}", code);
                    if code < 500 && code != 429 {
                        return Err(WebDavError::Msg(msg));
                    }
                    last_err = Some(msg);
                    tracing::warn!(
                        "[WebDAV] 上传可重试失败 attempt={} HTTP {} {}",
                        attempt + 1,
                        code,
                        remote_path
                    );
                }
                Err(e) => {
                    last_err = Some(e.to_string());
                    tracing::warn!(
                        "[WebDAV] 上传网络异常 attempt={} {}: {}",
                        attempt + 1,
                        remote_path,
                        e
                    );
                }
            }
            if attempt < RETRY_BACKOFF.len() - 1 {
                std::thread::sleep(Duration::from_secs(backoff));
            }
        }
        Err(WebDavError::Msg(format!(
            "PUT 重试耗尽: {}",
            last_err.unwrap_or_default()
        )))
    }

    /// 下载媒体。返回 bytes，找不到/失败返回 None。
    pub fn download(&self, remote_path: &str) -> Option<Vec<u8>> {
        for (attempt, &backoff) in RETRY_BACKOFF.iter().enumerate() {
            match self.request(
                Method::GET,
                remote_path,
                None,
                vec![],
                Some(Duration::from_secs(self.timeout.as_secs().max(120))),
            ) {
                Ok(mut resp) => {
                    let code = resp.status().as_u16();
                    if code == 404 {
                        tracing::info!("[WebDAV] 下载 {} 不存在", remote_path);
                        return None;
                    }
                    if (200..300).contains(&code) {
                        // S31b: 下载大小限制（100MB），流式读取累计大小
                        const MAX_DOWNLOAD_SIZE: u64 = 100 * 1024 * 1024;
                        if let Some(len) = resp.content_length() {
                            if len > MAX_DOWNLOAD_SIZE {
                                tracing::warn!(
                                    "[WebDAV] 下载 {} Content-Length {} 超过限制 {}",
                                    remote_path,
                                    len,
                                    MAX_DOWNLOAD_SIZE
                                );
                                return None;
                            }
                        }
                        let mut buf = Vec::new();
                        let mut total: u64 = 0;
                        let mut too_large = false;
                        use std::io::Read;
                        let mut chunk_buf = [0u8; 8192];
                        loop {
                            match resp.read(&mut chunk_buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total += n as u64;
                                    if total > MAX_DOWNLOAD_SIZE {
                                        tracing::warn!(
                                            "[WebDAV] 下载 {} 流式累计 {} 超过限制 {}",
                                            remote_path,
                                            total,
                                            MAX_DOWNLOAD_SIZE
                                        );
                                        too_large = true;
                                        break;
                                    }
                                    buf.extend_from_slice(&chunk_buf[..n]);
                                }
                                Err(e) => {
                                    tracing::warn!("[WebDAV] 下载 {} 读取失败: {}", remote_path, e);
                                    return None;
                                }
                            }
                        }
                        if !too_large && !buf.is_empty() {
                            return Some(buf);
                        }
                        return None;
                    }
                    if code < 500 && code != 429 {
                        tracing::warn!("[WebDAV] 下载 {} HTTP {}", remote_path, code);
                        return None;
                    }
                    tracing::warn!(
                        "[WebDAV] 下载可重试失败 attempt={} HTTP {} {}",
                        attempt + 1,
                        code,
                        remote_path
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "[WebDAV] 下载网络异常 attempt={} {}: {}",
                        attempt + 1,
                        remote_path,
                        e
                    );
                }
            }
            if attempt < RETRY_BACKOFF.len() - 1 {
                std::thread::sleep(Duration::from_secs(backoff));
            }
        }
        None
    }

    /// 删除媒体
    #[allow(dead_code)]
    pub fn delete(&self, remote_path: &str) -> bool {
        match self.request(
            Method::DELETE,
            remote_path,
            None,
            vec![],
            Some(Duration::from_secs(30)),
        ) {
            Ok(resp) => {
                let code = resp.status().as_u16();
                (200..300).contains(&code)
            }
            Err(WebDavError::Network(e)) => {
                if let Some(status) = e.status() {
                    let code = status.as_u16();
                    if code == 404 || code == 410 {
                        return true;
                    }
                    tracing::warn!("[WebDAV] DELETE {} HTTP {}", remote_path, code);
                }
                false
            }
            Err(e) => {
                tracing::warn!("[WebDAV] DELETE {} 异常: {}", remote_path, e);
                false
            }
        }
    }
}
