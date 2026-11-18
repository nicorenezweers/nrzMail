//! Statistik (`GET /stats`): wie viele Mails wann kamen und gingen – Kennzahlen, je Tag und
//! Monat, Stunden, Wochentag × Stunde, Top-Absender/-Empfänger/-Domains, Kategorien,
//! Postfächer mit Speicher. Zeitraum und Postfach wählbar, Zeitzone vom Browser.

use crate::api;
use crate::components::charts::{BarChart, BarList, Donut, Heatmap, Series};
use crate::components::common::*;
use crate::pages::layout::MobileMenuButton;
use crate::state::ACCOUNTS;
use crate::util;
use dioxus::prelude::*;
use nrzmail_api::{Stats as StatsData, StatsQuery};

const RANGES: [(u32, &str); 5] = [(7, "7 Tage"), (30, "30 Tage"), (90, "90 Tage"), (365, "1 Jahr"), (0, "Alles")];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez"];
const WEEKDAYS: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];

/// "2026-10-31" → (2026, 10, 31)
fn ymd(s: &str) -> Option<(i32, u32, u32)> {
    let mut it = s.split('-');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next().and_then(|d| d.parse().ok()).unwrap_or(1)))
}

fn day_short(s: &str) -> String {
    ymd(s).map(|(_, m, d)| format!("{d}.{m}.")).unwrap_or_else(|| s.to_string())
}

fn day_long(s: &str) -> String {
    ymd(s).map(|(y, m, d)| format!("{d}. {} {y}", MONTHS[(m.clamp(1, 12) - 1) as usize])).unwrap_or_else(|| s.to_string())
}

fn month_label(s: &str) -> String {
    ymd(s).map(|(y, m, _)| format!("{} {}", MONTHS[(m.clamp(1, 12) - 1) as usize], y % 100)).unwrap_or_else(|| s.to_string())
}

/// Etwa `count` gleichmässig verteilte Beschriftungen.
fn ticks(labels: &[String], count: usize, fmt: impl Fn(&str) -> String) -> Vec<(usize, String)> {
    let n = labels.len();
    if n == 0 {
        return Vec::new();
    }
    let step = n.div_ceil(count.max(1)).max(1);
    (0..n).step_by(step).map(|i| (i, fmt(&labels[i]))).collect()
}

#[component]
pub fn Stats() -> Element {
    let mut days = use_signal(|| 30u32);
    let mut account = use_signal(String::new);
    let accounts = ACCOUNTS();
    let data = use_resource(move || async move {
        let q = StatsQuery { account: Some(account()).filter(|a| !a.is_empty()), days: days(), tz: util::tz_offset_minutes() };
        api::get::<StatsData>(&format!("/stats?{}", q.to_query())).await
    });
    rsx! {
        document::Title { "Statistik – nrzMail" }
        div { class: "flex h-14 shrink-0 items-center gap-2 border-b px-3 md:hidden",
            MobileMenuButton {}
            span { class: "font-semibold", "Statistik" }
        }
        div { class: "min-h-0 flex-1 overflow-y-auto",
            div { class: "mx-auto max-w-6xl p-4 pb-24 md:p-6 md:pb-24",
                PageHeader { title: "Statistik", description: "Wie viele Mails wann kamen und gingen.",
                    actions: rsx! {
                        div { class: "flex rounded-lg border p-0.5", role: "group", "aria-label": "Zeitraum",
                            for (d , label) in RANGES {
                                button { key: "{d}",
                                    class: if days() == d { "rounded-md bg-accent px-2.5 py-1 text-xs font-medium" } else { "rounded-md px-2.5 py-1 text-xs text-muted-foreground hover:text-foreground" },
                                    "aria-pressed": "{days() == d}",
                                    onclick: move |_| days.set(d),
                                    "{label}"
                                }
                            }
                        }
                        if accounts.len() > 1 {
                            select { class: SELECT_SM, "aria-label": "Postfach", value: "{account}",
                                onchange: move |e| account.set(e.value()),
                                option { value: "", "Alle Postfächer" }
                                for a in accounts.iter() {
                                    option { value: "{a.id}", selected: account() == a.id, "{a.label()}" }
                                }
                            }
                        }
                    },
                }
                match &*data.read() {
                    None => rsx! { StatsSkeleton {} },
                    Some(Err(e)) => rsx! { Callout { text: e.message.clone(), error: true } },
                    Some(Ok(s)) => rsx! { StatsView { stats: s.clone(), days: days() } },
                }
            }
        }
    }
}

