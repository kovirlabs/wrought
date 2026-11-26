//! Helpers behind the App UI components (`request` routes): the data
//! table's server-side sort / filter / paginate, and server-rendered SVG
//! charts. Both work without JavaScript; the components in
//! `builtin-app/components/` render their output as semantic HTML.
//!
//! * `table_view(rows, columns, id="t", per_page=20, args=request.args,
//!   path=request.path)` returns what `components/data_table.html` prints:
//!   sorted, filtered and paginated rows plus the links for each header and
//!   page. Query parameters are namespaced by the table's `id`
//!   (`t_sort`, `t_dir`, `t_q`, `t_page`), so a page can hold several
//!   tables.
//! * `chart_svg(labels, series, kind="bar", title, width, height)` returns
//!   an `<svg role="img">` (a *safe* value) with a `<title>`, a `<desc>`
//!   summarising the data, and per-point `<title>` tooltips. Colours come
//!   from the CSS custom properties `--chart-1` .. `--chart-4` (falling
//!   back to TagBrush's `--color-primary`).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use minijinja::value::{Kwargs, Value};
use minijinja::{Error, ErrorKind};
use serde::Serialize;
use serde_json::Value as Json;

use crate::web::url_encode;

#[derive(Debug, Clone, Serialize)]
struct Column {
    key: String,
    label: String,
    num: bool,
    sortable: bool,
    /// `ascending` / `descending` when sorted by this column.
    aria_sort: Option<&'static str>,
    sort_url: String,
}

