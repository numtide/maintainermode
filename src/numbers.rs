//! The Numbers tab of `/state`: the monthly numbers of `metrics` as charts,
//! seen from five sides: whether nixpkgs keeps up, how long a pull request
//! waits, who does the work, what arrives and where it waits.

use crate::chart::{self, BarRow, Chart, DotRow, Form, Series, Unit};
use crate::html::{Html, html, join};
use crate::metrics::{self, Month};
use crate::web::{App, cell, num, pct, stat, table};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Range {
    Two,
    Five,
    All,
}

impl Range {
    const ALL: [Range; 3] = [Range::Two, Range::Five, Range::All];

    pub(crate) fn parse(s: Option<&str>) -> Self {
        match s {
            Some("2y") => Range::Two,
            Some("all") => Range::All,
            _ => Range::Five,
        }
    }

    fn months(self) -> Option<usize> {
        match self {
            Range::Two => Some(24),
            Range::Five => Some(60),
            Range::All => None,
        }
    }

    fn href(self) -> &'static str {
        match self {
            Range::Two => "/state?range=2y",
            Range::Five => "/state",
            Range::All => "/state?range=all",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Range::Two => "2 years",
            Range::Five => "5 years",
            Range::All => "all",
        }
    }
}

fn series(name: &'static str, class: &'static str, values: Vec<Option<f64>>) -> Series {
    Series {
        name,
        class,
        values,
    }
}

/// Where data starts for a reason: the months before `from` get a band.
fn gap(ms: &[&Month], from: &str, why: &'static str) -> Option<(usize, &'static str)> {
    let i = ms
        .iter()
        .position(|m| m.month.as_str() >= from)
        .unwrap_or(ms.len());
    (i > 0).then_some((i, why))
}

fn month(ym: (i32, u32)) -> String {
    format!("{}-{:02}", ym.0, ym.1)
}

fn section(id: &str, title: &str, lead: &str, charts: &[Html]) -> Html {
    html!(
        "<h2 class=\"f3 mt-5 mb-1\" id=\"{id}\">{title}</h2><p class=\"color-fg-muted mb-3\">{lead}</p>\
         <div class=\"charts\">{}</div>",
        join(charts.iter().cloned(), "")
    )
}

