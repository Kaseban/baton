//! Session selection for `baton convert` when no input path is given.
//!
//! Two modes:
//!   - `--latest`: silently pick the most recently modified session.
//!   - interactive: list the newest sessions with a human-readable preview
//!     (first user message) so IDs like `a81e1457-…` are recognizable.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use crate::canonical::{Agent, SessionRef};
use crate::formats;

/// How many sessions the interactive picker shows (newest first).
const PICKER_LIMIT: usize = 15;
/// Max preview width in characters.
const PREVIEW_WIDTH: usize = 80;

/// Resolve the input session path when none was given on the command line.
pub fn resolve_input(from: Agent, latest: bool) -> anyhow::Result<PathBuf> {
    let mut refs = formats::list(from);
    anyhow::ensure!(
        !refs.is_empty(),
        "no {} sessions found on this machine",
        formats::display_name(from)
    );
    refs.sort_by_key(|r| std::cmp::Reverse(r.mtime));

    if latest {
        let r = &refs[0];
        eprintln!(
            "using latest {} session: {}  {}",
            from,
            fmt_time(r.mtime),
            preview(r)
        );
        eprintln!("  {}", r.path.display());
        return Ok(r.path.clone());
    }

    anyhow::ensure!(
        std::io::stdin().is_terminal(),
        "no input file given and not running in a terminal — pass a session path or use --latest"
    );

    let total = refs.len();
    refs.truncate(PICKER_LIMIT);
    eprintln!(
        "select a {} session to convert (newest first):\n",
        formats::display_name(from)
    );
    for (i, r) in refs.iter().enumerate() {
        eprintln!("{:>3}) {}  {}", i + 1, fmt_time(r.mtime), preview(r));
        eprintln!("     {}", r.path.display());
    }
    if total > refs.len() {
        eprintln!(
            "     … {} more (run `baton list --agent {}` to see all)",
            total - refs.len(),
            from
        );
    }
    eprintln!();

    loop {
        eprint!("session [1-{}] (q to cancel): ", refs.len());
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            anyhow::bail!("cancelled");
        }
        let line = line.trim();
        if line.eq_ignore_ascii_case("q") {
            anyhow::bail!("cancelled");
        }
        match line.parse::<usize>() {
            Ok(n) if (1..=refs.len()).contains(&n) => return Ok(refs[n - 1].path.clone()),
            _ => eprintln!("enter a number between 1 and {}", refs.len()),
        }
    }
}

fn fmt_time(mtime: i64) -> String {
    chrono::DateTime::from_timestamp_millis(mtime)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "????-??-?? ??:??".into())
}

/// One-line human preview of a session: its title if the lister provided one,
/// otherwise the first user message (parsed via the format codec — only runs
/// for the handful of sessions shown, so the extra reads stay cheap).
fn preview(r: &SessionRef) -> String {
    let text = if !r.title.is_empty() {
        r.title.clone()
    } else {
        match formats::read(r.agent, &r.path) {
            Ok(s) => {
                let first = s.first_user_text().unwrap_or("").trim().to_string();
                if !first.is_empty() {
                    first
                } else if !s.title.is_empty() {
                    s.title
                } else {
                    format!("({} messages, no user text)", s.message_count())
                }
            }
            Err(_) => "(could not read preview)".into(),
        }
    };
    truncate_one_line(&text, PREVIEW_WIDTH)
}

fn truncate_one_line(s: &str, width: usize) -> String {
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = one_line.chars().take(width).collect();
    if one_line.chars().count() > width {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_and_flattens() {
        assert_eq!(truncate_one_line("a\nb\n  c", 80), "a b c");
        let long = "x".repeat(100);
        let t = truncate_one_line(&long, 80);
        assert_eq!(t.chars().count(), 81);
        assert!(t.ends_with('…'));
    }
}
