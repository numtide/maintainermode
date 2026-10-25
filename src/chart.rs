//! Charts drawn on the server as inline SVG, so the page needs no script.
//! Each chart has one fixed viewBox and scales to its card. The cards are
//! 340 to 500 px wide, so text of 13 units shows at 11 to 15 px. A `<title>`
//! on each month band is the tooltip, and the table under each chart holds
//! every value, so no value depends on colour or hover.

use crate::html::{Html, html, join};

const W: f64 = 400.0;
const H: f64 = 184.0;
const TOP: f64 = 18.0;
const BOTTOM: f64 = 160.0;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Unit {
    Count,
    Share,
    Months,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Form {
    /// One line each; a single series also gets a wash under it.
    Lines,
    /// Areas stacked from the first series up.
    Stack,
    /// Columns from zero; at most one series should have a value per month.
    Bars,
}

pub(crate) struct Series {
    pub name: &'static str,
    /// `c1` to `c4` (categorical), `o1` to `o5` (ordered), `up` or `down`.
    pub class: &'static str,
    pub values: Vec<Option<f64>>,
}

pub(crate) struct Chart<'a> {
    pub id: &'static str,
    pub title: &'static str,
    pub note: Html,
    pub months: &'a [String],
    pub form: Form,
    pub unit: Unit,
    pub series: Vec<Series>,
    /// The months before this index have no data for the reason given.
    pub gap: Option<(usize, &'static str)>,
}

fn nice(x: f64) -> f64 {
    let p = 10f64.powf(x.log10().floor());
    [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * p)
        .find(|s| *s >= x)
        .unwrap_or(10.0 * p)
}

/// Axis ends and step: three or four steps of a round size.
fn scale(lo: f64, hi: f64) -> (f64, f64, f64) {
    let span = if hi > lo { hi - lo } else { hi.abs().max(1.0) };
    let step = nice(span / 3.0);
    let (a, b) = ((lo / step).floor() * step, (hi / step).ceil() * step);
    (a, if b > a { b } else { a + step }, step)
}

fn compact(v: f64) -> String {
    let a = v.abs();
    if a >= 10_000.0 {
        format!("{:.0}K", v / 1000.0)
    } else if a >= 1000.0 {
        format!("{:.1}K", v / 1000.0).replace(".0K", "K")
    } else if a > 0.0 && a < 10.0 && v.fract() != 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.0}")
    }
}

fn tick(v: f64, unit: Unit) -> String {
    match unit {
        Unit::Count => compact(v),
        Unit::Share => format!("{:.0}%", v * 100.0),
        Unit::Months => format!("{} mo", compact(v)),
    }
}

/// The value as the tables and tooltips write it.
pub(crate) fn full(v: f64, unit: Unit) -> String {
    match unit {
        Unit::Count => thousands(v.round() as i64),
        Unit::Share if v != 0.0 && v.abs() < 0.1 => format!("{:.1}%", v * 100.0),
        Unit::Share => format!("{:.0}%", v * 100.0),
        Unit::Months => format!("{v:.1} months"),
    }
}

pub(crate) fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

fn latest(v: &[Option<f64>]) -> Option<(usize, f64)> {
    v.iter()
        .enumerate()
        .rev()
        .find_map(|(i, x)| x.map(|x| (i, x)))
}

