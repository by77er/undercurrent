//! Parsing of `UNDERCURRENT.md` rule files.
//!
//! A rule is a `## heading` followed by prose. An optional `sh` fenced block
//! makes it a command rule; without one the rule is semantic.

use crate::shell;

#[derive(Debug, Clone, PartialEq)]
pub struct Example {
    pub good: bool,
    pub lang: String,
    pub code: String,
}

#[derive(Debug, Clone, Default)]
pub struct Rule {
    pub id: String,
    pub title: String,
    pub line: usize,
    pub only: Option<String>,
    pub prose: String,
    pub commands: Vec<String>,
    pub examples: Vec<Example>,
}

impl Rule {
    pub fn command(&self) -> Option<&str> {
        self.commands.first().map(String::as_str)
    }

    pub fn is_semantic(&self) -> bool {
        self.commands.is_empty()
    }

    /// A command that mentions `$FILE` runs once per changed file; any other
    /// command runs once in the rule file's directory.
    pub fn per_file(&self) -> bool {
        self.command().is_some_and(shell::mentions_file)
    }
}

#[derive(Debug, Clone)]
pub struct Problem {
    pub line: usize,
    pub message: String,
    pub warning: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub context: String,
    pub rules: Vec<Rule>,
    pub problems: Vec<Problem>,
}

enum FenceKind {
    Command,
    Example { good: bool, lang: String },
    Other,
}

struct Fence {
    marker: char,
    len: usize,
    kind: FenceKind,
    open: String,
    line: usize,
    body: Vec<String>,
}

pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

fn fence_open(line: &str) -> Option<(char, usize, &str)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    (len >= 3).then(|| (marker, len, trimmed[len..].trim()))
}

fn classify(info: &str) -> FenceKind {
    let tokens: Vec<&str> = info.split_whitespace().collect();
    match tokens.as_slice() {
        ["sh"] => FenceKind::Command,
        [.., last] if *last == "good" || *last == "bad" => FenceKind::Example {
            good: *last == "good",
            lang: if tokens.len() > 1 {
                tokens[0].to_string()
            } else {
                String::new()
            },
        },
        _ => FenceKind::Other,
    }
}

/// A `key: value` line directly under a rule heading.
fn metadata(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    let key_ok = !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c == '-');
    (key_ok && value.starts_with(' ') && !value.trim().is_empty()).then(|| (key, value.trim()))
}

fn apply_metadata(
    rule: &mut Rule,
    key: &str,
    value: &str,
    line: usize,
    problems: &mut Vec<Problem>,
) {
    let mut error = |message: String| {
        problems.push(Problem {
            line,
            message,
            warning: false,
        })
    };
    match key {
        "only" => rule.only = Some(value.to_string()),
        "id" if slug(value) == value => rule.id = value.to_string(),
        "id" => error(format!(
            "`id: {value}` must be lowercase letters, digits and dashes"
        )),
        "severity" => error("remove `severity:`; every rule blocks".to_string()),
        "paths" => error(
            "remove `paths:`; a rule applies to its file's directory and below. \
             Move the rule to a deeper UNDERCURRENT.md, or use `only:` to filter by name"
                .to_string(),
        ),
        "kind" => error("remove `kind:`; an `sh` block makes a rule a command rule".to_string()),
        _ => error(format!(
            "unknown key `{key}:`; the only keys are `only:` and `id:`. \
             If this line is prose, put a blank line between it and the heading"
        )),
    }
}

fn push_text(target: &mut String, line: &str) {
    target.push_str(line);
    target.push('\n');
}

fn finish_fence(fence: Fence, close: &str, current: &mut Option<Rule>, out: &mut Parsed) {
    let code = fence.body.join("\n");
    match (fence.kind, current.as_mut()) {
        (FenceKind::Command, Some(rule)) => rule.commands.push(code),
        (FenceKind::Example { good, lang }, Some(rule)) => {
            rule.examples.push(Example { good, lang, code })
        }
        (FenceKind::Other, Some(rule)) => {
            push_text(&mut rule.prose, &fence.open);
            push_text(&mut rule.prose, &code);
            push_text(&mut rule.prose, close);
        }
        (FenceKind::Other, None) => {
            push_text(&mut out.context, &fence.open);
            push_text(&mut out.context, &code);
            push_text(&mut out.context, close);
        }
        (_, None) => out.problems.push(Problem {
            line: fence.line,
            message: "this block is outside any rule; put it under a `## heading`".to_string(),
            warning: false,
        }),
    }
}

pub fn parse(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let mut current: Option<Rule> = None;
    let mut fence: Option<Fence> = None;
    let mut meta_open = false;

    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        if let Some(mut open) = fence.take() {
            let closes = fence_open(line).is_some_and(|(m, len, info)| {
                m == open.marker && len >= open.len && info.is_empty()
            });
            if closes {
                finish_fence(open, line, &mut current, &mut out);
            } else {
                open.body.push(line.to_string());
                fence = Some(open);
            }
            continue;
        }
        if let Some((marker, len, info)) = fence_open(line) {
            meta_open = false;
            let kind = classify(info);
            fence = Some(Fence {
                marker,
                len,
                kind,
                open: line.to_string(),
                line: number,
                body: vec![],
            });
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            out.rules.extend(current.take());
            let title = title.trim().to_string();
            current = Some(Rule {
                id: slug(&title),
                title,
                line: number,
                ..Rule::default()
            });
            meta_open = true;
            continue;
        }
        if meta_open {
            if let (Some((key, value)), Some(rule)) = (metadata(line), current.as_mut()) {
                apply_metadata(rule, key, value, number, &mut out.problems);
                continue;
            }
            meta_open = false;
        }
        match current.as_mut() {
            Some(rule) => push_text(&mut rule.prose, line),
            None => push_text(&mut out.context, line),
        }
    }

    if let Some(open) = fence {
        out.problems.push(Problem {
            line: open.line,
            message: "this code block is never closed".to_string(),
            warning: false,
        });
    }
    out.rules.extend(current);
    out.context = out.context.trim().to_string();
    for rule in &mut out.rules {
        rule.prose = rule.prose.trim().to_string();
    }
    out
}
