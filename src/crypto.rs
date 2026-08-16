// AES-ECB (iLink CDN 媒体) + AES-GCM (WebDAV 凭证) + PBKDF2 + MD5 + 随机数

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Nonce};
use md5::{Digest, Md5};
use rand::RngCore;

/// 生成随机十六进制字符串
pub fn random_hex(num_bytes: usize) -> String {
    let mut buf = vec![0u8; num_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(&buf)
}

/// 生成随机字节
pub fn random_bytes(num: usize) -> Vec<u8> {
    let mut buf = vec![0u8; num];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// MD5 哈希，返回十六进制字符串
pub fn md5_hex(data: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// SHA-256 哈希，返回十六进制字符串。
/// 用于 bot_id 去重索引（不可逆哈希，token 原文由 user_tokens 表加密存储）。
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// AES-128-ECB 加密 (PKCS7 填充)。
///
/// ⚠️ ECB 模式不提供语义安全，通常禁止用于新设计。
/// 此处保留仅因 iLink CDN 协议固定使用此模式，改为 CBC/GCM 会导致 CDN 不兼容。
/// 加密内容仅为 iLink CDN 媒体数据块，不含用户凭证或长期密钥；
/// 单次密钥仅用于该协议传输，不与任何其他用途复用。
/// 如未来 iLink 协议升级支持 GCM/CBC，应立即切换并移除此函数。
/// 新业务数据加密请使用 `aes_gcm_encrypt`。
pub fn aes_ecb_encrypt(plaintext: &[u8], key: &[u8]) -> anyhow::Result<Vec<u8>> {
    if key.len() != 16 {
        anyhow::bail!("AES-128 密钥必须 16 字节, 实际 {}", key.len());
    }
    let cipher =
        Aes128::new_from_slice(key).map_err(|e| anyhow::anyhow!("AES 密钥初始化失败: {}", e))?;
    // 计算填充后长度
    let pad_len = 16 - (plaintext.len() % 16);
    let mut buf = vec![0u8; plaintext.len() + pad_len];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    // PKCS7 填充
    for byte in buf.iter_mut().skip(plaintext.len()) {
        *byte = pad_len as u8;
    }
    // 逐块加密
    let mut result = Vec::with_capacity(buf.len());
    for chunk in buf.chunks_exact(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        result.extend_from_slice(&block);
    }
    Ok(result)
}

/// AES-128-ECB 解密 (PKCS7 去填充)。
/// ⚠️ 仅限 iLink CDN 协议解密使用，严禁用于新业务。详见 `aes_ecb_encrypt`。
pub fn aes_ecb_decrypt(ciphertext: &[u8], key: &[u8]) -> anyhow::Result<Vec<u8>> {
    if key.len() != 16 {
        anyhow::bail!("AES-128 密钥必须 16 字节, 实际 {}", key.len());
    }
    if ciphertext.is_empty() {
        anyhow::bail!("密文为空");
    }
    if !ciphertext.len().is_multiple_of(16) {
        anyhow::bail!("密文长度 {} 不是 16 的倍数, 数据可能损坏", ciphertext.len());
    }
    let cipher =
        Aes128::new_from_slice(key).map_err(|e| anyhow::anyhow!("AES 密钥初始化失败: {}", e))?;

    let mut result = Vec::with_capacity(ciphertext.len());
    for chunk in ciphertext.chunks_exact(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        result.extend_from_slice(&block);
    }

    // PKCS7 去填充（带校验）
    // S59: 避免 unwrap() 在空结果时 panic
    let pad_len = match result.last() {
        Some(&last) => last as usize,
        None => anyhow::bail!("AES-ECB 解密失败: 结果为空"),
    };
    if pad_len == 0 || pad_len > 16 {
        tracing::warn!("[CRYPTO] 无效填充长度 {}, 可能密钥错误", pad_len);
        anyhow::bail!("AES-ECB 解密失败: 无效填充长度 {}", pad_len);
    }
    // 校验所有填充字节一致
    let data_len = result.len();
    for i in 0..pad_len {
        if result[data_len - 1 - i] != pad_len as u8 {
            tracing::warn!("[CRYPTO] 填充校验失败, 可能密钥错误");
            anyhow::bail!("AES-ECB 解密失败: 填充校验不一致");
        }
    }
    result.truncate(data_len - pad_len);
    Ok(result)
}

/// AES-256-GCM 加密
/// 返回 "enc:" + base64(nonce(12) + ciphertext + tag(16))
pub fn aes_gcm_encrypt(plaintext: &str, key: &[u8]) -> anyhow::Result<String> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    if key.len() != 32 {
        anyhow::bail!("AES-256-GCM 密钥必须 32 字节");
    }
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("GCM 密钥初始化失败: {}", e))?;
    let nonce_bytes = random_bytes(12);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("GCM 加密失败: {}", e))?;
    // ciphertext 已包含 tag
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(format!(
        "enc:{}",
        base64::engine::general_purpose::STANDARD.encode(&blob)
    ))
}

