/// Only allowlisted fields reach this boundary. No raw JSON or system prompts are forwarded.
#[derive(Debug, Clone, Default)]
pub struct Redactor { home: Option<String> }
impl Redactor {
    pub fn new(home: Option<String>) -> Self { Self { home: home.filter(|s| !s.is_empty() && s != "/") } }
    pub fn clean(&self, text: &str) -> String {
        let text = if let Some(home) = &self.home { text.replace(home, "~") } else { text.to_owned() };
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            if let Some(prefix) = ["/Users/", "/home/"].into_iter().find(|p| rest.starts_with(p)) {
                let end = prefix.len() + rest[prefix.len()..].find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '\'' | '"' | '`')).unwrap_or(rest.len() - prefix.len());
                if end > prefix.len() { out.push('~'); i += end; continue; }
            }
            if rest.starts_with("Bearer ") {
                out.push_str("Bearer ");
                let start = "Bearer ".len() + rest["Bearer ".len()..].len() - rest["Bearer ".len()..].trim_start().len();
                let end = start + rest[start..].find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>')).unwrap_or(rest.len()-start);
                if end > start { out.push_str("[已隐藏]"); }
                i += end; continue;
            }
            let key_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
            if let Some(prefix) = ["sk-", "ghp_", "xoxb-"].into_iter().find(|p| rest.starts_with(p)) {
                let n = rest[prefix.len()..].chars().take_while(|c| key_char(*c)).count();
                if n >= 20 && !text[..i].chars().next_back().is_some_and(char::is_alphanumeric) {
                    out.push_str("[已隐藏]"); i += prefix.len()+n; continue;
                }
            }
            // Email addresses, including addresses embedded in prose or JSON output.
            let email_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '%' | '@');
            let n = if i == 0 || !text[..i].chars().next_back().is_some_and(email_char) {
                rest.chars().take_while(|c| email_char(*c)).count()
            } else { 0 };
            if n > 0 && rest[..n].split_once('@').is_some_and(|(a,b)| !a.is_empty() && b.contains('.')) {
                out.push_str("[已隐藏]"); i += n; continue;
            }
            let c = rest.chars().next().expect("nonempty UTF-8 suffix");
            out.push(c); i += c.len_utf8();
        }
        out
    }
    pub fn text(&self, text: &str) -> (String, bool) { truncate_chars(&self.clean(text), 4000, false) }
    pub fn tail(&self, text: &str) -> (String, bool) { truncate_chars(&self.clean(text), 4000, true) }
    pub fn diff(&self, text: &str) -> (String, bool) {
        let text = self.clean(text);
        let mut end = text.len().min(64 * 1024);
        while !text.is_char_boundary(end) { end -= 1; }
        let truncated = end < text.len();
        (text[..end].to_owned(), truncated)
    }
}
fn truncate_chars(text: &str, max: usize, tail: bool) -> (String, bool) {
    if tail {
        if let Some((start, _)) = text.char_indices().rev().nth(max - 1) {
            return (text[start..].to_owned(), start > 0);
        }
    } else if let Some((end, _)) = text.char_indices().nth(max) {
        return (text[..end].to_owned(), true);
    }
    (text.to_owned(), false)
}