#[derive(Debug, Clone, Serialize)]
struct PageLink {
    number: usize,
    url: String,
    current: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TableView {
    id: String,
    columns: Vec<Column>,
    rows: Vec<Vec<Json>>,
    /// The original row objects of this page (for custom cells).
    items: Vec<Json>,
    total: usize,
    shown_from: usize,
    shown_to: usize,
    page: usize,
    pages: usize,
    prev_url: Option<String>,
    next_url: Option<String>,
    page_links: Vec<PageLink>,
    q: String,
    q_name: String,
    hidden: Vec<(String, String)>,
    path: String,
}

fn cmp_json(a: &Json, b: &Json) -> Ordering {
    match (a, b) {
        (Json::Null, Json::Null) => Ordering::Equal,
        (Json::Null, _) => Ordering::Greater,
        (_, Json::Null) => Ordering::Less,
        (Json::Number(x), Json::Number(y)) => x
            .as_f64()
            .partial_cmp(&y.as_f64())
            .unwrap_or(Ordering::Equal),
        (Json::Bool(x), Json::Bool(y)) => x.cmp(y),
        _ => text(a).to_lowercase().cmp(&text(b).to_lowercase()),
    }
}

fn text(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

fn link(path: &str, params: &BTreeMap<String, String>) -> String {
    if params.is_empty() {
        return path.to_string();
    }
    let q: Vec<String> = params
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect();
    if q.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", q.join("&"))
    }
}

/// `table_view(rows, columns, ...)`.
pub fn table_view(rows: Value, columns: Value, kwargs: Kwargs) -> Result<Value, Error> {
    let bad = |m: &str| Error::new(ErrorKind::InvalidOperation, format!("table_view: {m}"));
    let id: Option<String> = kwargs.get("id")?;
    let id = id.unwrap_or_else(|| "t".into());
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad("id must be letters, digits or `_`"));
    }
    let per_page: Option<usize> = kwargs.get("per_page")?;
    let per_page = per_page.unwrap_or(20).clamp(1, 500);
    let args: Option<Value> = kwargs.get("args")?;
    let path: Option<String> = kwargs.get("path")?;
    kwargs.assert_all_used()?;
    let path = path.unwrap_or_default();
    let args: BTreeMap<String, String> = args
        .map(|a| crate::templates::value_map(&a))
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| {
            (
                k,
                v.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string()),
            )
        })
        .collect();

    let rows: Vec<Json> = serde_json::to_value(&rows)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| bad("rows must be a list"))?;
    let cols_json: Vec<Json> = serde_json::to_value(&columns)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| bad("columns must be a list"))?;
    let mut cols: Vec<(String, String, bool, bool)> = Vec::new();
    for c in &cols_json {
        match c {
            Json::String(k) => cols.push((k.clone(), k.clone(), false, true)),
            Json::Object(m) => {
                let key = m
                    .get("key")
                    .and_then(Json::as_str)
                    .ok_or_else(|| bad("each column needs a `key`"))?
                    .to_string();
                let label = m
                    .get("label")
                    .and_then(Json::as_str)
                    .unwrap_or(&key)
                    .to_string();
                let num = m.get("num").and_then(Json::as_bool).unwrap_or(false);
                let sortable = m.get("sort").and_then(Json::as_bool).unwrap_or(true);
                cols.push((key, label, num, sortable));
            }
            _ => return Err(bad("a column is a key or {key, label, num, sort}")),
        }
    }

    let p = |n: &str| format!("{id}_{n}");
    let q = args.get(&p("q")).cloned().unwrap_or_default();
    let sort = args
        .get(&p("sort"))
        .filter(|s| cols.iter().any(|c| &c.0 == *s && c.3))
        .cloned();
    let desc = args.get(&p("dir")).is_some_and(|d| d == "desc");

    let cell = |row: &Json, key: &str| row.get(key).cloned().unwrap_or(Json::Null);
    let needle = q.trim().to_lowercase();
    let mut filtered: Vec<Json> = rows
        .into_iter()
        .filter(|r| {
            needle.is_empty()
                || cols
                    .iter()
                    .any(|c| text(&cell(r, &c.0)).to_lowercase().contains(&needle))
        })
        .collect();
    if let Some(s) = &sort {
        filtered.sort_by(|a, b| {
            let o = cmp_json(&cell(a, s), &cell(b, s));
            if desc { o.reverse() } else { o }
        });
    }
    let total = filtered.len();
    let pages = total.div_ceil(per_page).max(1);
    let page = args
        .get(&p("page"))
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, pages);
    let start = (page - 1) * per_page;
    let items: Vec<Json> = filtered.into_iter().skip(start).take(per_page).collect();

    // Other parameters on the page survive our links.
    let base: BTreeMap<String, String> = args.clone();
    let columns = cols
        .iter()
        .map(|(key, label, num, sortable)| {
            let mut m = base.clone();
            let now_asc = sort.as_deref() == Some(key.as_str()) && !desc;
            m.insert(p("sort"), key.clone());
            m.insert(p("dir"), if now_asc { "desc".into() } else { "asc".into() });
            m.remove(&p("page"));
            Column {
                key: key.clone(),
                label: label.clone(),
                num: *num,
                sortable: *sortable,
                aria_sort: (sort.as_deref() == Some(key.as_str())).then_some(if desc {
                    "descending"
                } else {
                    "ascending"
                }),
                sort_url: link(&path, &m),
            }
        })
        .collect();
    let page_url = |n: usize| {
        let mut m = base.clone();
        if n == 1 {
            m.remove(&p("page"));
        } else {
            m.insert(p("page"), n.to_string());
        }
        link(&path, &m)
    };
    let hidden: Vec<(String, String)> = base
        .iter()
        .filter(|(k, _)| **k != p("q") && **k != p("page"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let view = TableView {
        id: id.clone(),
        rows: items
            .iter()
            .map(|r| cols.iter().map(|c| cell(r, &c.0)).collect())
            .collect(),
        shown_from: if total == 0 { 0 } else { start + 1 },
        shown_to: start + items.len(),
        items,
        columns,
        total,
        page,
        pages,
        prev_url: (page > 1).then(|| page_url(page - 1)),
        next_url: (page < pages).then(|| page_url(page + 1)),
        page_links: (1..=pages)
            .filter(|n| pages <= 9 || *n == 1 || *n == pages || n.abs_diff(page) <= 2)
            .map(|n| PageLink {
                number: n,
                url: page_url(n),
                current: n == page,
            })
            .collect(),
        q,
        q_name: p("q"),
        hidden,
        path,
    };
    Ok(Value::from_serialize(&view))
}

const COLORS: [&str; 4] = [
    "var(--chart-1, var(--color-primary, #2563eb))",
    "var(--chart-2, #d97706)",
    "var(--chart-3, #059669)",
    "var(--chart-4, #db2777)",
];

fn esc(s: &str) -> String {
    let mut o = String::new();
    let _ = crate::templates::escape_html(s, &mut o);
    o
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.2}")
    }
}

