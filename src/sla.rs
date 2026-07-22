//! SLA 未处理提醒调度：每 5 分钟扫描「待办跟踪」表 pending 行，
//! 按工作时长（工作日 09:30–18:00 Asia/Shanghai）判断，超 1 工作小时未处理则私聊提醒，
//! 之后每满 1 工作小时重复提醒，直到该行被标记 done。

use crate::cards;
use crate::state::AppState;
use crate::store;
use chrono::{Datelike, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Asia::Shanghai;
use std::time::Duration;
use tracing::{info, warn};

/// 每满 1 工作小时提醒一次。
const REMIND_INTERVAL_SECS: i64 = 3600;

/// 计算 [from_ms, to_ms] 之间落在「工作日 09:30–18:00 Asia/Shanghai」内的秒数。
pub fn business_seconds(from_ms: i64, to_ms: i64) -> i64 {
    if to_ms <= from_ms {
        return 0;
    }
    let tz = Shanghai;
    let from = Utc.timestamp_millis_opt(from_ms).unwrap().with_timezone(&tz);
    let to = Utc.timestamp_millis_opt(to_ms).unwrap().with_timezone(&tz);
    let open = NaiveTime::from_hms_opt(9, 30, 0).unwrap();
    let close = NaiveTime::from_hms_opt(18, 0, 0).unwrap();

    let mut total = 0i64;
    let mut day = from.date_naive();
    let last = to.date_naive();
    while day <= last {
        let wd = day.weekday();
        if wd != Weekday::Sat && wd != Weekday::Sun {
            let ws = tz.from_local_datetime(&day.and_time(open)).unwrap();
            let we = tz.from_local_datetime(&day.and_time(close)).unwrap();
            let s = ws.max(from);
            let e = we.min(to);
            if e > s {
                total += (e - s).num_seconds();
            }
        }
        day = match day.succ_opt() {
            Some(d) => d,
            None => break,
        };
    }
    total
}

/// 扫描一次：对每个 pending 行，若自上次提醒（或待处理起点）起已过 1 个工作小时，则提醒。
pub async fn tick(state: &AppState) {
    let now = Utc::now().timestamp_millis();
    let rows = store::list_pending(state).await;
    // 兜底去重：同一人同一 item 本轮最多提醒一次（防并发竞态产生的重复行导致翻倍）。
    let mut sent: std::collections::HashSet<(String, u64)> = std::collections::HashSet::new();
    for row in rows {
        let anchor = row.last_reminded_ms.unwrap_or(row.pending_since_ms);
        if business_seconds(anchor, now) < REMIND_INTERVAL_SECS {
            continue;
        }
        if !sent.insert((row.open_id.clone(), row.number)) {
            // 同一人+同一 item 本轮已提醒过，跳过重复行
            store::set_reminded(state, &row.record_id, now).await;
            continue;
        }
        let card = cards::sla_reminder_card(&row.kind, row.number, &row.title, &row.url, &row.role);
        match state.feishu.send_card("open_id", &row.open_id, &card).await {
            Ok(_) => {
                store::set_reminded(state, &row.record_id, now).await;
                info!("SLA 提醒已发：{} #{} -> {}", row.kind, row.number, row.open_id);
            }
            Err(e) => warn!("SLA 提醒发送失败 {} #{}: {e:#}", row.kind, row.number),
        }
    }
}

/// 启动后台调度循环（每 5 分钟一次）。
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(300));
        loop {
            ticker.tick().await;
            tick(&state).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::business_seconds;
    use chrono::TimeZone;
    use chrono_tz::Asia::Shanghai;

    fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        Shanghai
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn within_same_workday() {
        // 周一 10:00 -> 11:30 = 5400s
        let a = ms(2026, 7, 20, 10, 0); // 2026-07-20 是周一
        let b = ms(2026, 7, 20, 11, 30);
        assert_eq!(business_seconds(a, b), 5400);
    }

    #[test]
    fn clipped_to_window() {
        // 周一 08:00 -> 09:30 窗口前 = 0；到 10:30 = 3600
        let a = ms(2026, 7, 20, 8, 0);
        assert_eq!(business_seconds(a, ms(2026, 7, 20, 9, 30)), 0);
        assert_eq!(business_seconds(a, ms(2026, 7, 20, 10, 30)), 3600);
    }

    #[test]
    fn skips_weekend() {
        // 周五 17:30 -> 周一 09:31：周五剩 30min(1800) + 周一 1min(60) = 1860
        let fri = ms(2026, 7, 24, 17, 30); // 2026-07-24 周五
        let mon = ms(2026, 7, 27, 9, 31); // 2026-07-27 周一
        assert_eq!(business_seconds(fri, mon), 1800 + 60);
    }

    #[test]
    fn after_hours_zero() {
        // 周一 18:30 -> 19:30 全在窗口外
        let a = ms(2026, 7, 20, 18, 30);
        let b = ms(2026, 7, 20, 19, 30);
        assert_eq!(business_seconds(a, b), 0);
    }
}
