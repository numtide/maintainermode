//! `html!` is `format!` that wraps every argument, inline captures too, in
//! `crate::html::Esc`, and returns `crate::html::Html`. A `macro_rules!`
//! macro cannot see inline captures such as `{name}`, so they would skip
//! the escaping; that is why this is a proc macro.

use proc_macro::{Delimiter, Group, Ident, Punct, Spacing, Span, TokenStream, TokenTree};
use std::str::FromStr;

#[proc_macro]
pub fn html(input: TokenStream) -> TokenStream {
    match expand(input) {
        Ok(ts) => ts,
        Err(msg) => TokenStream::from_str(&format!("::std::compile_error!({msg:?})")).unwrap(),
    }
}

fn expand(input: TokenStream) -> Result<TokenStream, String> {
    let mut parts = split(input).into_iter();
    let lit = match parts.next().map(|p| p.into_iter().collect::<Vec<_>>()) {
        Some(v) if v.len() == 1 => match &v[0] {
            TokenTree::Literal(l) => l.clone(),
            _ => return Err("html!: the first argument must be a string literal".into()),
        },
        _ => return Err("html!: the first argument must be a string literal".into()),
    };
    let mut args = TokenStream::new();
    let mut named = Vec::new();
    for part in parts {
        let tokens: Vec<TokenTree> = part.into_iter().collect();
        if tokens.is_empty() {
            continue;
        }
        args.extend([comma()]);
        match (&tokens.first(), &tokens.get(1)) {
            (Some(TokenTree::Ident(name)), Some(TokenTree::Punct(eq)))
                if eq.as_char() == '=' && eq.spacing() == Spacing::Alone =>
            {
                named.push(name.to_string());
                args.extend(tokens[..2].iter().cloned());
                args.extend(esc(tokens[2..].iter().cloned().collect()));
            }
            _ => args.extend(esc(tokens.into_iter().collect())),
        }
    }
    for name in captures(&lit.to_string()) {
        if named.contains(&name) {
            continue;
        }
        named.push(name.clone());
        let id = Ident::new(&name, Span::call_site());
        args.extend([comma(), TokenTree::Ident(id.clone())]);
        args.extend([TokenTree::Punct(Punct::new('=', Spacing::Alone))]);
        args.extend(esc(TokenTree::Ident(id).into()));
    }
    let mut format = TokenStream::from_str("::std::format!").unwrap();
    let mut inner = TokenStream::from(TokenTree::Literal(lit));
    inner.extend(args);
    format.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, inner))]);
    let mut out = TokenStream::from_str("crate::html::Html::__escaped").unwrap();
    out.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, format))]);
    Ok(out)
}

fn comma() -> TokenTree {
    TokenTree::Punct(Punct::new(',', Spacing::Alone))
}

/// `crate::html::Esc(&(expr))`
fn esc(expr: TokenStream) -> TokenStream {
    let mut inner = TokenStream::from(TokenTree::Punct(Punct::new('&', Spacing::Alone)));
    inner.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, expr))]);
    let mut out = TokenStream::from_str("crate::html::Esc").unwrap();
    out.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, inner))]);
    out
}

/// Splits on the commas at the top level. Commas inside a group are not
/// seen here; a closure `|a, b|` or a turbofish with two types would be
/// split wrongly, so bind those to a name first.
fn split(input: TokenStream) -> Vec<TokenStream> {
    let mut parts = vec![TokenStream::new()];
    for tt in input {
        match &tt {
            TokenTree::Punct(p) if p.as_char() == ',' => parts.push(TokenStream::new()),
            _ => parts.last_mut().unwrap().extend([tt]),
        }
    }
    parts
}

/// The names of the inline captures in the source text of a string literal.
fn captures(src: &str) -> Vec<String> {
    let raw = src.starts_with('r');
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if !raw => {
                i += if chars.get(i + 1) == Some(&'u') {
                    skip_unicode(&chars, i)
                } else {
                    2
                }
            }
            '{' if chars.get(i + 1) == Some(&'{') => i += 2,
            '{' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j] != '}' && chars[j] != ':' {
                    j += 1;
                }
                let name: String = chars[start..j].iter().collect();
                let ident = name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                if ident && !out.contains(&name) {
                    out.push(name);
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    out
}

fn skip_unicode(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j < chars.len() && chars[j] != '}' {
        j += 1;
    }
    j + 1 - i
}
