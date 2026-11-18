use crate::check::Outcome;
use crate::run::Finding;
use std::fmt::Write as _;
use std::io::Write as _;

fn location(finding: &Finding) -> String {
    match (&finding.path, finding.line) {
        (Some(path), Some(line)) => format!("{path}:{line}"),
        (Some(path), None) => path.clone(),
        _ => finding.rule_file.clone(),
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| format!("{prefix}{line}\n"))
        .collect()
}

fn summary(outcome: &Outcome) -> String {
    let mut line = format!(
        "{} violation(s) across {} file(s); {} command check(s) run.",
        outcome.findings.len(),
        outcome.files,
        outcome.commands_run
    );
    if let Some(semantic) = &outcome.semantic {
        let _ = write!(
            line,
            " {} semantic check(s): {} from cache, {} flagged for a second look, {} failed to run.",
            semantic.checks,
            semantic.cached,
            semantic.flagged,
            semantic.errors.len()
        );
    }
    if !outcome.unevaluated.is_empty() {
        let _ = write!(
            line,
            " {} semantic rule(s) applied but were NOT judged because {}.",
            outcome.unevaluated.len(),
            outcome.unevaluated_reason
        );
    }
    if outcome.bootstrapped {
        line.push_str(
            " The base branch has no rule files, so this change was judged by the rules it adds.",
        );
    }
    line
}

fn problems(outcome: &Outcome) -> Vec<String> {
    let Some(semantic) = &outcome.semantic else {
        return Vec::new();
    };
    let errors = semantic
        .errors
        .iter()
        .map(|error| format!("error: {error}"));
    errors
        .chain(semantic.notes.iter().map(|note| format!("note: {note}")))
        .collect()
}

pub fn text(outcome: &Outcome) -> String {
    let mut out = String::new();
    for finding in &outcome.findings {
        let _ = writeln!(out, "{}: {}", location(finding), finding.title);
        let _ = writeln!(out, "  rule `{}` in {}", finding.rule_id, finding.rule_file);
        out.push_str(&indent(&finding.prose, "  "));
        if !finding.detail.is_empty() {
            out.push_str(&indent(&finding.detail, "  | "));
        }
        out.push('\n');
    }
    for (rule_file, title) in &outcome.unevaluated {
        let _ = writeln!(out, "not judged: {title} ({rule_file})");
    }
    for problem in problems(outcome) {
        let _ = writeln!(out, "{problem}");
    }
    out.push_str(&summary(outcome));
    out.push('\n');
    out
}

fn escape(value: &str, property: bool) -> String {
    let mut out = value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A");
    if property {
        out = out.replace(':', "%3A").replace(',', "%2C");
    }
    out
}

pub fn annotations(outcome: &Outcome) -> String {
    let mut out = String::new();
    for finding in &outcome.findings {
        let mut properties = format!("title={}", escape(&finding.title, true));
        if let Some(path) = &finding.path {
            let _ = write!(properties, ",file={}", escape(path, true));
        }
        if let Some(line) = finding.line {
            let _ = write!(properties, ",line={line}");
        }
        let message = format!("{}\n\n{}", finding.prose, finding.detail);
        let _ = writeln!(
            out,
            "::error {properties}::{}",
            escape(message.trim(), false)
        );
    }
    if !outcome.unevaluated.is_empty() {
        let message = format!(
            "{} semantic rule(s) were not judged because {}.",
            outcome.unevaluated.len(),
            outcome.unevaluated_reason
        );
        let _ = writeln!(
            out,
            "::warning title=Undercurrent::{}",
            escape(&message, false)
        );
    }
    for problem in problems(outcome) {
        let level = if problem.starts_with("error") {
            "error"
        } else {
            "warning"
        };
        let _ = writeln!(
            out,
            "::{level} title=Undercurrent::{}",
            escape(&problem, false)
        );
    }
    out
}

fn markdown(outcome: &Outcome) -> String {
    let mut out = format!("## Undercurrent\n\n{}\n\n", summary(outcome));
    for finding in &outcome.findings {
        let _ = writeln!(out, "### `{}`: {}\n", location(finding), finding.title);
        let _ = writeln!(
            out,
            "{}\n\nRule `{}` in `{}`.\n",
            finding.prose, finding.rule_id, finding.rule_file
        );
        if !finding.detail.is_empty() {
            let _ = writeln!(out, "````\n{}\n````\n", finding.detail);
        }
    }
    out
}

pub fn write_step_summary(outcome: &Outcome) {
    let Some(path) = std::env::var_os("GITHUB_STEP_SUMMARY") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
    {
        let _ = file.write_all(markdown(outcome).as_bytes());
    }
}
