// 老单用户库 → 多用户架构 迁移逻辑 (Phase 1 Wave 2)
//
// 背景：老版部署为单用户，base_dir/wechat_bot.db 同时承载 web_password（全局单密码）、
// bot_config（iLink 配置）与各数据表；媒体缓存位于 base_dir/media_cache/。
// 首次启动新版多用户架构时自动迁移：
//   1. 在 system.db 建 owner 用户（沿用老库密码哈希/salt/iterations）；
//   2. 把老库数据表复制到 users/<owner_uid>/user.db；
//   3. 老媒体缓存移到 users/<owner_uid>/media_cache/；
//   4. 老库改名为 wechat_bot.db.bak 备份。
// 幂等：重复运行不报错、不重复迁移。

use crate::config;
use crate::storage::{Database, SystemDatabase};
use rusqlite::{params, Connection};
use std::fs;

/// 老库 → 多用户迁移入口。
///
/// 返回值：
/// - `Ok(Some(owner_uid))`：执行了迁移，owner 用户已建，数据已搬到 users/<uid>/。
/// - `Ok(None)`：无需迁移（system.db 已有用户 / 老库不存在 / 老库无 web_password 记录）。
///
/// 幂等：通过 `system_db.list_users()` 与 `get_user_by_username("owner")` 判断是否已迁移。
pub fn migrate_legacy_to_multiuser(system_db: &SystemDatabase) -> anyhow::Result<Option<i64>> {
    // 1. 幂等检查：system.db 已有用户或 owner 已存在 → 已迁移过
    if !system_db.list_users().is_empty() || system_db.get_user_by_username("owner").is_some() {
        tracing::debug!("[MIGRATION] system.db 已有用户，跳过老库迁移");
        return Ok(None);
    }

    // 2. 无老库 → 无需迁移
    let legacy_db_path = config::db_file();
    if !legacy_db_path.exists() {
        tracing::debug!(
            "[MIGRATION] 未发现老库 {}，跳过迁移",
            legacy_db_path.display()
        );
        return Ok(None);
    }

    // 3. 打开老库读取 web_password（作为 owner 凭证来源）
    let legacy_conn = Connection::open(&legacy_db_path)?;
    let pw_row: Option<(String, String, i64)> = legacy_conn
        .query_row(
            "SELECT password_hash, salt, iterations FROM web_password WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    // 老库 web_password 可能无 iterations 列（极旧版本），回退 200000
                    row.get::<_, i64>(2).unwrap_or(200_000),
                ))
            },
        )
        .ok();
    // 关闭老库连接，避免后续 ATTACH/改名时文件被占用
    drop(legacy_conn);

    let (pw_hash, salt, iterations) = match pw_row {
        Some(v) => v,
        None => {
            // 老库无 web_password 行：跳过迁移，留给首次运行向导处理
            tracing::warn!(
                "[MIGRATION] 老库 {} 无 web_password 记录，跳过迁移，留给首次运行向导处理",
                legacy_db_path.display()
            );
            return Ok(None);
        }
    };

    tracing::info!(
        "[MIGRATION] 检测到老库 {}，开始迁移为多用户架构",
        legacy_db_path.display()
    );

    // 迁移开始前先备份老库为 .pre-migration-bak
    //   若迁移中途失败（如步骤 6 表结构不一致 panic、磁盘满、权限错误），
    //   system.db 已建 owner 用户但 user.db 数据不全，老库尚未改名，
    //   重启后因 system.db 已有用户 → 跳过迁移 → 数据丢失。
    //   现在迁移前先复制老库为 .pre-migration-bak，迁移失败时可手动回滚。
    let pre_bak_path = legacy_db_path.with_file_name("wechat_bot.db.pre-migration-bak");
    if !pre_bak_path.exists() {
        match fs::copy(&legacy_db_path, &pre_bak_path) {
            Ok(_) => tracing::info!(
                "[MIGRATION] 老库已备份为 {}（迁移失败时可用于回滚）",
                pre_bak_path.display()
            ),
            Err(e) => {
                tracing::error!(
                    "[MIGRATION] 老库预备份失败 {}: {}，中止迁移以确保数据安全",
                    pre_bak_path.display(),
                    e
                );
                return Err(anyhow::anyhow!("老库预备份失败: {}", e));
            }
        }
    }

    // 4. 在 system.db 创建 owner 用户（沿用老库密码哈希/salt/iterations）
    //    分配 6 位随机 uid（与新版 create_user 流程一致）
    let owner_uid = system_db.allocate_random_uid()?;
    system_db.create_user(owner_uid, "owner", &pw_hash, &salt, iterations, "owner")?;
    tracing::info!("[MIGRATION] 已创建 owner 用户 (uid={})", owner_uid);

    // 5. 创建 users/<owner_uid>/ 目录
    let user_dir = config::user_dir(owner_uid);
    fs::create_dir_all(&user_dir)?;

    // 6. 创建 user.db 并复制老库数据表
    //    Database::new_for_user 会通过 init_db 建好空表；
    //    用单例的 conn_lock() 做 ATTACH 复制，保证 ATTACH 与 INSERT 在同一连接内执行。
    //    Database::new_for_user 改返回 Result，迁移失败时友好返回。
    let user_db = Database::new_for_user(owner_uid)?;
    {
        let conn = user_db.conn_lock();
        // ponytail: ceiling=一次性 O(n) 全表复制，单进程迁移，无需并发。升级路径：分批事务 + WAL。
        let legacy_path_str = legacy_db_path.to_string_lossy().to_string();
        conn.execute("ATTACH DATABASE ? AS old", params![&legacy_path_str])?;

        // 逐表复制。web_password 不复制（owner 凭证已存 system.db）；
        // sessions 不复制（新版会话走 system.db 的 sessions 表）。
        let tables = [
            "config",
            "user_tokens",
            "messages",
            "messages_v2",
            "media_meta",
            "media_remote",
            "webdav_config",
        ];
        // L-8：跳过的表不再只留单条 warn——汇总计数并在迁移完成后写入
        //      数据目录 migration-skipped.txt，防止运维漏看导致数据缺失无据可查。
        let mut skipped: Vec<(String, String)> = Vec::new();
        for table in &tables {
            let sql = format!("INSERT INTO {} SELECT * FROM old.{}", table, table);
            match conn.execute(&sql, []) {
                Ok(_) => tracing::info!("[MIGRATION] 已复制表 {}", table),
                Err(e) => {
                    // 老版本可能无 messages_v2 等表，或列结构不一致 → 跳过不中断
                    tracing::warn!("[MIGRATION] 跳过表 {}: {}", table, e);
                    skipped.push((table.to_string(), e.to_string()));
                }
            }
        }
        if !skipped.is_empty() {
            let report_path = config::base_dir().join("migration-skipped.txt");
            let report = format!(
                "iLink-WM1 迁移跳过报告 {}\n老库: {}\n共 {} 张表未复制（详情见下，老库备份保留在 .pre-migration-bak）：\n\n{}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                legacy_path_str,
                skipped.len(),
                skipped
                    .iter()
                    .map(|(t, e)| format!("  - {}: {}", t, e))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            if let Err(we) = fs::write(&report_path, report) {
                tracing::warn!(
                    "[MIGRATION] 跳过报告写入失败 {}: {}",
                    report_path.display(),
                    we
                );
            }
            tracing::warn!(
                "[MIGRATION] 共 {} 张表未复制，报告已写入 {}",
                skipped.len(),
                report_path.display()
            );
        }

        // DETACH 老库，释放对老库文件的引用
        let _ = conn.execute("DETACH DATABASE old", []);
    } // conn_lock 释放

    // 7. 迁移媒体文件：老 media_cache/* → users/<owner_uid>/media_cache/
    let legacy_media_dir = config::media_cache_dir();
    if legacy_media_dir.exists() {
        let new_media_dir = config::user_media_cache_dir(owner_uid);
        fs::create_dir_all(&new_media_dir)?;
        match fs::read_dir(&legacy_media_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let from = entry.path();
                    let to = new_media_dir.join(entry.file_name());
                    // 同盘 rename 高效；跨盘/被占用时 fallback 到 copy+remove
                    if let Err(e) = fs::rename(&from, &to) {
                        tracing::debug!(
                            "[MIGRATION] rename 失败 ({}), 尝试 copy+remove: {}",
                            e,
                            from.display()
                        );
                        if let Err(e) = fs::copy(&from, &to).and_then(|_| fs::remove_file(&from)) {
                            tracing::warn!(
                                "[MIGRATION] 迁移媒体文件 {} 失败: {}，跳过",
                                from.display(),
                                e
                            );
                        }
                    }
                }
                tracing::info!("[MIGRATION] 媒体文件迁移完成");
            }
            Err(e) => {
                // 读取老媒体目录失败不阻断整体迁移
                tracing::warn!(
                    "[MIGRATION] 读取老媒体目录 {} 失败: {}，跳过媒体迁移",
                    legacy_media_dir.display(),
                    e
                );
            }
        }
    }

    // 8. 老库改名备份：wechat_bot.db → wechat_bot.db.bak
    let bak_path = legacy_db_path.with_file_name("wechat_bot.db.bak");
    fs::rename(&legacy_db_path, &bak_path)?;

    // 9. 日志
    tracing::info!(
        "[MIGRATION] 老库已迁移为 owner 用户 (uid={})，数据复制到 users/{}/user.db，老库备份为 wechat_bot.db.bak",
        owner_uid,
        owner_uid
    );

    // 10. 返回迁移结果
    Ok(Some(owner_uid))
}