#[component]
fn StatsSkeleton() -> Element {
    use crate::ui::skeleton::Skeleton;
    rsx! {
        div { class: "grid gap-4", "aria-busy": "true",
            div { class: "grid grid-cols-2 gap-3 md:grid-cols-4",
                for i in 0..8 { Skeleton { key: "{i}", class: "h-24" } }
            }
            Skeleton { class: "h-64" }
            div { class: "grid gap-4 md:grid-cols-2", Skeleton { class: "h-56" } Skeleton { class: "h-56" } }
        }
    }
}

#[component]
fn Tile(#[props(into)] label: String, #[props(into)] value: String, #[props(into, optional)] sub: Option<String>) -> Element {
    rsx! {
        div { class: "rounded-xl border bg-card p-4",
            p { class: "text-xs text-muted-foreground", "{label}" }
            p { class: "mt-1 text-2xl font-semibold tabular-nums tracking-tight", "{value}" }
            if let Some(s) = sub { p { class: "mt-0.5 truncate text-xs text-muted-foreground", "{s}" } }
        }
    }
}

#[component]
fn Card(#[props(into)] title: String, #[props(into, optional)] description: Option<String>, #[props(into, optional)] class: Option<String>, children: Element) -> Element {
    let c = class.unwrap_or_default();
    rsx! {
        section { class: "rounded-xl border bg-card p-4 md:p-5 {c}",
            h2 { class: "font-semibold leading-none", "{title}" }
            if let Some(d) = description { p { class: "mt-1.5 text-xs text-muted-foreground", "{d}" } }
            div { class: "mt-4", {children} }
        }
    }
}

