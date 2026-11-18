//! Diagramme als SVG, in Rust erzeugt: Säulen (auch gestapelt), Heatmap und Ring. Nur
//! Grautöne mit dezenten Akzenten, Werte als Tooltip (`<title>`). Die Flächen strecken sich
//! auf die Breite (`preserveAspectRatio="none"`), Beschriftungen stehen als HTML daneben –
//! so bleibt die Schrift auf jedem Bildschirm gleich gross.

use crate::util;
use dioxus::prelude::*;

/// Für Text in SVG (Tooltips).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Runde Obergrenze für die Achse: 1, 2, 5 × 10ⁿ.
pub fn nice_max(v: i64) -> i64 {
    if v <= 0 {
        return 1;
    }
    let mag = 10f64.powf((v as f64).log10().floor());
    let f = v as f64 / mag;
    let nf = if f <= 1.0 {
        1.0
    } else if f <= 2.0 {
        2.0
    } else if f <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nf * mag).round().max(1.0) as i64
}

/// Eine Reihe im Säulendiagramm.
#[derive(Clone, PartialEq)]
pub struct Series {
    pub name: &'static str,
    /// Tailwind-Klasse für die Füllung, z. B. `fill-foreground/60`
    pub class: &'static str,
    pub values: Vec<i64>,
}