/// `chart_svg(labels, series, kind="bar", title="", width=640, height=240)`.
/// `series` is `[{name, values}]` (or one list of numbers).
pub fn chart_svg(labels: Value, series: Value, kwargs: Kwargs) -> Result<Value, Error> {
    let bad = |m: &str| Error::new(ErrorKind::InvalidOperation, format!("chart_svg: {m}"));
    let kind: Option<String> = kwargs.get("kind")?;
    let kind = kind.unwrap_or_else(|| "bar".into());
    if kind != "bar" && kind != "line" {
        return Err(bad("kind is `bar` or `line`"));
    }
    let title: Option<String> = kwargs.get("title")?;
    let title = title.unwrap_or_default();
    let width: Option<u32> = kwargs.get("width")?;
    let height: Option<u32> = kwargs.get("height")?;
    kwargs.assert_all_used()?;
    let (w, h) = (
        f64::from(width.unwrap_or(640).clamp(200, 2000)),
        f64::from(height.unwrap_or(240).clamp(120, 1200)),
    );
    let labels: Vec<String> = serde_json::to_value(&labels)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| bad("labels must be a list"))?
        .iter()
        .map(text)
        .collect();
    let series_json = serde_json::to_value(&series).map_err(|_| bad("bad series"))?;
    let mut sets: Vec<(String, Vec<f64>)> = Vec::new();
    let nums = |v: &Json| -> Option<Vec<f64>> {
        v.as_array()
            .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
    };
    match &series_json {
        Json::Array(a) if a.iter().all(|x| x.is_number()) => {
            sets.push((String::new(), nums(&series_json).unwrap_or_default()));
        }
        Json::Array(a) => {
            for s in a {
                let name = s.get("name").map(text).unwrap_or_default();
                let values = s
                    .get("values")
                    .and_then(nums)
                    .ok_or_else(|| bad("each series needs `values`"))?;
                sets.push((name, values));
            }
        }
        _ => return Err(bad("series must be a list")),
    }
    if sets.len() > COLORS.len() {
        return Err(bad("at most 4 series"));
    }
    let n = labels.len();
    if n == 0 || sets.iter().any(|(_, v)| v.len() != n) {
        return Err(bad("every series needs one value per label"));
    }
    let max = sets
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .fold(0.0f64, f64::max);
    let min = sets
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .fold(0.0f64, f64::min);
    let (left, right, top, bottom) = (44.0, 12.0, 12.0, 34.0);
    let (pw, ph) = (w - left - right, h - top - bottom);
    let span = if max - min > 0.0 { max - min } else { 1.0 };
    let y = |v: f64| top + ph - (v - min) / span * ph;

    let mut desc = String::new();
    for (name, values) in &sets {
        if !name.is_empty() {
            let _ = write!(desc, "{name}: ");
        }
        let parts: Vec<String> = labels
            .iter()
            .zip(values)
            .map(|(l, v)| format!("{l} {}", fmt_num(*v)))
            .collect();
        let _ = write!(desc, "{}. ", parts.join(", "));
    }
    let id = format!(
        "chart-{:x}",
        desc.bytes()
            .fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b))
                .wrapping_mul(0x100000001b3))
    );
    let mut s = String::new();
    let _ = write!(
        s,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" role="img" aria-labelledby="{id}-t {id}-d" font-size="12" font-family="system-ui, sans-serif">"#
    );
    let _ = write!(
        s,
        r#"<title id="{id}-t">{}</title><desc id="{id}-d">{}</desc>"#,
        esc(if title.is_empty() { "Chart" } else { &title }),
        esc(desc.trim())
    );
    // Grid lines and y labels.
    for i in 0..=4 {
        let v = min + span * f64::from(i) / 4.0;
        let yy = y(v);
        let _ = write!(
            s,
            r#"<line x1="{left}" x2="{}" y1="{yy:.1}" y2="{yy:.1}" stroke="currentColor" stroke-opacity="0.15"/><text x="{}" y="{:.1}" text-anchor="end" fill="currentColor">{}</text>"#,
            w - right,
            left - 6.0,
            yy + 4.0,
            fmt_num(v)
        );
    }
    let slot = pw / n as f64;
    for (i, l) in labels.iter().enumerate() {
        let _ = write!(
            s,
            r#"<text x="{:.1}" y="{:.1}" text-anchor="middle" fill="currentColor">{}</text>"#,
            left + slot * (i as f64 + 0.5),
            h - bottom + 18.0,
            esc(l)
        );
    }
    let zero = y(0.0f64.clamp(min, max));
    for (si, (name, values)) in sets.iter().enumerate() {
        let color = COLORS[si];
        let tip = |l: &str, v: f64| {
            if name.is_empty() {
                format!("{l}: {}", fmt_num(v))
            } else {
                format!("{name}, {l}: {}", fmt_num(v))
            }
        };
        if kind == "bar" {
            let bw = (slot * 0.7) / sets.len() as f64;
            for (i, v) in values.iter().enumerate() {
                let x = left + slot * i as f64 + slot * 0.15 + bw * si as f64;
                let (y0, y1) = if *v >= 0.0 {
                    (y(*v), zero)
                } else {
                    (zero, y(*v))
                };
                let _ = write!(
                    s,
                    r#"<rect x="{x:.1}" y="{y0:.1}" width="{bw:.1}" height="{:.1}" fill="{color}"><title>{}</title></rect>"#,
                    (y1 - y0).max(0.5),
                    esc(&tip(&labels[i], *v))
                );
            }
        } else {
            let pts: Vec<String> = values
                .iter()
                .enumerate()
                .map(|(i, v)| format!("{:.1},{:.1}", left + slot * (i as f64 + 0.5), y(*v)))
                .collect();
            let _ = write!(
                s,
                r#"<polyline points="{}" fill="none" stroke="{color}" stroke-width="2"/>"#,
                pts.join(" ")
            );
            for (i, v) in values.iter().enumerate() {
                let _ = write!(
                    s,
                    r#"<circle cx="{:.1}" cy="{:.1}" r="3.5" fill="{color}"><title>{}</title></circle>"#,
                    left + slot * (i as f64 + 0.5),
                    y(*v),
                    esc(&tip(&labels[i], *v))
                );
            }
        }
    }
    s.push_str("</svg>");
    Ok(Value::from_safe_string(s))
}