impl Chart<'_> {
    fn band(&self) -> f64 {
        W / self.months.len().max(1) as f64
    }

    fn x(&self, i: usize) -> f64 {
        (i as f64 + 0.5) * self.band()
    }

    /// The top of each series in the plot: the value, or the running sum
    /// for a stack.
    fn tops(&self) -> Vec<Vec<Option<f64>>> {
        if self.form != Form::Stack {
            return self.series.iter().map(|s| s.values.clone()).collect();
        }
        let mut sum = vec![0.0; self.months.len()];
        self.series
            .iter()
            .map(|s| {
                s.values
                    .iter()
                    .zip(sum.iter_mut())
                    .map(|(v, acc)| {
                        *acc += v.unwrap_or(0.0);
                        v.map(|_| *acc)
                    })
                    .collect()
            })
            .collect()
    }

    pub(crate) fn render(&self) -> Html {
        if self.months.is_empty() {
            return html!(
                "<figure class=\"chart\" id=\"{id}\"><figcaption><h3 class=\"chart-title\">{title}</h3>\
                 <p class=\"chart-note\">No complete month yet.</p></figcaption></figure>",
                id = self.id,
                title = self.title,
            );
        }
        let tops = self.tops();
        let all = tops.iter().flatten().flatten().copied();
        let (lo, hi) = all.fold((0f64, 0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
        let (lo, hi, step) = scale(lo, hi);
        let y = |v: f64| BOTTOM - (v - lo) / (hi - lo) * (BOTTOM - TOP);
        let mut svg = Html::new();

        if let Some((end, why)) = self.gap.filter(|(end, _)| *end > 0) {
            let w = end.min(self.months.len()) as f64 * self.band();
            svg.push(&html!(
                "<rect class=\"chart-gap\" x=\"0\" y=\"{TOP}\" width=\"{w:.1}\" height=\"{h:.1}\"/>\
                 <text class=\"chart-gap-label\" x=\"{cx:.1}\" y=\"{ty:.1}\" text-anchor=\"middle\">{why}</text>",
                h = BOTTOM - TOP,
                cx = w / 2.0,
                ty = TOP + 14.0,
            ));
        }

        let mut t = lo;
        while t <= hi + step / 2.0 {
            let ty = y(t);
            svg.push(&html!(
                "<line class=\"{c}\" x1=\"0\" x2=\"{W}\" y1=\"{ty:.1}\" y2=\"{ty:.1}\"/>\
                 <text class=\"chart-tick\" x=\"0\" y=\"{ly:.1}\">{l}</text>",
                c = if t == 0.0 { "chart-base" } else { "chart-grid" },
                ly = ty - 4.0,
                l = tick(t, self.unit),
            ));
            t += step;
        }

        match self.form {
            Form::Bars => {
                let w = (self.band() * 0.7).clamp(1.0, 24.0);
                for (s, vals) in self.series.iter().zip(&tops) {
                    for (i, v) in vals.iter().enumerate() {
                        let Some(v) = *v else { continue };
                        let (a, b) = (y(v.max(0.0)), y(v.min(0.0)));
                        svg.push(&html!(
                            "<rect class=\"bar {c}\" x=\"{x:.1}\" y=\"{a:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"{r:.1}\"/>",
                            c = s.class,
                            x = self.x(i) - w / 2.0,
                            h = (b - a).max(0.5),
                            r = (w / 4.0).min(2.0),
                        ));
                    }
                }
            }
            Form::Stack => {
                let mut below: Vec<f64> = vec![lo.max(0.0); self.months.len()];
                for (s, vals) in self.series.iter().zip(&tops) {
                    let top: Vec<f64> = vals
                        .iter()
                        .zip(&below)
                        .map(|(v, b)| v.unwrap_or(*b))
                        .collect();
                    let mut d = String::new();
                    for (i, v) in top.iter().enumerate() {
                        d.push_str(&format!(
                            "{}{:.1},{:.1}",
                            if i == 0 { "M" } else { "L" },
                            self.x(i),
                            y(*v)
                        ));
                    }
                    for (i, v) in below.iter().enumerate().rev() {
                        d.push_str(&format!("L{:.1},{:.1}", self.x(i), y(*v)));
                    }
                    d.push('Z');
                    svg.push(&html!("<path class=\"stack {c}\" d=\"{d}\"/>", c = s.class));
                    below = top;
                }
                for vals in &tops[..tops.len().saturating_sub(1)] {
                    svg.push(&html!(
                        "<path class=\"chart-sep\" d=\"{d}\"/>",
                        d = line(vals, |i| self.x(i), y)
                    ));
                }
            }
            Form::Lines => {
                let single = self.series.len() == 1;
                for (s, vals) in self.series.iter().zip(&tops) {
                    let d = line(vals, |i| self.x(i), y);
                    if single {
                        svg.push(&html!(
                            "<path class=\"wash {c}\" d=\"{w}\"/>",
                            c = s.class,
                            w = wash(vals, |i| self.x(i), y, y(lo.max(0.0)))
                        ));
                    }
                    svg.push(&html!("<path class=\"line {c}\" d=\"{d}\"/>", c = s.class));
                }
                for (s, vals) in self.series.iter().zip(&tops) {
                    if let Some((i, v)) = latest(vals) {
                        svg.push(&html!(
                            "<circle class=\"end {c}\" cx=\"{cx:.1}\" cy=\"{cy:.1}\" r=\"4\"/>",
                            c = s.class,
                            cx = self.x(i),
                            cy = y(v)
                        ));
                    }
                }
            }
        }

        svg.push(&self.years());
        svg.push(&self.hits());

        let summary = self.summary();
        html!(
            "<figure class=\"chart\" id=\"{id}\"><figcaption><h3 class=\"chart-title\">{title}</h3>\
             <p class=\"chart-note\">{note}</p></figcaption>{legend}\
             <svg class=\"chart-svg\" viewBox=\"0 0 {W} {H}\" role=\"img\" aria-labelledby=\"{id}-t {id}-d\">\
             <title id=\"{id}-t\">{title}</title><desc id=\"{id}-d\">{summary}</desc>{svg}</svg>\
             <details class=\"chart-table\"><summary>Table</summary>{table}</details></figure>",
            id = self.id,
            title = self.title,
            note = &self.note,
            legend = self.legend(),
            table = self.table(),
        )
    }

    fn years(&self) -> Html {
        let spacing = 12.0 * self.band();
        let every = (48.0 / spacing).ceil().max(1.0) as i32;
        let mut out = Html::new();
        for (i, m) in self.months.iter().enumerate() {
            let Some(year) = m.strip_suffix("-01").and_then(|y| y.parse::<i32>().ok()) else {
                continue;
            };
            if year % every != 0 {
                continue;
            }
            let x = self.x(i);
            let (x, anchor) = if x < 18.0 {
                (0.0, "start")
            } else if x > W - 18.0 {
                (W, "end")
            } else {
                (x, "middle")
            };
            out.push(&html!(
                "<line class=\"chart-grid\" x1=\"{x0:.1}\" x2=\"{x0:.1}\" y1=\"{BOTTOM}\" y2=\"{y2:.1}\"/>\
                 <text class=\"chart-tick\" x=\"{x:.1}\" y=\"{ty:.1}\" text-anchor=\"{anchor}\">{year}</text>",
                x0 = self.x(i),
                y2 = BOTTOM + 4.0,
                ty = H - 6.0,
            ));
        }
        out
    }

    fn hits(&self) -> Html {
        let band = self.band();
        let rows = self.months.iter().enumerate().map(|(i, m)| {
            let values = join(
                self.series.iter().filter_map(|s| {
                    s.values[i].map(|v| html!("{}: {}", s.name, full(v, self.unit)))
                }),
                "; ",
            );
            html!(
                "<rect x=\"{x:.1}\" y=\"{TOP}\" width=\"{band:.1}\" height=\"{h:.1}\"><title>{m}: {values}</title></rect>",
                x = i as f64 * band,
                h = BOTTOM - TOP,
            )
        });
        html!("<g class=\"chart-hit\">{}</g>", join(rows, ""))
    }

    fn legend(&self) -> Html {
        let key = if self.form == Form::Lines {
            "key-line"
        } else {
            "key-box"
        };
        let items = self.series.iter().map(|s| {
            let value = match latest(&s.values) {
                Some((i, v)) if i + 1 == self.months.len() => {
                    html!(" <b>{}</b>", full(v, self.unit))
                }
                Some((i, v)) if self.form != Form::Bars => {
                    html!(" <b>{}</b> ({})", full(v, self.unit), self.months[i])
                }
                _ => Html::new(),
            };
            html!(
                "<li><span class=\"key {key} {c}\"></span>{name}{value}</li>",
                c = s.class,
                name = s.name
            )
        });
        html!("<ul class=\"chart-legend\">{}</ul>", join(items, ""))
    }

    fn summary(&self) -> String {
        let (Some(first), Some(last)) = (self.months.first(), self.months.last()) else {
            return String::new();
        };
        let parts: Vec<String> = self
            .series
            .iter()
            .filter_map(|s| {
                latest(&s.values).map(|(i, v)| {
                    format!("{} {} in {}", s.name, full(v, self.unit), self.months[i])
                })
            })
            .collect();
        format!("From {first} to {last}. Latest: {}.", parts.join(", "))
    }

    fn table(&self) -> Html {
        let head: Html = self
            .series
            .iter()
            .map(|s| html!("<th>{}</th>", s.name))
            .collect();
        let rows: Html = (0..self.months.len())
            .rev()
            .map(|i| {
                let cells: Html = self
                    .series
                    .iter()
                    .map(|s| {
                        html!(
                            "<td>{}</td>",
                            s.values[i].map_or_else(|| "–".to_string(), |v| full(v, self.unit))
                        )
                    })
                    .collect();
                html!("<tr><td>{}</td>{cells}</tr>", self.months[i])
            })
            .collect();
        html!(
            "<div class=\"markdown-body\"><table class=\"numbers\"><thead><tr><th>month</th>{head}</tr></thead>\
             <tbody>{rows}</tbody></table></div>"
        )
    }
}

/// A path through the values, broken where one is missing.
fn line(vals: &[Option<f64>], x: impl Fn(usize) -> f64, y: impl Fn(f64) -> f64) -> String {
    let mut d = String::new();
    let mut pen = false;
    for (i, v) in vals.iter().enumerate() {
        match v {
            Some(v) => {
                d.push_str(&format!(
                    "{}{:.1},{:.1}",
                    if pen { "L" } else { "M" },
                    x(i),
                    y(*v)
                ));
                pen = true;
            }
            None => pen = false,
        }
    }
    d
}

/// The area between each run of values and `base`.
fn wash(
    vals: &[Option<f64>],
    x: impl Fn(usize) -> f64,
    y: impl Fn(f64) -> f64,
    base: f64,
) -> String {
    let mut d = String::new();
    let mut run: Vec<(f64, f64)> = Vec::new();
    let close = |run: &mut Vec<(f64, f64)>, d: &mut String| {
        if let (Some(a), Some(b)) = (run.first(), run.last()) {
            d.push_str(&format!("M{:.1},{base:.1}", a.0));
            for (px, py) in run.iter() {
                d.push_str(&format!("L{px:.1},{py:.1}"));
            }
            d.push_str(&format!("L{:.1},{base:.1}Z", b.0));
        }
        run.clear();
    };
    for (i, v) in vals.iter().enumerate() {
        match v {
            Some(v) => run.push((x(i), y(*v))),
            None => close(&mut run, &mut d),
        }
    }
    close(&mut run, &mut d);
    d
}

/// One row of a dot plot: a label, a note such as the count, and one value
/// per key, each a share.
pub(crate) struct DotRow {
    pub label: String,
    pub note: String,
    pub values: Vec<f64>,
}

/// Shares on one 0 to 100% track per row, one dot per key.
pub(crate) fn dots(
    id: &'static str,
    title: &'static str,
    note: Html,
    keys: &[(&'static str, &'static str)],
    rows: &[DotRow],
) -> Html {
    let legend: Html = keys
        .iter()
        .map(|(name, c)| html!("<li><span class=\"key key-dot {c}\"></span>{name}</li>"))
        .collect();
    let body: Html = rows
        .iter()
        .map(|r| {
            let marks: Html = r
                .values
                .iter()
                .zip(keys)
                .map(|(v, (name, c))| {
                    html!(
                        "<span class=\"dot {c}\" style=\"left:{p:.1}%\" title=\"{name}: {v}\"></span>",
                        p = v * 100.0,
                        v = full(*v, Unit::Share)
                    )
                })
                .collect();
            let text = join(r.values.iter().map(|v| Html::text(&full(*v, Unit::Share))), " · ");
            html!(
                "<li class=\"dots-row\"><div class=\"dots-label\">{label} <span class=\"color-fg-muted\">{n}</span></div>\
                 <div class=\"dots-track\">{marks}</div><div class=\"dots-value\">{text}</div></li>",
                label = &r.label,
                n = &r.note
            )
        })
        .collect();
    let head: Html = keys
        .iter()
        .map(|(name, _)| html!("<th>{name}</th>"))
        .collect();
    let trs: Html = rows
        .iter()
        .map(|r| {
            let cells: Html = r
                .values
                .iter()
                .map(|v| html!("<td>{}</td>", full(*v, Unit::Share)))
                .collect();
            html!("<tr><td>{}</td><td>{}</td>{cells}</tr>", r.label, r.note)
        })
        .collect();
    html!(
        "<figure class=\"chart chart-wide\" id=\"{id}\"><figcaption><h3 class=\"chart-title\">{title}</h3>\
         <p class=\"chart-note\">{note}</p></figcaption><ul class=\"chart-legend\">{legend}</ul>\
         <ul class=\"dots\" aria-label=\"{title}\"><li class=\"dots-row\" aria-hidden=\"true\"><div class=\"dots-label\"></div>\
         <div class=\"dots-track dots-scale\"><span style=\"left:0%\">0%</span><span style=\"left:50%\">50%</span>\
         <span style=\"left:100%\">100%</span></div><div class=\"dots-value\"></div></li>{body}</ul>\
         <details class=\"chart-table\"><summary>Table</summary><div class=\"markdown-body\"><table class=\"numbers\">\
         <thead><tr><th></th><th></th>{head}</tr></thead><tbody>{trs}</tbody></table></div></details></figure>"
    )
}

/// One row of a bar table: a label, the value that sets the bar, and the
/// other columns.
pub(crate) struct BarRow {
    pub label: String,
    pub value: f64,
    /// The text at the tip of the bar.
    pub tip: Html,
    pub cells: Vec<Html>,
}

/// A table whose second column draws the value of each row as a bar.
/// `head` names the label column, the bar column and then the others.
pub(crate) fn bars(
    id: &'static str,
    title: &'static str,
    note: Html,
    head: &[&'static str],
    rows: &[BarRow],
) -> Html {
    let max = rows
        .iter()
        .map(|r| r.value)
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    let th: Html = head
        .iter()
        .enumerate()
        .map(|(i, h)| match i {
            0 => html!("<th>{h}</th>"),
            1 => html!("<th class=\"hbar-col\">{h}</th>"),
            _ => html!("<th class=\"num\">{h}</th>"),
        })
        .collect();
    let body: Html = rows
        .iter()
        .map(|r| {
            let cells: Html = r
                .cells
                .iter()
                .map(|c| html!("<td class=\"num\">{c}</td>"))
                .collect();
            html!(
                "<tr><th scope=\"row\">{label}</th><td class=\"hbar-col\"><span class=\"hbar-fill\"><span class=\"hbar c1\" style=\"width:{p:.1}%\"></span><span class=\"hbar-tip\">{tip}</span></span></td>{cells}</tr>",
                tip = &r.tip,
                label = &r.label,
                p = r.value / max * 80.0,
            )
        })
        .collect();
    html!(
        "<figure class=\"chart chart-wide\" id=\"{id}\"><figcaption><h3 class=\"chart-title\">{title}</h3>\
         <p class=\"chart-note\">{note}</p></figcaption>\
         <div class=\"hbars-box\"><table class=\"hbars\"><thead><tr>{th}</tr></thead><tbody>{body}</tbody></table></div></figure>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axes_take_round_steps() {
        assert_eq!(scale(0.0, 12_589.0), (0.0, 15_000.0, 5000.0));
        assert_eq!(scale(0.0, 0.62), (0.0, 0.75, 0.25));
        assert_eq!(scale(-628.0, 1200.0), (-1000.0, 2000.0, 1000.0));
        assert_eq!(compact(12_500.0), "12K");
        assert_eq!(compact(2500.0), "2.5K");
        assert_eq!(compact(2000.0), "2K");
        assert_eq!(full(12_589.0, Unit::Count), "12,589");
        assert_eq!(full(0.031, Unit::Share), "3.1%");
    }

    #[test]
    fn a_missing_value_breaks_the_line() {
        let d = line(
            &[Some(1.0), None, Some(2.0), Some(3.0)],
            |i| i as f64,
            |v| v,
        );
        assert_eq!(d, "M0.0,1.0M2.0,2.0L3.0,3.0");
    }

    #[test]
    fn a_stack_sums_and_text_is_escaped() {
        let months = vec!["2026-01".to_string(), "2026-02".to_string()];
        let c = Chart {
            id: "t",
            title: "Open <now>",
            note: Html::new(),
            months: &months,
            form: Form::Stack,
            unit: Unit::Count,
            series: vec![
                Series {
                    name: "new",
                    class: "o1",
                    values: vec![Some(1.0), Some(2.0)],
                },
                Series {
                    name: "old",
                    class: "o2",
                    values: vec![Some(3.0), None],
                },
            ],
            gap: None,
        };
        assert_eq!(c.tops()[1], vec![Some(4.0), None]);
        let h = c.render();
        assert!(h.contains("Open &lt;now&gt;"));
        assert!(h.contains("<title>2026-01: new: 1; old: 3</title>"));
    }

    #[test]
    fn a_chart_without_values_draws_no_nan() {
        assert_eq!(scale(0.0, 0.0), (0.0, 0.5, 0.5));
        let none = vec!["2026-01".to_string()];
        for months in [&none, &Vec::new()] {
            for form in [Form::Stack, Form::Lines, Form::Bars] {
                let c = Chart {
                    id: "t",
                    title: "t",
                    note: Html::new(),
                    months,
                    form,
                    unit: Unit::Count,
                    series: vec![Series {
                        name: "a",
                        class: "o1",
                        values: vec![None; months.len()],
                    }],
                    gap: None,
                };
                let h = c.render().to_string();
                assert!(!h.contains("NaN") && !h.contains("d=\"Z\""), "{h}");
            }
        }
    }
}