/// Säulen, mehrere Reihen gestapelt. `tooltips[i]` beschriftet Säule i,
/// `ticks` sind (Index, Text) für die x-Achse.
#[component]
pub fn BarChart(
    series: Vec<Series>,
    tooltips: Vec<String>,
    ticks: Vec<(usize, String)>,
    #[props(default = "h-48")] height: &'static str,
) -> Element {
    let n = tooltips.len().max(series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    if n == 0 {
        return rsx! { p { class: "py-8 text-center text-sm text-muted-foreground", "Keine Daten im Zeitraum." } };
    }
    let total = |i: usize| series.iter().map(|s| s.values.get(i).copied().unwrap_or(0)).sum::<i64>();
    let max = nice_max((0..n).map(total).max().unwrap_or(0));
    let gap = if n > 120 { 0.0 } else if n > 40 { 0.15 } else { 0.25 };
    let mut svg = format!(
        "<svg viewBox=\"0 0 {n} 100\" preserveAspectRatio=\"none\" class=\"absolute inset-0 h-full w-full overflow-visible\" role=\"img\">\
         <line x1=\"0\" x2=\"{n}\" y1=\"0.5\" y2=\"0.5\" class=\"stroke-border\" vector-effect=\"non-scaling-stroke\" stroke-dasharray=\"3 3\"/>\
         <line x1=\"0\" x2=\"{n}\" y1=\"50\" y2=\"50\" class=\"stroke-border\" vector-effect=\"non-scaling-stroke\" stroke-dasharray=\"3 3\"/>\
         <line x1=\"0\" x2=\"{n}\" y1=\"100\" y2=\"100\" class=\"stroke-border\" vector-effect=\"non-scaling-stroke\"/>"
    );
    for i in 0..n {
        let label = tooltips.get(i).cloned().unwrap_or_default();
        let parts: Vec<String> = series
            .iter()
            .map(|s| format!("{}: {}", s.name, util::num(s.values.get(i).copied().unwrap_or(0))))
            .collect();
        let title = if series.len() == 1 { format!("{label}: {}", util::num(total(i))) } else { format!("{label}\n{}", parts.join("\n")) };
        svg.push_str(&format!("<g><title>{}</title><rect x=\"{i}\" y=\"0\" width=\"1\" height=\"100\" fill=\"transparent\"/>", esc(&title)));
        let mut base = 100.0;
        for s in &series {
            let v = s.values.get(i).copied().unwrap_or(0);
            if v <= 0 {
                continue;
            }
            let h = v as f64 / max as f64 * 100.0;
            base -= h;
            svg.push_str(&format!(
                "<rect x=\"{:.3}\" y=\"{base:.3}\" width=\"{:.3}\" height=\"{h:.3}\" class=\"{}\"/>",
                i as f64 + gap / 2.0,
                1.0 - gap,
                s.class
            ));
        }
        svg.push_str("</g>");
    }
    svg.push_str("</svg>");
    let half = max / 2;
    rsx! {
        div { class: "grid gap-1.5",
            div { class: "flex gap-2",
                div { class: "relative w-8 shrink-0 text-right text-[11px] tabular-nums text-muted-foreground {height}",
                    span { class: "absolute right-0 top-0 -translate-y-1/2", {util::num(max)} }
                    span { class: "absolute right-0 top-1/2 -translate-y-1/2", {util::num(half)} }
                    span { class: "absolute right-0 bottom-0 translate-y-1/2", "0" }
                }
                div { class: "chart relative min-w-0 flex-1 {height}", dangerous_inner_html: "{svg}" }
            }
            div { class: "relative ml-10 h-4 text-[11px] text-muted-foreground",
                for (i , t) in ticks {
                    span { key: "{i}", class: "absolute -translate-x-1/2 whitespace-nowrap",
                        style: "left: {(i as f64 + 0.5) / n as f64 * 100.0}%",
                        "{t}"
                    }
                }
            }
            if series.len() > 1 {
                div { class: "ml-10 flex flex-wrap gap-4 text-xs text-muted-foreground",
                    for s in series.iter() {
                        span { class: "flex items-center gap-1.5",
                            svg { class: "size-2.5", view_box: "0 0 10 10", rect { width: "10", height: "10", rx: "2", class: s.class } }
                            "{s.name}"
                        }
                    }
                }
            }
        }
    }
}

/// Wochentag × Stunde (7 × 24). Je mehr, desto heller.
#[component]
pub fn Heatmap(values: Vec<Vec<i64>>) -> Element {
    const DAYS: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];
    const FULL: [&str; 7] = ["Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag"];
    let max = values.iter().flatten().copied().max().unwrap_or(0).max(1);
    let mut svg = String::from("<svg viewBox=\"0 0 24 7\" preserveAspectRatio=\"none\" class=\"absolute inset-0 h-full w-full\" role=\"img\">");
    for (d, row) in values.iter().take(7).enumerate() {
        for h in 0..24 {
            let v = row.get(h).copied().unwrap_or(0);
            let opacity = if v == 0 { 0.06 } else { 0.18 + 0.82 * (v as f64 / max as f64) };
            svg.push_str(&format!(
                "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"0.88\" height=\"0.84\" rx=\"0.12\" class=\"fill-foreground\" fill-opacity=\"{opacity:.3}\"><title>{} {h}–{} Uhr: {}</title></rect>",
                h as f64 + 0.06,
                d as f64 + 0.08,
                FULL[d],
                h + 1,
                util::num(v)
            ));
        }
    }
    svg.push_str("</svg>");
    rsx! {
        div { class: "grid gap-1.5",
            div { class: "flex gap-2",
                div { class: "grid w-6 shrink-0 grid-rows-7 text-[11px] text-muted-foreground",
                    for d in DAYS { span { class: "flex items-center", "{d}" } }
                }
                div { class: "chart relative h-44 min-w-0 flex-1", dangerous_inner_html: "{svg}" }
            }
            div { class: "relative ml-8 h-4 text-[11px] text-muted-foreground",
                for h in [0, 6, 12, 18] {
                    span { key: "{h}", class: "absolute", style: "left: {h as f64 / 24.0 * 100.0}%", "{h}:00" }
                }
                span { class: "absolute right-0", "24:00" }
            }
        }
    }
}

