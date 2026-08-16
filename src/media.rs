// 媒体处理：MIME 嗅探、SILK → WAV 转码（ffmpeg）

use crossbeam_channel::{bounded, Receiver, Sender};

/// ffmpeg 并发信号量：防止多个大文件同时转码打满 CPU/内存。
/// 默认最多 2 个并发，可通过 ILINK_FFMPEG_MAX_CONCURRENT 环境变量调整。
static FFMPEG_SEM: once_cell::sync::Lazy<(Sender<()>, Receiver<()>)> =
    once_cell::sync::Lazy::new(|| {
        let max = std::env::var("ILINK_FFMPEG_MAX_CONCURRENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2usize);
        let (tx, rx) = bounded(max);
        for _ in 0..max {
            let _ = tx.send(());
        }
        (tx, rx)
    });

struct FfmpegPermit;
impl Drop for FfmpegPermit {
    fn drop(&mut self) {
        let _ = FFMPEG_SEM.0.send(());
    }
}

/// 根据 MIME 类型推断文件扩展名（含点号，如 ".jpg"）
/// 无法识别时返回 None
pub fn ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "image/jpeg" => Some(".jpg"),
        "image/png" => Some(".png"),
        "image/gif" => Some(".gif"),
        "image/webp" => Some(".webp"),
        "audio/mpeg" => Some(".mp3"),
        "audio/wav" => Some(".wav"),
        "audio/silk" => Some(".silk"),
        "audio/amr" => Some(".amr"),
        "audio/ogg" => Some(".ogg"),
        "audio/flac" => Some(".flac"),
        "audio/midi" => Some(".mid"),
        "video/mp4" => Some(".mp4"),
        "video/webm" => Some(".webm"),
        _ => None,
    }
}

/// 受支持媒体的文件名扩展名白名单（含点号，小写）。
/// 与 `ext_for_mime` 支持的类型保持一致，仅在内容嗅探无法识别时用于安全回退。
/// 刻意排除 .html/.svg/.js 等可执行/脚本类扩展名。
fn is_allowed_media_ext(ext: &str) -> bool {
    matches!(
        ext,
        ".jpg"
            | ".jpeg"
            | ".png"
            | ".gif"
            | ".webp"
            | ".mp3"
            | ".wav"
            | ".silk"
            | ".amr"
            | ".ogg"
            | ".flac"
            | ".mid"
            | ".midi"
            | ".mp4"
            | ".webm"
    )
}

/// 从文件名提取扩展名（含点号，小写）
/// 如 "photo.JPG" → Some(".jpg")，"noext" → None
/// 含 NUL 或控制字符的文件名视为非法，返回 None。
pub fn ext_for_filename(filename: &str) -> Option<String> {
    // 拒绝 NUL/控制字符，防止路径截断与注入
    if filename.chars().any(|c| c == '\0' || c.is_control()) {
        return None;
    }
    let dot = filename.rfind('.')?;
    let ext = &filename[dot..];
    if ext.len() > 10 || ext.contains('/') || ext.contains('\\') {
        return None;
    }
    Some(ext.to_lowercase())
}

/// 综合推导扩展名：以内容嗅探结果（`ext_for_mime`，源自 `detect_mime`）为权威来源。
/// 仅当嗅探无法识别，且文件名扩展名落在受支持媒体白名单内时，才回退到文件名扩展名；
/// 否则返回 None，避免信任用户提供的任意扩展名。
/// 返回小写含点号的扩展名，如 ".jpg"。
pub fn derive_ext(mime: &str, filename: &str) -> Option<String> {
    // 内容嗅探类型为权威来源
    if let Some(e) = ext_for_mime(mime) {
        return Some(e.to_string());
    }
    // 嗅探未识别时，仅接受白名单内的文件名扩展名
    if !filename.is_empty() {
        if let Some(e) = ext_for_filename(filename) {
            if is_allowed_media_ext(&e) {
                return Some(e);
            }
        }
    }
    None
}

/// 通过文件头魔数嗅探 MIME 类型
pub fn detect_mime(data: &[u8]) -> &'static str {
    if data.len() < 4 {
        return "application/octet-stream";
    }
    // PNG
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    // GIF
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return "image/gif";
    }
    // WEBP (RIFF....WEBP)
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return "image/webp";
    }
    // JPEG
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    // WAV (RIFF....WAVE)
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WAVE" {
        return "audio/wav";
    }
    // MP3 (ID3 or frame sync)
    if data.starts_with(b"ID3") || (data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xE0) == 0xE0)
    {
        return "audio/mpeg";
    }
    // FLAC
    if data.starts_with(b"fLaC") {
        return "audio/flac";
    }
    // OGG
    if data.starts_with(b"OggS") {
        return "audio/ogg";
    }
    // SILK (#!SILK_V3)
    if data.starts_with(b"#!SILK_V3") {
        return "audio/silk";
    }
    // AMR (#!AMR)
    if data.starts_with(b"#!AMR") {
        return "audio/amr";
    }
    // MP4 (ftyp)
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "video/mp4";
    }
    // MIDI (MThd)
    if data.starts_with(b"MThd") {
        return "audio/midi";
    }
    // WEBM (1A 45 DF A3)
    if data.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return "video/webm";
    }
    "application/octet-stream"
}

/// SILK → WAV 转换结果
///
/// 转码失败时返回描述性错误信息（不再静默返回原始数据），便于上层返回有意义错误码。
///   调用方把 MIME 硬编码为 "audio/wav"，前端拿到 audio/wav 头但实际是 SILK 字节流，
///   `<audio>` 元素解码失败触发 error → toast 显示"浏览器不支持此语音格式"，
///   误导用户以为是浏览器问题，实际是后端 ffmpeg 转码失败）。
///   改为返回 `TranscodeResult`，调用方据 `success` 字段决定真实 MIME，
///   前端按 MIME 识别失败场景并显示"语音转码失败"。
pub struct TranscodeResult {
    /// 转码成功为 WAV/PCM 数据；失败为原始输入数据
    pub data: Vec<u8>,
    /// true=已转为 WAV；false=转码失败，data 为原始数据
    pub success: bool,
}

