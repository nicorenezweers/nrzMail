//! Statistik: wie viele Nachrichten wann kamen und gingen – je Tag, Monat, Stunde, Wochentag,
//! Heatmap, häufigste Absender/Empfänger/Domains, Kategorien, Ordner und Postfächer.
//! Alles per SQL; Tage und Stunden in der Zeitzone des Browsers (`date + tz*60`).
//! Spam und Entwürfe zählen nicht mit.

use crate::api::{self, Category, StatsQuery};
use crate::db::models::now;
use crate::error::AppResult;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use std::collections::HashMap;
use time::{Date, OffsetDateTime};

/// Höchstens so viele Tage in `perDay`.
const MAX_DAYS: i64 = 3_660;

struct Scope<'a> {
    user_id: &'a str,
    account: Option<&'a str>,
    from: i64,
    to: i64,
    tz_secs: i64,
}

/// `WITH s AS (…)`: die Nachrichten im Zeitraum, `ld` = Ortszeit in Sekunden.
fn base(b: &mut QueryBuilder<Sqlite>, s: &Scope<'_>) {
    b.push("WITH s AS (SELECT m.*, m.date + ")
        .push_bind(s.tz_secs)
        .push(
            " AS ld FROM messages m JOIN mail_accounts a ON a.id = m.account_id \
             LEFT JOIN folders f ON f.id = m.folder_id WHERE a.user_id = ",
        )
        .push_bind(s.user_id.to_string())
        .push(" AND COALESCE(f.role, 'other') NOT IN ('junk', 'drafts') AND m.draft = 0 AND m.date >= ")
        .push_bind(s.from)
        .push(" AND m.date <= ")
        .push_bind(s.to);
    if let Some(a) = s.account {
        b.push(" AND m.account_id = ").push_bind(a.to_string());
    }
    b.push(") ");
}

fn query(s: &Scope<'_>, rest: &str) -> QueryBuilder<Sqlite> {
    let mut b = QueryBuilder::new("");
    base(&mut b, s);
    b.push(rest.to_string());
    b
}

fn day_label(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month() as u8, d.day())
}

fn local_date(ts: i64, tz_secs: i64) -> Date {
    OffsetDateTime::from_unix_timestamp(ts + tz_secs)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .date()
}