/// Ring mit Anteilen und Legende.
#[component]
pub fn Donut(parts: Vec<(String, i64)>) -> Element {
    // Grautöne von hell nach dunkel, ein dezenter Akzent
    const CLASSES: [&str; 6] = ["stroke-foreground", "stroke-foreground/70", "stroke-foreground/45", "stroke-foreground/25", "stroke-success/80", "stroke-foreground/15"];
    const DOTS: [&str; 6] = ["bg-foreground", "bg-foreground/70", "bg-foreground/45", "bg-foreground/25", "bg-success/80", "bg-foreground/15"];
    let total: i64 = parts.iter().map(|(_, v)| *v).sum();
    if total == 0 {
        return rsx! { p { class: "py-8 text-center text-sm text-muted-foreground", "Keine Daten im Zeitraum." } };
    }
    // Kreis mit Umfang 100: stroke-dasharray = Anteil in Prozent
    let r = 15.915_494;
    let mut svg = format!(
        "<svg viewBox=\"0 0 42 42\" class=\"size-40 -rotate-90\" role=\"img\"><circle cx=\"21\" cy=\"21\" r=\"{r}\" fill=\"none\" stroke-width=\"6\" class=\"stroke-muted\"/>"
    );
    let mut offset = 0.0;
    for (i, (label, v)) in parts.iter().enumerate() {
        if *v <= 0 {
            continue;
        }
        let pct = *v as f64 / total as f64 * 100.0;
        svg.push_str(&format!(
            "<circle cx=\"21\" cy=\"21\" r=\"{r}\" fill=\"none\" stroke-width=\"6\" class=\"{}\" stroke-dasharray=\"{pct:.3} {:.3}\" stroke-dashoffset=\"{:.3}\"><title>{}: {} ({:.0} %)</title></circle>",
            CLASSES[i % CLASSES.len()],
            100.0 - pct,
            -offset,
            esc(label),
            util::num(*v),
            pct
        ));
        offset += pct;
    }
    svg.push_str("</svg>");
    rsx! {
        div { class: "flex flex-wrap items-center gap-6",
            div { class: "chart relative grid place-items-center",
                div { dangerous_inner_html: "{svg}" }
                div { class: "absolute text-center",
                    p { class: "text-lg font-semibold tabular-nums", {util::num(total)} }
                    p { class: "text-[11px] text-muted-foreground", "Nachrichten" }
                }
            }
            div { class: "grid min-w-48 flex-1 gap-2 text-sm",
                for (i , (label , v)) in parts.iter().enumerate() {
                    div { key: "{i}", class: "flex items-center gap-2",
                        span { class: "size-2.5 shrink-0 rounded-sm {DOTS[i % DOTS.len()]}" }
                        span { class: "min-w-0 flex-1 truncate", "{label}" }
                        span { class: "tabular-nums text-muted-foreground", {util::num(*v)} }
                        span { class: "w-10 text-right text-xs tabular-nums text-muted-foreground", {format!("{:.0} %", *v as f64 / total as f64 * 100.0)} }
                    }
                }
            }
        }
    }
}

/// Liste mit Balken (Top-Absender, Domains …).
#[component]
pub fn BarList(rows: Vec<(String, String, i64)>, #[props(default = "Keine Daten im Zeitraum.")] empty: &'static str) -> Element {
    let max = rows.iter().map(|r| r.2).max().unwrap_or(0).max(1);
    if rows.is_empty() {
        return rsx! { p { class: "py-6 text-center text-sm text-muted-foreground", "{empty}" } };
    }
    rsx! {
        div { class: "grid gap-1.5",
            for (i , (label , sub , v)) in rows.into_iter().enumerate() {
                div { key: "{i}", class: "relative overflow-hidden rounded-md px-2.5 py-1.5", title: if sub.is_empty() { label.clone() } else { format!("{label} – {sub}") },
                    div { class: "absolute inset-y-0 left-0 rounded-md bg-foreground/[0.07]", style: "width: {v as f64 / max as f64 * 100.0}%" }
                    div { class: "relative flex items-center gap-2 text-sm",
                        span { class: "min-w-0 flex-1 truncate", "{label}"
                            if !sub.is_empty() && sub != label { span { class: "ml-1.5 text-xs text-muted-foreground", "{sub}" } }
                        }
                        span { class: "shrink-0 tabular-nums text-muted-foreground", {util::num(v)} }
                    }
                }
            }
        }
    }
}