/// SILK → WAV 转换
/// 优先使用 ffmpeg（pilk 在 Rust 中无直接对应）
pub fn silk_to_wav(silk_data: &[u8]) -> TranscodeResult {
    // S72: SILK 头 magic 校验 — `#!SILK_V3`（9字节）或 `#!SILK_V3\n`（10字节变体）
    let pcm_data = if silk_data.starts_with(b"#!SILK_V3") {
        // 10 字节变体：第 10 字节（index 9）必须为 '\n' (0x0A)，否则按 9 字节头处理
        let skip = if silk_data.len() > 9 && silk_data[9] == b'\n' {
            10
        } else {
            9
        };
        decode_silk_with_ffmpeg(&silk_data[skip..])
    } else {
        // 非 SILK 格式，尝试直接用 ffmpeg
        decode_silk_with_ffmpeg(silk_data)
    };

    match pcm_data {
        Some(pcm) => {
            // PCM 24000Hz 单声道 16bit → WAV
            TranscodeResult {
                data: build_wav(&pcm, 24000, 1, 16),
                success: true,
            }
        }
        None => TranscodeResult {
            data: silk_data.to_vec(),
            success: false,
        },
    }
}

/// 使用 ffmpeg 将音频转换为 WAV
pub fn ffmpeg_to_wav(input_data: &[u8]) -> TranscodeResult {
    match run_ffmpeg(
        input_data,
        "wav",
        &[
            "-y", "-i", "pipe:0", "-f", "wav", "-ar", "24000", "-ac", "1", "pipe:1",
        ],
    ) {
        Some(wav) => TranscodeResult {
            data: wav,
            success: true,
        },
        None => TranscodeResult {
            data: input_data.to_vec(),
            success: false,
        },
    }
}

/// 使用 ffmpeg 解码 SILK 为 PCM
fn decode_silk_with_ffmpeg(silk_data: &[u8]) -> Option<Vec<u8>> {
    run_ffmpeg(
        silk_data,
        "pcm",
        &[
            "-y", "-f", "silk", "-i", "pipe:0", "-f", "s16le", "-ar", "24000", "-ac", "1", "pipe:1",
        ],
    )
}

/// ffmpeg 子进程超时按输入大小动态调整（大文件给更多时间）。
///   原 120s 固定超时对 100KB 短语音浪费资源（占着子进程 60s+ 才超时），
///   对 50MB 大文件又过早 kill 转码失败。按经验：
///     - <1MB：60s（短语音足够，快速失败释放资源）
///     - 1-10MB：120s（中长音频）
///     - >10MB：240s（大文件给足时间，避免误杀）
///   上限 240s 防止恶意大文件无限占用 worker。
fn ffmpeg_timeout_secs(input_size: usize) -> u64 {
    if input_size < 1_000_000 {
        60
    } else if input_size < 10_000_000 {
        120
    } else {
        240
    }
}

/// 调用 ffmpeg 子进程
fn run_ffmpeg(input_data: &[u8], _output_format: &str, args: &[&str]) -> Option<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // 获取 ffmpeg 并发许可，限制同时运行的转码任务数量。
    let _permit = FFMPEG_SEM.1.recv().ok().map(|_| FfmpegPermit)?;

    let mut child = Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // 先取走 stdin 句柄，确保后续能显式关闭
    let mut stdin = child.stdin.take()?;
    // 写入数据；失败时显式 kill 子进程，避免孤儿
    if let Err(e) = stdin.write_all(input_data) {
        tracing::warn!("[FFMPEG] stdin 写入失败: {}", e);
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    drop(stdin); // 显式关闭 stdin，让 ffmpeg 进入 flush 阶段

    // S29: ffmpeg 子进程超时保护（120s）
    // wait_with_output 会消费 child 导致超时后无法 kill，
    // 改为：独立线程读 stdout（防止管道写阻塞死锁），主线程 try_wait 轮询 + 超时 kill。
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;
    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let (tx, rx) = mpsc::channel::<std::io::Result<Vec<u8>>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let result = stdout.read_to_end(&mut buf).map(|_| buf);
        let _ = tx.send(result);
    });

    let deadline =
        std::time::Instant::now() + Duration::from_secs(ffmpeg_timeout_secs(input_data.len()));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "[FFMPEG] 子进程超时 {}s（输入 {} 字节），已 kill",
                        ffmpeg_timeout_secs(input_data.len()),
                        input_data.len()
                    );
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                tracing::warn!("[FFMPEG] try_wait 失败: {}", e);
                return None;
            }
        }
    };

    match rx.recv() {
        Ok(Ok(buf)) if status.success() && !buf.is_empty() => Some(buf),
        Ok(Ok(_)) => None,
        Ok(Err(e)) => {
            tracing::warn!("[FFMPEG] stdout 读取失败: {}", e);
            None
        }
        Err(e) => {
            tracing::warn!("[FFMPEG] stdout 线程 channel 异常: {}", e);
            None
        }
    }
}

/// 构造 WAV 文件头 + PCM 数据
fn build_wav(pcm: &[u8], sample_rate: u32, channels: u16, bits_per_sample: u16) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let chunk_size = 36 + data_len;

    let mut wav = Vec::with_capacity(44 + pcm.len());
    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&chunk_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(pcm);
    wav
}
