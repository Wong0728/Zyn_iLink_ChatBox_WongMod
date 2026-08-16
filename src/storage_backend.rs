// 媒体存储后端抽象 — 三级回退：本地 FS → WebDAV → S3 兼容
// 参考 openilink-hub 的 storage 包设计
// ponytail: trait 方法为预留接口，当前 bot.rs 直接用 save_media_cache 而非 tiered_storage.put

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

/// 存储后端 trait
pub trait StorageBackend: Send + Sync {
    /// 存储媒体，返回远程路径或唯一标识
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<String, String>;
    /// 读取媒体
    fn get(&self, key: &str) -> Option<Vec<u8>>;
    /// 检查是否存在
    fn exists(&self, key: &str) -> bool;
    /// 删除媒体
    fn delete(&self, key: &str) -> bool;
    /// 后端名称（用于日志）
    fn name(&self) -> &'static str;
}

// ── LocalFs 后端（默认，始终启用）──────────────────

// 本地媒体缓存写入强制 50MB 上限。
//   服务端独立强制 50MB，与前端 25MB 解耦——前端限制用户体验，后端限制安全。
//   上限可通过 ILINK_MEDIA_MAX_FILE_MB 环境变量调整（默认 50，最大 200）。
const DEFAULT_MEDIA_MAX_FILE_BYTES: usize = 50 * 1024 * 1024;

fn media_max_file_bytes() -> usize {
    std::env::var("ILINK_MEDIA_MAX_FILE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .unwrap_or(DEFAULT_MEDIA_MAX_FILE_BYTES)
        .min(200 * 1024 * 1024)
}

pub struct LocalFsBackend {
    cache_dir: PathBuf,
}

impl LocalFsBackend {
    pub fn new(cache_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        Self { cache_dir }
    }

    fn file_path(&self, key: &str) -> PathBuf {
        // 只接受纯 hex 字符的 key，拒绝其他字符（防止路径碰撞）
        if key.is_empty() || !key.bytes().all(|c| c.is_ascii_hexdigit()) {
            // 将非法 key 映射到专用子目录，避免碰撞
            let hash = crate::crypto::md5_hex(key.as_bytes());
            let bucket = &hash[..2];
            self.cache_dir.join(bucket).join(&hash)
        } else if key.len() >= 2 {
            let bucket = &key[..2];
            self.cache_dir.join(bucket).join(key)
        } else {
            self.cache_dir.join("00").join(key)
        }
    }
}

impl StorageBackend for LocalFsBackend {
    fn put(&self, key: &str, data: &[u8], _content_type: &str) -> Result<String, String> {
        // 服务端独立强制文件大小上限，与前端检查解耦。
        let max_bytes = media_max_file_bytes();
        if data.len() > max_bytes {
            return Err(format!(
                "文件过大：{} 字节，超过上限 {} 字节（{} MB）",
                data.len(),
                max_bytes,
                max_bytes / 1024 / 1024
            ));
        }
        let path = self.file_path(key);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, data).map_err(|e| format!("本地写入失败: {}", e))?;
        Ok(path.to_string_lossy().to_string())
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.file_path(key);
        std::fs::read(&path).ok()
    }

    fn exists(&self, key: &str) -> bool {
        self.file_path(key).exists()
    }

    fn delete(&self, key: &str) -> bool {
        let path = self.file_path(key);
        std::fs::remove_file(&path).is_ok()
    }

    fn name(&self) -> &'static str {
        "LocalFS"
    }
}

// ── TieredStorage — 多级回退 —───────────────────────

/// 多级存储：按顺序查找，写入所有已启用的后端
pub struct TieredStorage {
    backends: Vec<Box<dyn StorageBackend>>,
}

impl TieredStorage {
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
        }
    }

    /// 添加后端（先添加的优先级更高）
    pub fn add_backend(&mut self, backend: Box<dyn StorageBackend>) {
        self.backends.push(backend);
    }

    /// 移除指定名称的后端
    pub fn remove_backend_by_name(&mut self, name: &str) {
        self.backends.retain(|b| b.name() != name);
    }

    /// 后端列表（可变引用，用于外部管理）
    pub fn backends_mut(&mut self) -> &mut Vec<Box<dyn StorageBackend>> {
        &mut self.backends
    }

    /// 读取：按优先级查找，命中后回填高层级后端
    ///
    /// 缓存未命中回填限制在小文件（≤1MB）同步执行，
    ///   大文件跳过回填避免阻塞当前请求。
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        for (i, backend) in self.backends.iter().enumerate() {
            if let Some(data) = backend.get(key) {
                // 仅对 ≤1MB 的小文件同步回填高层级后端
                if i > 0 && data.len() <= 1_048_576 {
                    for j in 0..i {
                        let data_ref = &data;
                        let _ = self.backends[j].put(key, data_ref, "application/octet-stream");
                    }
                }
                return Some(data);
            }
        }
        None
    }

    /// 写入：写入所有已启用的后端
    pub fn put(&self, key: &str, data: &[u8], content_type: &str) -> Vec<Result<String, String>> {
        self.backends
            .iter()
            .map(|b| b.put(key, data, content_type))
            .collect()
    }

    /// 是否存在
    pub fn exists(&self, key: &str) -> bool {
        self.backends.iter().any(|b| b.exists(key))
    }

    /// 后端数量
    pub fn backend_count(&self) -> usize {
        self.backends.len()
    }
}

impl Default for TieredStorage {
    fn default() -> Self {
        Self::new()
    }
}

// ── WebDAV 后端适配器 ──────────────────────────────

/// 通过 Arc 包装已有的 WebDavClient
pub struct WebDavStorageBackend {
    client: Arc<crate::webdav::WebDavClient>,
}

impl WebDavStorageBackend {
    pub fn new(client: Arc<crate::webdav::WebDavClient>) -> Self {
        Self { client }
    }
}

impl StorageBackend for WebDavStorageBackend {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<String, String> {
        self.client
            .upload(key, data.to_vec(), content_type)
            .map_err(|e| format!("WebDAV 上传失败: {}", e))
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        let remote_path = self.client.remote_path_for(key);
        self.client.download(&remote_path)
    }

    fn exists(&self, key: &str) -> bool {
        // 改用 HEAD 请求替代 GET 全量下载。
        let remote_path = self.client.remote_path_for(key);
        self.client.head_check(&remote_path).is_some()
    }

    fn delete(&self, key: &str) -> bool {
        let remote_path = self.client.remote_path_for(key);
        self.client.delete(&remote_path)
    }

    fn name(&self) -> &'static str {
        "WebDAV"
    }
}