pub(crate) fn numbers(app: &App, range: Range) -> Html {
    let r = app.metrics.read().unwrap().clone();
    let mut body = Html::new();
    let Some(at) = r.generated_at else {
        body.push(&Html::from(
            "<div class=\"flash\">The first sync has not finished yet.</div>",
        ));
        body.push(&crate::web::packages_section(app));
        return body;
    };
    let current = at.format("%Y-%m").to_string();
    let done: Vec<&Month> = r.months.iter().filter(|m| m.month != current).collect();
    let ms: &[&Month] = match range.months() {
        Some(n) => &done[done.len().saturating_sub(n)..],
        None => &done,
    };
    let months: Vec<String> = ms.iter().map(|m| m.month.clone()).collect();
    let col = |f: &dyn Fn(&Month) -> Option<f64>| -> Vec<Option<f64>> {
        ms.iter().map(|m| f(m)).collect()
    };
    let count = |n: usize| Some(n as f64);
    let reviews_from = month(metrics::REVIEWS_FROM);
    let newcomers_from = r.newcomers_from.clone().unwrap_or_default();
    let chart = |id, title, note: Html, form, unit, series: Vec<Series>, gap| {
        Chart {
            id,
            title,
            note,
            months: &months,
            form,
            unit,
            series,
            gap,
        }
        .render()
    };

    let n = &r.now;
    body.push(&html!(
        "<p class=\"f6 color-fg-muted\">{} pull requests, opened since {}. Computed {}. {}</p>",
        chart::thousands(r.pull_requests as i64),
        r.earliest_created
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default(),
        crate::web::ago(at),
        if app.history_complete() {
            "The archive holds all of history."
        } else {
            "The archive still walks back through history, so older months are incomplete."
        }
    ));
    body.push(&html!(
        "<div class=\"d-flex flex-wrap gap-2 mb-4\">{}{}{}{}{}</div>",
        stat(
            n.open,
            "open",
            html!(
                "{} drafts, {} from bots, median age {} days",
                n.drafts,
                n.by_bots,
                num(n.age_days_p50)
            )
        ),
        stat(
            n.never_reviewed,
            "never reviewed",
            "by people, not drafts, no review by anyone else"
        ),
        stat(
            n.security,
            "open security fixes",
            html!("median age {} days", num(n.security_age_days_p50))
        ),
        stat(
            n.first_time,
            "open from first-time contributors",
            "by GitHub's association or the nixpkgs label"
        ),
        stat(
            n.ai,
            "open with disclosed AI use",
            "Assisted-by trailer or llm-assisted label; a floor"
        ),
    ));

    let links = join(
        Range::ALL.iter().map(|&x| {
            let cur = if x == range {
                Html::from(" aria-current=\"page\"")
            } else {
                Html::new()
            };
            html!("<a href=\"{}\"{cur}>{}</a>", x.href(), x.label())
        }),
        "",
    );
    body.push(&html!(
        "<nav class=\"range-nav\" aria-label=\"Months shown\"><span class=\"color-fg-muted\">Months shown:</span> {links}\
         <span class=\"color-fg-muted\">complete months, {} to {}</span></nav>",
        months.first().cloned().unwrap_or_default(),
        months.last().cloned().unwrap_or_default(),
    ));

    let closed = |m: &Month| m.merged + m.closed_unmerged;
    let net = |m: &Month| m.opened as f64 - closed(m) as f64;
    let backlog_months: Vec<Option<f64>> = (0..ms.len())
        .map(|i| {
            let from = done
                .iter()
                .position(|m| m.month == ms[i].month)
                .unwrap_or(0);
            let window = &done[from.saturating_sub(2)..=from];
            let per = window.iter().map(|m| closed(m)).sum::<usize>() as f64 / window.len() as f64;
            (per > 0.0).then(|| ms[i].open_at_end as f64 / per)
        })
        .collect();
    body.push(&section(
        "flow",
        "Does nixpkgs keep up?",
        "Pull requests that arrive against pull requests that close, and what is left open.",
        &[
            chart(
                "opened-closed",
                "Opened and closed each month",
                Html::from("Closed counts merged and closed without a merge."),
                Form::Lines,
                Unit::Count,
                vec![
                    series("opened", "c1", col(&|m| count(m.opened))),
                    series("closed", "c2", col(&|m| count(closed(m)))),
                ],
                None,
            ),
            chart(
                "net",
                "Change of the open backlog",
                Html::from("Opened minus closed. A drop is often a clean-up of old pull requests."),
                Form::Bars,
                Unit::Count,
                vec![
                    series("grew", "up", col(&|m| Some(net(m)).filter(|v| *v > 0.0))),
                    series("shrank", "down", col(&|m| Some(net(m)).filter(|v| *v <= 0.0))),
                ],
                None,
            ),
            chart(
                "backlog-age",
                "Open at the end of each month, by age",
                Html::from("Drafts count: nixpkgs keeps no record of when a draft became ready."),
                Form::Stack,
                Unit::Count,
                vec![
                    series("under 7 days", "o1", col(&|m| count(m.open_at_end_age.under_week))),
                    series("7 to 30 days", "o2", col(&|m| count(m.open_at_end_age.under_month))),
                    series("30 to 90 days", "o3", col(&|m| count(m.open_at_end_age.under_quarter))),
                    series("90 to 365 days", "o4", col(&|m| count(m.open_at_end_age.under_year))),
                    series("a year or more", "o5", col(&|m| count(m.open_at_end_age.year_or_more))),
                ],
                None,
            ),
            chart(
                "backlog-months",
                "The open backlog in months of work",
                Html::from(
                    "Open at the end of the month, divided by the pull requests closed per month over the last three months.",
                ),
                Form::Lines,
                Unit::Months,
                vec![series("months of work", "c1", backlog_months)],
                None,
            ),
        ],
    ));

    let kinds_window = match (&r.kinds_from, &r.kinds_to) {
        (Some(a), Some(b)) => format!("{a} to {b}"),
        _ => String::new(),
    };
    let kind_rows: Vec<DotRow> = r
        .kinds
        .iter()
        .map(|k| DotRow {
            label: k.kind.to_string(),
            note: chart::thousands(k.pull_requests as i64),
            values: vec![k.merged_within_7d, k.merged_within_90d, k.closed_within_90d],
        })
        .collect();
    body.push(&section(
        "wait",
        "How long does a pull request wait?",
        "Pull requests by people; bots are left out. Each month counts the pull requests opened in it, \
         the open ones too, so a recent month does not look faster than it is.",
        &[
            chart(
                "first-review",
                "A first review within a week",
                html!("A review by someone other than the author. GitHub has reviews since 2016-09."),
                Form::Lines,
                Unit::Share,
                vec![series("reviewed within 7 days", "c1", col(&|m| m.reviewed_within_7d))],
                gap(ms, &reviews_from, "no GitHub reviews"),
            ),
            chart(
                "merged-within",
                "Merged within 7, 30 and 90 days",
                Html::from("A month shows once its days have passed for the whole month."),
                Form::Lines,
                Unit::Share,
                vec![
                    series("7 days", "o2", col(&|m| m.merged_within_7d)),
                    series("30 days", "o3", col(&|m| m.merged_within_30d)),
                    series("90 days", "o5", col(&|m| m.merged_within_90d)),
                ],
                None,
            ),
            chart(
                "not-merged",
                "What does not merge",
                Html::from("Closed without a merge within 90 days, and still open a year later."),
                Form::Lines,
                Unit::Share,
                vec![
                    series("closed within 90 days", "c2", col(&|m| m.closed_within_90d)),
                    series("open after a year", "c4", col(&|m| m.open_after_365d)),
                ],
                None,
            ),
            chart(
                "newcomers",
                "New authors: the first pull request merged",
                Html::from("Of the people whose first nixpkgs pull request opened that month: merged within 90 days."),
                Form::Lines,
                Unit::Share,
                vec![series("merged within 90 days", "c1", col(&|m| m.new_authors_merged_within_90d))],
                gap(ms, &newcomers_from, "the data starts in 2012"),
            ),
            chart::dots(
                "kinds",
                "What changes the wait",
                html!(
                    "Pull requests opened from {kinds_window}, so each one had 90 days. The number is how many. \
                     A pull request can be of more than one kind."
                ),
                &[
                    ("merged within 7 days", "c1"),
                    ("merged within 90 days", "c3"),
                    ("closed without a merge within 90 days", "c2"),
                ],
                &kind_rows,
            ),
        ],
    ));

    let review_count = |v: usize, m: &Month| (m.month >= reviews_from).then_some(v as f64);
    body.push(&section(
        "people",
        "Who does the work?",
        "Counts of people; no number names a person. Bots are left out, so a merge through the merge bot counts for nobody.",
        &[
            chart(
                "people-month",
                "People each month",
                Html::from("Distinct people who opened, reviewed or merged a pull request, and first-time authors."),
                Form::Lines,
                Unit::Count,
                vec![
                    series("authors", "c1", col(&|m| count(m.authors))),
                    series("reviewers", "c2", col(&|m| review_count(m.reviewers, m))),
                    series("mergers", "c3", col(&|m| count(m.mergers))),
                    series(
                        "new authors",
                        "c4",
                        col(&|m| (m.month >= newcomers_from).then_some(m.new_authors as f64)),
                    ),
                ],
                None,
            ),
            chart(
                "merges-per-merger",
                "Merges per person who merged",
                Html::from("The median and the 90th percentile of the people who merged that month."),
                Form::Lines,
                Unit::Count,
                vec![
                    series("median", "c1", col(&|m| m.merges_per_merger_p50)),
                    series("90th percentile", "c2", col(&|m| m.merges_per_merger_p90)),
                ],
                None,
            ),
            chart(
                "half",
                "People who did half of the work",
                Html::from(
                    "The fewest people who together did half of the merges, or of the reviews, in the 12 months to that month.",
                ),
                Form::Lines,
                Unit::Count,
                vec![
                    series("merges", "c1", col(&|m| m.merge_people_for_half_12m.map(|v| v as f64))),
                    series("reviews", "c2", col(&|m| m.review_people_for_half_12m.map(|v| v as f64))),
                ],
                None,
            ),
            chart(
                "come-back",
                "New authors who come back",
                Html::from(
                    "Of the people whose first pull request opened that month: another pull request within a year, \
                     by whether the first one merged within 90 days.",
                ),
                Form::Lines,
                Unit::Share,
                vec![
                    series("first merged", "c1", col(&|m| m.new_authors_back_within_365d_merged)),
                    series("first not merged", "c2", col(&|m| m.new_authors_back_within_365d_unmerged)),
                ],
                gap(ms, &newcomers_from, "the data starts in 2012"),
            ),
        ],
    ));

    body.push(&section(
        "inflow",
        "What arrives?",
        "Kinds come from the nixpkgs labels and titles, whose use changed over the years.",
        &[
            chart(
                "by-whom",
                "Opened, by who opened them",
                html!(
                    "The update bot is {} (nixpkgs-update). Most other bot pull requests are backports.",
                    crate::model::UPDATE_BOT
                ),
                Form::Stack,
                Unit::Count,
                vec![
                    series("people", "c1", col(&|m| count(m.opened - m.opened_by_bots))),
                    series("update bot", "c2", col(&|m| count(m.opened_by_update_bot))),
                    series(
                        "other bots",
                        "c3",
                        col(&|m| count(m.opened_by_bots - m.opened_by_update_bot)),
                    ),
                ],
                None,
            ),
            chart(
                "version-updates",
                "Version updates",
                Html::from("Pull requests with a title like <code>name: 1.0 -&gt; 1.1</code>, by people and by the update bot."),
                Form::Lines,
                Unit::Count,
                vec![
                    series("by people", "c1", col(&|m| count(m.opened_bumps_by_people))),
                    series("update bot", "c2", col(&|m| count(m.opened_by_update_bot))),
                ],
                None,
            ),
            chart(
                "kinds-opened",
                "New packages, security fixes and backports",
                Html::from(
                    "A new package has the label or a title like <code>name: init at 1.0</code>; a security fix has the label; \
                     a backport targets a <code>release-*</code> branch.",
                ),
                Form::Lines,
                Unit::Count,
                vec![
                    series("new packages", "c1", col(&|m| count(m.opened_new_package))),
                    series("security fixes", "c2", col(&|m| count(m.opened_security))),
                    series("backports", "c3", col(&|m| count(m.opened_backport))),
                ],
                None,
            ),
            chart(
                "ai",
                "Disclosed AI use",
                Html::from(
                    "An <code>Assisted-by:</code> trailer or the <code>llm-assisted</code> label, and other forms of attribution. \
                     Nobody can see undisclosed use, so each count is a minimum.",
                ),
                Form::Lines,
                Unit::Count,
                vec![
                    series("disclosed", "c1", col(&|m| count(m.opened_ai))),
                    series("other forms", "c2", col(&|m| count(m.opened_ai_other_form))),
                ],
                None,
            ),
        ],
    ));

    let mut areas: Vec<(f64, &metrics::AreaRow)> = r
        .areas
        .iter()
        .filter(|a| a.merged_365d > 0)
        .map(|a| (a.open as f64 / (a.merged_365d as f64 / 12.0), a))
        .collect();
    areas.sort_by(|a, b| b.0.total_cmp(&a.0));
    let area_rows: Vec<BarRow> = areas
        .iter()
        .map(|(months, a)| BarRow {
            label: a.area.clone(),
            value: *months,
            tip: html!("{months:.1}"),
            cells: vec![
                Html::text(&chart::thousands(a.open as i64)),
                Html::text(&pct(
                    (a.open > 0).then(|| a.never_reviewed as f64 / a.open as f64)
                )),
                Html::text(&pct(a.merged_within_30d)),
            ],
        })
        .collect();
    body.push(&section(
        "areas",
        "Where do pull requests wait?",
        "Each part of the tree, from the files a pull request changes. One pull request can count in several parts.",
        &[chart::bars(
            "by-area",
            "Open pull requests in months of merges",
            Html::from(
                "Open now, divided by the merges per month of the last 365 days. Never reviewed: open pull requests by people, not drafts, \
                 with no review, as a share of all open ones. Merged in 30 days is for pull requests by people \
                 opened from 395 to 30 days ago.",
            ),
            &[
                "part of the tree",
                "months of merges",
                "open",
                "never reviewed",
                "merged in 30 days",
            ],
            &area_rows,
        )],
    ));

    let c = &r.concentration_365d;
    let load = |l: &metrics::Load, what: &'static str| {
        vec![
            Html::from(what),
            cell(l.total),
            cell(l.people),
            cell(l.people_for_half),
            cell(l.people_for_80),
        ]
    };
    let tools: Vec<Vec<Html>> = r
        .ai_tools_365d
        .iter()
        .map(|(t, n)| vec![cell(t), cell(n)])
        .collect();
    let rows: Vec<Vec<Html>> = ms
        .iter()
        .rev()
        .map(|m| {
            vec![
                cell(&m.month),
                cell(m.opened),
                cell(m.opened_by_bots),
                cell(m.opened_first_time),
                html!("{} / {}", m.opened_ai, m.opened_ai_other_form),
                cell(m.merged),
                cell(m.closed_unmerged),
                cell(m.open_at_end),
                cell(m.reviews),
                cell(m.reviewers),
                cell(m.mergers),
                cell(pct(m.reviewed_within_7d)),
                cell(pct(m.merged_within_30d)),
                cell(pct(m.merge_rate_first_time)),
            ]
        })
        .collect();
    body.push(&html!(
        "<h2 class=\"f3 mt-5 mb-2\" id=\"more\">More numbers</h2>\
         <div class=\"d-flex flex-wrap gap-3 mb-4\"><div class=\"flex-1\"><h3 class=\"f4 mb-2\">How many people do the work, last 365 days</h3>{}</div>\
         <div><h3 class=\"f4 mb-2\">Disclosed AI tools, last 365 days</h3><details class=\"chart-table\"><summary>{}</summary>{}</details></div></div>\
         <details class=\"chart-table mb-4\"><summary>Every month, {} to {}</summary>{}</details>",
        table(
            &["", "total", "people", "people for half", "people for 80%"],
            &[
                load(&c.merges, "merges"),
                load(&c.reviews, "reviews"),
                load(&c.authored, "authored, people"),
            ]
        ),
        match tools.len() {
            1 => "1 tool".to_string(),
            n => format!("{n} tools"),
        },
        table(&["tool", "pull requests"], &tools),
        months.first().cloned().unwrap_or_default(),
        months.last().cloned().unwrap_or_default(),
        table(
            &[
                "month",
                "opened",
                "bots",
                "first-time",
                "AI / other form",
                "merged",
                "closed",
                "open at end",
                "reviews",
                "reviewers",
                "mergers",
                "reviewed in 7 days",
                "merged in 30 days",
                "first-time merge rate",
            ],
            &rows
        ),
    ));
    body.push(&crate::web::packages_section(app));
    body
}