/// `GET /stats`
pub async fn stats(pool: &SqlitePool, user_id: &str, q: &StatsQuery) -> AppResult<api::Stats> {
    let tz_secs = i64::from(q.tz.clamp(-14 * 60, 14 * 60)) * 60;
    let to = now();
    let today = local_date(to, tz_secs);
    let account = q.account.as_deref().filter(|a| !a.is_empty());
    let from = if q.days > 0 {
        let days = i64::from(q.days).min(MAX_DAYS * 10);
        let first = today - time::Duration::days(days - 1);
        first.midnight().assume_utc().unix_timestamp() - tz_secs
    } else {
        let oldest: Option<i64> = sqlx::query_scalar(
            "SELECT min(m.date) FROM messages m JOIN mail_accounts a ON a.id = m.account_id
             WHERE a.user_id = ? AND (? IS NULL OR m.account_id = ?)",
        )
        .bind(user_id)
        .bind(account)
        .bind(account)
        .fetch_one(pool)
        .await?;
        oldest.unwrap_or(to).min(to)
    };
    let scope = Scope {
        user_id,
        account,
        from,
        to,
        tz_secs,
    };

    // Summen
    let (messages, received, sent, unread, flagged, with_attachments, threads, size_bytes): (
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = query(
        &scope,
        "SELECT count(*), COALESCE(sum(is_sent = 0), 0), COALESCE(sum(is_sent = 1), 0),
                COALESCE(sum(seen = 0 AND is_sent = 0), 0), COALESCE(sum(flagged = 1), 0),
                COALESCE(sum(has_attachments = 1), 0), count(DISTINCT thread_id), COALESCE(sum(size), 0) FROM s",
    )
    .build_query_as()
    .fetch_one(pool)
    .await?;
    let accounts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mail_accounts WHERE user_id = ? AND (? IS NULL OR id = ?)",
    )
    .bind(user_id)
    .bind(account)
    .bind(account)
    .fetch_one(pool)
    .await?;
    let first_day = local_date(from, tz_secs);
    let day_count = ((today - first_day).whole_days() + 1).max(1);

    // Je Tag (bei days=0 höchstens die letzten 365)
    let per_day_rows: Vec<(String, i64, i64)> = query(
        &scope,
        "SELECT strftime('%Y-%m-%d', ld, 'unixepoch') AS d, COALESCE(sum(is_sent = 0), 0), COALESCE(sum(is_sent = 1), 0)
         FROM s GROUP BY d",
    )
    .build_query_as()
    .fetch_all(pool)
    .await?;
    let by_day: HashMap<String, (i64, i64)> = per_day_rows
        .iter()
        .map(|(d, r, s)| (d.clone(), (*r, *s)))
        .collect();
    let day_limit = if q.days == 0 { 365 } else { MAX_DAYS };
    let start_day = if day_count > day_limit {
        today - time::Duration::days(day_limit - 1)
    } else {
        first_day
    };
    let mut per_day = Vec::new();
    let mut d = start_day;
    while d <= today {
        let label = day_label(d);
        let (received, sent) = by_day.get(&label).copied().unwrap_or((0, 0));
        per_day.push(api::DayCount {
            date: label,
            received,
            sent,
        });
        match d.next_day() {
            Some(n) => d = n,
            None => break,
        }
    }
    let busiest_day = per_day_rows
        .iter()
        .filter(|(_, r, _)| *r > 0)
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(date, received, sent)| api::DayCount {
            date: date.clone(),
            received: *received,
            sent: *sent,
        });

    // Je Monat
    let by_month: HashMap<String, (i64, i64)> = query(
        &scope,
        "SELECT strftime('%Y-%m', ld, 'unixepoch') AS mo, COALESCE(sum(is_sent = 0), 0), COALESCE(sum(is_sent = 1), 0)
         FROM s GROUP BY mo",
    )
    .build_query_as::<(String, i64, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(m, r, s)| (m, (r, s)))
    .collect();
    let mut per_month = Vec::new();
    let (mut y, mut m) = (first_day.year(), first_day.month() as u8);
    let (ty, tm) = (today.year(), today.month() as u8);
    while (y, m) <= (ty, tm) && per_month.len() < 1200 {
        let label = format!("{y:04}-{m:02}");
        let (received, sent) = by_month.get(&label).copied().unwrap_or((0, 0));
        per_month.push(api::MonthCount {
            month: label,
            received,
            sent,
        });
        if m == 12 {
            y += 1;
            m = 1;
        } else {
            m += 1;
        }
    }

    // Stunden, Wochentage, Heatmap (empfangene)
    let cells: Vec<(i64, i64, i64)> = query(
        &scope,
        "SELECT (CAST(strftime('%w', ld, 'unixepoch') AS INTEGER) + 6) % 7 AS wd,
                CAST(strftime('%H', ld, 'unixepoch') AS INTEGER) AS h, count(*)
         FROM s WHERE is_sent = 0 GROUP BY wd, h",
    )
    .build_query_as()
    .fetch_all(pool)
    .await?;
    let mut heatmap = vec![vec![0i64; 24]; 7];
    let mut per_hour = vec![0i64; 24];
    let mut per_weekday = vec![0i64; 7];
    for (wd, h, n) in cells {
        if let (Ok(wd), Ok(h)) = (usize::try_from(wd), usize::try_from(h)) {
            if wd < 7 && h < 24 {
                heatmap[wd][h] += n;
                per_hour[h] += n;
                per_weekday[wd] += n;
            }
        }
    }

    // Häufigste Absender, Empfänger, Domains
    let top_senders = query(
        &scope,
        "SELECT max(from_name), lower(from_address) AS addr, count(*) AS n FROM s
         WHERE is_sent = 0 AND from_address <> '' GROUP BY addr ORDER BY n DESC, addr LIMIT 10",
    )
    .build_query_as::<(String, String, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(name, address, count)| api::SenderCount {
        name,
        address,
        count,
    })
    .collect();
    let top_recipients = query(
        &scope,
        ", r(name, address) AS (
            SELECT COALESCE(json_extract(j.value, '$.name'), ''), lower(json_extract(j.value, '$.address'))
            FROM s, json_each(s.to_addrs) j WHERE s.is_sent = 1
            UNION ALL
            SELECT COALESCE(json_extract(j.value, '$.name'), ''), lower(json_extract(j.value, '$.address'))
            FROM s, json_each(s.cc_addrs) j WHERE s.is_sent = 1)
         SELECT max(name), address, count(*) AS n FROM r WHERE address IS NOT NULL AND address <> ''
         GROUP BY address ORDER BY n DESC, address LIMIT 10",
    )
    .build_query_as::<(String, String, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(name, address, count)| api::SenderCount {
        name,
        address,
        count,
    })
    .collect();
    let top_domains = query(
        &scope,
        "SELECT lower(substr(from_address, instr(from_address, '@') + 1)) AS dom, count(*) AS n FROM s
         WHERE is_sent = 0 AND instr(from_address, '@') > 0 GROUP BY dom ORDER BY n DESC, dom LIMIT 10",
    )
    .build_query_as::<(String, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(domain, count)| api::DomainCount { domain, count })
    .collect();

    let categories = query(
        &scope,
        "SELECT category, count(*) AS n FROM s WHERE is_sent = 0 GROUP BY category ORDER BY n DESC",
    )
    .build_query_as::<(String, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(c, count)| api::CategoryCount {
        category: Category::parse(&c).unwrap_or_default(),
        count,
    })
    .collect();

    let folders = query(
        &scope,
        "SELECT f.id, f.name, CASE WHEN a.name <> '' THEN a.name ELSE a.email END, count(*) AS n
         FROM s JOIN folders f ON f.id = s.folder_id JOIN mail_accounts a ON a.id = s.account_id
         GROUP BY f.id ORDER BY n DESC, f.name",
    )
    .build_query_as::<(String, String, String, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(folder_id, name, account_name, count)| api::FolderCount {
        folder_id,
        name,
        account_name,
        count,
    })
    .collect();

    let accounts_list = query(
        &scope,
        "SELECT a.id, CASE WHEN a.name <> '' THEN a.name ELSE a.email END, a.color, count(*) AS n, COALESCE(sum(s.size), 0)
         FROM s JOIN mail_accounts a ON a.id = s.account_id GROUP BY a.id ORDER BY n DESC",
    )
    .build_query_as::<(String, String, String, i64, i64)>()
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(account_id, name, color, count, size_bytes)| api::AccountCount {
        account_id,
        name,
        color,
        count,
        size_bytes,
    })
    .collect();

    Ok(api::Stats {
        from,
        to,
        totals: api::StatsTotals {
            messages,
            received,
            sent,
            unread,
            flagged,
            with_attachments,
            threads,
            size_bytes,
            accounts,
            avg_per_day: received as f64 / day_count as f64,
        },
        per_day,
        per_month,
        per_hour,
        per_weekday,
        heatmap,
        top_senders,
        top_recipients,
        top_domains,
        categories,
        folders,
        accounts: accounts_list,
        busiest_day,
    })
}
