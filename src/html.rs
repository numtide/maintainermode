//! HTML that is known to be safe. `html!` escapes each argument unless it
//! is already [`Html`], so text from GitHub or from a person cannot become
//! markup by accident.

use std::fmt;

pub(crate) use html_macro::html;

#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub(crate) struct Html(String);

impl Html {
    pub(crate) const fn new() -> Self {
        Html(String::new())
    }

    /// Plain text, escaped.
    pub(crate) fn text(s: &str) -> Self {
        let mut out = String::with_capacity(s.len());
        escape_into(&mut out, s);
        Html(out)
    }

    #[doc(hidden)]
    pub(crate) fn __escaped(s: String) -> Self {
        Html(s)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn push(&mut self, h: &Html) {
        self.0.push_str(&h.0);
    }

    pub(crate) fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for Html {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// The bridge for the modules that still build HTML as `str` or `String`:
// these conversions, `Deref`, `push_str` and `fmt::Write` trust their
// input. When the last module uses `html!`, only `From<&'static str>` stays.
impl From<&str> for Html {
    fn from(s: &str) -> Self {
        Html(s.to_string())
    }
}

impl From<String> for Html {
    fn from(s: String) -> Self {
        Html(s)
    }
}

impl From<&String> for Html {
    fn from(s: &String) -> Self {
        Html(s.clone())
    }
}

impl std::ops::Deref for Html {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl FromIterator<String> for Html {
    fn from_iter<I: IntoIterator<Item = String>>(iter: I) -> Self {
        Html(iter.into_iter().collect())
    }
}

impl fmt::Write for Html {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.push_str(s);
        Ok(())
    }
}

impl Html {
    pub(crate) fn push_str(&mut self, s: &str) {
        self.0.push_str(s);
    }
}

impl From<&Html> for Html {
    fn from(h: &Html) -> Self {
        h.clone()
    }
}

impl std::borrow::Borrow<str> for Html {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Html {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromIterator<Html> for Html {
    fn from_iter<I: IntoIterator<Item = Html>>(iter: I) -> Self {
        let mut out = Html::new();
        for h in iter {
            out.0.push_str(&h.0);
        }
        out
    }
}

impl FromIterator<Html> for String {
    fn from_iter<I: IntoIterator<Item = Html>>(iter: I) -> Self {
        iter.into_iter().map(|h| h.0).collect()
    }
}

impl axum::response::IntoResponse for Html {
    fn into_response(self) -> axum::response::Response {
        axum::response::Html(self.0).into_response()
    }
}

pub(crate) fn escape_into(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// The text of a fragment of our own HTML: tags out, entities decoded.
/// Not for HTML from elsewhere: it knows only the entities we write.
pub(crate) fn to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// How a value goes into `html!`: text is escaped, [`Html`] and numbers are
/// written as they are.
pub(crate) trait Render {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

impl Render for Html {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Render for str {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(self.len());
        escape_into(&mut out, self);
        f.write_str(&out)
    }
}

impl Render for String {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().render(f)
    }
}

impl Render for std::borrow::Cow<'_, str> {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_ref().render(f)
    }
}

impl Render for char {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.encode_utf8(&mut [0; 4]).render(f)
    }
}

impl<T: Render + ?Sized> Render for &T {
    fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).render(f)
    }
}

macro_rules! number {
    ($($t:ty),*) => {$(
        impl Render for $t {
            fn render(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(self, f)
            }
        }
    )*};
}
number!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize, f32, f64);

pub(crate) struct Esc<'a, T: Render + ?Sized>(pub &'a T);

impl<T: Render + ?Sized> fmt::Display for Esc<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.render(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_escaped_and_html_is_not() {
        let name = "<b>&\"x\"";
        let bold = html!("<b>{}</b>", "ok");
        let h = html!("<p title=\"{name}\">{bold} {n:.1}</p>", n = 2.25);
        assert_eq!(
            h.as_str(),
            "<p title=\"&lt;b&gt;&amp;&quot;x&quot;\"><b>ok</b> 2.2</p>"
        );
    }

    #[test]
    fn to_text_drops_tags_and_decodes_once() {
        assert_eq!(
            to_text("Use <code>hash = \"sha256-…\"</code> &amp;lt; <a href=\"/x\">a &gt; b</a>"),
            "Use hash = \"sha256-…\" &lt; a > b"
        );
    }

    #[test]
    fn braces_and_unicode_escapes_are_not_captures() {
        let h = html!("{{x}}\u{a0}{}", 1);
        assert_eq!(h.as_str(), "{x}\u{a0}1");
    }

    #[test]
    fn a_named_argument_is_escaped_once() {
        let v = String::from("a<b");
        assert_eq!(html!("{v}", v = v).as_str(), "a&lt;b");
        assert_eq!(html!(r#"<i a="{v}">"#).as_str(), "<i a=\"a&lt;b\">");
    }
}
