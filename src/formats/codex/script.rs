//! Codex Desktop's code mode: the model calls its tools from a JavaScript
//! script run by a `custom_tool_call` named `exec`, e.g.
//! `const r = await tools.exec_command({cmd: "ls"}); text(r.output);`.

use serde_json::Value;

/// Every `tools.<name>(` the script calls, in order of first use.
pub(super) fn tools(script: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find("tools.") {
        let after = &rest[at + "tools.".len()..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let follows = after[name.len()..].trim_start().starts_with('(');
        let standalone = at == 0 || !rest[..at].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        if !name.is_empty() && follows && standalone && !out.contains(&name) {
            out.push(name.clone());
        }
        rest = &after[name.len()..];
    }
    out
}

/// The argument of every `tools.<name>({...})` call in the script whose
/// argument is an object literal that reads as JSON once its keys are quoted.
pub(super) fn literal_args(script: &str, name: &str) -> Vec<Value> {
    let needle = format!("tools.{name}(");
    let mut out = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find(&needle) {
        rest = &rest[at + needle.len()..];
        let body = rest.trim_start();
        if body.starts_with('{')
            && let Some(literal) = balanced(body)
            && let Some(value) = js_to_json(literal)
        {
            out.push(value);
        }
    }
    out
}

/// The `{...}` at the start of `text`, braces inside strings ignored.
fn balanced(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&text[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// A JavaScript object literal as JSON: bare keys quoted, single-quoted
/// strings double-quoted, trailing commas dropped. Anything that is not a
/// plain literal (variables, template substitutions, calls) gives `None`.
fn js_to_json(src: &str) -> Option<Value> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + 16);
    let mut i = 0;
    let last_significant = |out: &str| out.trim_end().chars().last();
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' | '\'' | '`' => {
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != c {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        s.push(chars[i]);
                        s.push(chars[i + 1]);
                        i += 2;
                        continue;
                    }
                    if c == '`' && chars[i] == '$' && chars.get(i + 1) == Some(&'{') {
                        return None;
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                // Re-escape for a double-quoted JSON string.
                let s = if c == '"' {
                    s
                } else {
                    s.replace("\\'", "'").replace("\\`", "`").replace('"', "\\\"").replace('\n', "\\n")
                };
                out.push('"');
                out.push_str(&s);
                out.push('"');
                i += 1;
            }
            c if c.is_alphabetic() || c == '_' || c == '$' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                    i += 1;
                }
                let ident: String = chars[start..i].iter().collect();
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let is_key = chars.get(j) == Some(&':') && matches!(last_significant(&out), Some('{' | ','));
                if is_key {
                    out.push_str(&format!("\"{ident}\""));
                } else {
                    out.push_str(&ident);
                }
            }
            '}' | ']' => {
                let trimmed = out.trim_end().len();
                out.truncate(trimmed);
                if out.ends_with(',') {
                    out.pop();
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    serde_json::from_str(&out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_tools_and_reads_plan_literals() {
        let script = r#"const p = await tools.update_plan({plan:[
  {step:"Inventory 'docs'",status:"in_progress"},
  {step:'Audit "code"', status:"pending"},
]}); const r = await tools.exec_command({cmd:`ls ${x}`}); text(p);"#;
        assert_eq!(tools(script), ["update_plan", "exec_command"]);
        let plans = literal_args(script, "update_plan");
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0]["plan"][0]["step"], "Inventory 'docs'");
        assert_eq!(plans[0]["plan"][1]["step"], "Audit \"code\"");
        assert_eq!(plans[0]["plan"][1]["status"], "pending");
        assert!(literal_args(script, "exec_command").is_empty(), "a template substitution is not a literal");
    }
}