/// Register `table_view` and `chart_svg`.
pub fn register(env: &mut minijinja::Environment<'static>) {
    env.add_function("table_view", table_view);
    env.add_function("chart_svg", chart_svg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::{Environment, context};

    fn env() -> Environment<'static> {
        let mut e = Environment::new();
        register(&mut e);
        e
    }

    #[test]
    fn table_sorts_filters_and_paginates() {
        let rows = serde_json::json!([
            {"id": 3, "name": "Cherry"}, {"id": 1, "name": "apple"},
            {"id": 2, "name": "Banana"}, {"id": 10, "name": "date"},
        ]);
        let e = env();
        let r = |args: serde_json::Value| {
            e.render_str(
                r#"{% set t = table_view(rows, ["id", {"key": "name", "label": "Name"}], id="f", per_page=2, args=args, path="/x/") %}{% for r in t.rows %}{{ r[0] }},{% endfor %}|{{ t.page }}/{{ t.pages }}|{{ t.columns[0].sort_url }}|{{ t.columns[0].aria_sort or "" }}|{{ t.next_url or "" }}"#,
                context! { rows => rows.clone(), args => args },
            )
            .unwrap()
        };
        assert_eq!(
            r(serde_json::json!({})),
            "3,1,|1/2|/x/?f_dir=asc&f_sort=id||/x/?f_page=2"
        );
        assert_eq!(
            r(serde_json::json!({"f_sort": "id", "f_dir": "desc"})),
            "10,3,|1/2|/x/?f_dir=asc&f_sort=id|descending|/x/?f_dir=desc&f_page=2&f_sort=id"
        );
        assert_eq!(
            r(serde_json::json!({"f_sort": "name", "f_page": "2"})),
            "3,10,|2/2|/x/?f_dir=asc&f_sort=id||"
        );
        assert_eq!(
            r(serde_json::json!({"f_q": "AN"})),
            "2,|1/1|/x/?f_dir=asc&f_q=AN&f_sort=id||"
        );
        // Unknown sort columns and silly pages are ignored.
        assert_eq!(
            r(serde_json::json!({"f_sort": "x", "f_page": "99"}))
                .split('|')
                .nth(1)
                .unwrap(),
            "2/2"
        );
    }

    #[test]
    fn charts_are_accessible_svg_and_escape_text() {
        let e = env();
        let out = e
            .render_str(
                r#"{{ chart_svg(["Mon", "<Tue>"], [{"name": "Orders", "values": [3, 5]}], title="Week & more") }}"#,
                context! {},
            )
            .unwrap();
        assert!(out.starts_with("<svg") && out.ends_with("</svg>"), "{out}");
        assert!(out.contains(r#"role="img""#));
        assert!(out.contains("<title id=") && out.contains("Week &amp; more"));
        assert!(out.contains("Orders: Mon 3, &lt;Tue&gt; 5."), "{out}");
        assert_eq!(out.matches("<rect").count(), 2);
        let line = e
            .render_str(
                r#"{{ chart_svg(["a","b","c"], [1, 4, 2], kind="line") }}"#,
                context! {},
            )
            .unwrap();
        assert!(line.contains("<polyline") && line.matches("<circle").count() == 3);
        assert!(
            e.render_str(r#"{{ chart_svg(["a"], [1, 2]) }}"#, context! {})
                .is_err()
        );
    }
}