#[component]
fn StatsView(stats: StatsData, days: u32) -> Element {
    let s = stats;
    let t = &s.totals;
    let range = if days == 0 {
        format!("seit {}", util::date(s.from))
    } else {
        format!("{} – {}", util::date(s.from), util::date(s.to))
    };
    let day_labels: Vec<String> = s.per_day.iter().map(|d| d.date.clone()).collect();
    let month_labels: Vec<String> = s.per_month.iter().map(|m| m.month.clone()).collect();
    let hours: Vec<String> = (0..24).map(|h| format!("{h}:00–{}:00", h + 1)).collect();
    let busiest = s.busiest_day.as_ref().filter(|d| d.received > 0).map(|d| format!("{} am {}", util::num(d.received), day_long(&d.date)));
    let max_account = s.accounts.iter().map(|a| a.size_bytes).max().unwrap_or(0).max(1);
    let weekday_sum: i64 = s.per_weekday.iter().sum();
    rsx! {
        p { class: "-mt-3 mb-5 text-xs text-muted-foreground", "{range}" }
        div { class: "grid gap-4",
            div { class: "grid grid-cols-2 gap-3 md:grid-cols-4",
                Tile { label: "Nachrichten", value: util::num(t.messages), sub: util::plural(t.threads, "Unterhaltung", "Unterhaltungen") }
                Tile { label: "Empfangen", value: util::num(t.received), sub: format!("Ø {} pro Tag", util::decimal(t.avg_per_day)) }
                Tile { label: "Gesendet", value: util::num(t.sent), sub: if t.received > 0 { format!("{:.0} % der empfangenen", t.sent as f64 / t.received as f64 * 100.0) } else { String::new() } }
                Tile { label: "Speicher", value: util::size(t.size_bytes), sub: util::plural(t.accounts, "Postfach", "Postfächer") }
                Tile { label: "Ungelesen", value: util::num(t.unread) }
                Tile { label: "Mit Stern", value: util::num(t.flagged) }
                Tile { label: "Mit Anhang", value: util::num(t.with_attachments), sub: if t.messages > 0 { format!("{:.0} %", t.with_attachments as f64 / t.messages as f64 * 100.0) } else { String::new() } }
                Tile { label: "Meiste an einem Tag", value: s.busiest_day.as_ref().map(|d| util::num(d.received)).unwrap_or_else(|| "–".into()), sub: busiest.clone().unwrap_or_default() }
            }
            Card { title: "Je Tag", description: "Empfangene und gesendete Nachrichten",
                BarChart {
                    series: vec![
                        Series { name: "Empfangen", class: "fill-foreground/55", values: s.per_day.iter().map(|d| d.received).collect() },
                        Series { name: "Gesendet", class: "fill-success/80", values: s.per_day.iter().map(|d| d.sent).collect() },
                    ],
                    tooltips: day_labels.iter().map(|d| day_long(d)).collect::<Vec<_>>(),
                    ticks: ticks(&day_labels, 7, day_short),
                    height: "h-52",
                }
            }
            if s.per_month.len() > 1 {
                Card { title: "Je Monat",
                    BarChart {
                        series: vec![
                            Series { name: "Empfangen", class: "fill-foreground/55", values: s.per_month.iter().map(|m| m.received).collect() },
                            Series { name: "Gesendet", class: "fill-success/80", values: s.per_month.iter().map(|m| m.sent).collect() },
                        ],
                        tooltips: month_labels.iter().map(|m| month_label(m)).collect::<Vec<_>>(),
                        ticks: ticks(&month_labels, 8, month_label),
                        height: "h-44",
                    }
                }
            }
            div { class: "grid gap-4 lg:grid-cols-2",
                Card { title: "Tageszeit", description: "Empfangene je Stunde",
                    BarChart {
                        series: vec![Series { name: "Empfangen", class: "fill-foreground/55", values: s.per_hour.clone() }],
                        tooltips: hours,
                        ticks: vec![(0, "0".to_string()), (6, "6".to_string()), (12, "12".to_string()), (18, "18".to_string()), (23, "23".to_string())],
                        height: "h-40",
                    }
                }
                Card { title: "Wochentag", description: "Empfangene je Wochentag",
                    BarChart {
                        series: vec![Series { name: "Empfangen", class: "fill-foreground/55", values: s.per_weekday.clone() }],
                        tooltips: WEEKDAYS.iter().enumerate().map(|(i, d)| {
                            let v = s.per_weekday.get(i).copied().unwrap_or(0);
                            if weekday_sum > 0 { format!("{d} ({:.0} %)", v as f64 / weekday_sum as f64 * 100.0) } else { d.to_string() }
                        }).collect::<Vec<_>>(),
                        ticks: WEEKDAYS.iter().enumerate().map(|(i, d)| (i, d.to_string())).collect::<Vec<_>>(),
                        height: "h-40",
                    }
                }
            }
            Card { title: "Wochentag × Stunde", description: "Wann die meisten Nachrichten ankommen",
                Heatmap { values: s.heatmap.clone() }
            }
            div { class: "grid gap-4 lg:grid-cols-3",
                Card { title: "Häufigste Absender",
                    BarList { rows: s.top_senders.iter().map(|x| (if x.name.trim().is_empty() { x.address.clone() } else { x.name.clone() }, x.address.clone(), x.count)).collect::<Vec<_>>() }
                }
                Card { title: "Häufigste Empfänger", description: "An wen du schreibst",
                    BarList { rows: s.top_recipients.iter().map(|x| (if x.name.trim().is_empty() { x.address.clone() } else { x.name.clone() }, x.address.clone(), x.count)).collect::<Vec<_>>(),
                        empty: "Im Zeitraum nichts gesendet." }
                }
                Card { title: "Domains",
                    BarList { rows: s.top_domains.iter().map(|x| (x.domain.clone(), String::new(), x.count)).collect::<Vec<_>>() }
                }
            }
            div { class: "grid gap-4 lg:grid-cols-2",
                Card { title: "Kategorien", description: "Einteilung der empfangenen Nachrichten",
                    Donut { parts: s.categories.iter().map(|c| (c.category.label().to_string(), c.count)).collect::<Vec<_>>() }
                }
                Card { title: "Postfächer", description: "Nachrichten und Speicher",
                    if s.accounts.is_empty() {
                        p { class: "py-6 text-center text-sm text-muted-foreground", "Keine Postfächer." }
                    }
                    div { class: "grid gap-3",
                        for a in s.accounts.iter() {
                            div { key: "{a.account_id}", class: "grid gap-1.5",
                                div { class: "flex items-center gap-2 text-sm",
                                    Dot { color: a.color.clone() }
                                    span { class: "min-w-0 flex-1 truncate", "{a.name}" }
                                    span { class: "tabular-nums text-muted-foreground", {util::num(a.count)} }
                                    span { class: "w-20 text-right text-xs tabular-nums text-muted-foreground", {util::size(a.size_bytes)} }
                                }
                                div { class: "h-1.5 overflow-hidden rounded-full bg-muted",
                                    div { class: "h-full rounded-full bg-foreground/50", style: "width: {a.size_bytes as f64 / max_account as f64 * 100.0}%" }
                                }
                            }
                        }
                    }
                    if !s.folders.is_empty() {
                        p { class: "mb-2 mt-5 text-xs font-medium uppercase tracking-wider text-muted-foreground", "Grösste Ordner" }
                        BarList { rows: s.folders.iter().take(8).map(|f| (f.name.clone(), f.account_name.clone(), f.count)).collect::<Vec<_>>() }
                    }
                }
            }
        }
    }
}