/// AES-256-GCM 解密。
/// 非 "enc:" 前缀的数据按以下策略处理：
///   - 默认（严格模式）：返回 Err，拒绝明文回退。启动迁移已将 DB 中敏感字段加密，
///     运行期遇到明文表明数据未正确加密（可能为绕过写入），应拒绝而非当明文返回。
///   - 兼容模式（`ILINK_ALLOW_PLAINTEXT_FALLBACK=1`）：返回原文并记录 warn，
///     仅用于迁移失败时的紧急兜底，需尽快重新保存以加密。
///
/// 解密失败（密钥错误/数据损坏）始终返回 Err。
pub fn aes_gcm_decrypt(stored: &str, key: &[u8]) -> anyhow::Result<String> {
    if stored.is_empty() {
        return Ok(String::new());
    }

    // 非 enc: 前缀按明文处理：默认拒绝（严格模式），
    // ILINK_ALLOW_PLAINTEXT_FALLBACK=1 时兼容回退。
    if !stored.starts_with("enc:") {
        let allow_plaintext_fallback = std::env::var("ILINK_ALLOW_PLAINTEXT_FALLBACK")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if allow_plaintext_fallback {
            tracing::warn!(
                "[CRYPTO] 发现明文存储的敏感数据（ILINK_ALLOW_PLAINTEXT_FALLBACK=1 兼容模式），建议重新保存以加密"
            );
            return Ok(stored.to_string());
        }
        tracing::error!(
            "[CRYPTO] 拒绝明文回退：数据未以 enc: 前缀加密存储。如为升级迁移失败，可临时设置 ILINK_ALLOW_PLAINTEXT_FALLBACK=1 兜底，并尽快重新保存敏感数据"
        );
        anyhow::bail!(
            "明文敏感数据被拒绝：数据未以 enc: 前缀加密存储。如为升级迁移失败，可临时设置 ILINK_ALLOW_PLAINTEXT_FALLBACK=1 兜底"
        );
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(&stored[4..])
        .map_err(|e| anyhow::anyhow!("GCM base64 解码失败: {}", e))?;
    if blob.len() < 12 + 16 {
        anyhow::bail!("GCM 密文长度不足: {}", blob.len());
    }
    let nonce = Nonce::from_slice(&blob[..12]);
    let ciphertext = &blob[12..];
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("GCM 密钥初始化失败: {}", e))?;
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("GCM 解密失败（密钥错误或数据损坏）: {}", e))?;
    Ok(String::from_utf8_lossy(&plaintext).into_owned())
}

/// PBKDF2-SHA256 哈希
pub fn pbkdf2_hash(password: &str, salt: &str, iterations: u32) -> String {
    use pbkdf2::pbkdf2_hmac;
    use sha2::Sha256;
    let mut derived_key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(
        password.as_bytes(),
        salt.as_bytes(),
        iterations,
        &mut derived_key,
    );
    hex::encode(derived_key)
}

/// 常量时间比较（防时序攻击）。
/// 使用 u64 累加器避免 u8 溢出导致的长度检查绕过。
pub fn constant_time_compare(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut acc: u64 = (a_bytes.len() ^ b_bytes.len()) as u64;
    let max = a_bytes.len().max(b_bytes.len());
    for i in 0..max {
        let av = a_bytes.get(i).copied().unwrap_or(0);
        let bv = b_bytes.get(i).copied().unwrap_or(0);
        acc |= (av ^ bv) as u64;
    }
    acc == 0
}

use base64::Engine;
