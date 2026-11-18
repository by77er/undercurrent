use crate::rules::{Parsed, Problem, Rule};
use crate::shell;
use std::collections::HashSet;

const THRESHOLD_WORDS: &[&str] = &[
    "at most",
    "at least",
    "more than",
    "fewer than",
    "less than",
    "under ",
    "over ",
    "exceed",
    "maximum",
    "minimum",
    "no longer than",
];

fn looks_countable(rule: &Rule) -> bool {
    let prose = format!("{} {}", rule.title, rule.prose).to_lowercase();
    prose.chars().any(|c| c.is_ascii_digit()) && THRESHOLD_WORDS.iter().any(|w| prose.contains(w))
}

fn check_rule(rule: &Rule, seen: &mut HashSet<String>, problems: &mut Vec<Problem>) {
    let mut report = |message: String, warning: bool| {
        problems.push(Problem {
            line: rule.line,
            message,
            warning,
        });
    };
    if rule.id.is_empty() {
        report(
            "the heading needs at least one letter or digit to form an id".to_string(),
            false,
        );
    } else if !seen.insert(rule.id.clone()) {
        report(
            format!(
                "two rules in this file share the id `{}`; rename one or add `id:`",
                rule.id
            ),
            false,
        );
    }
    if rule.prose.is_empty() {
        report(
            "add prose under the heading; it is the message shown when the rule fails".to_string(),
            false,
        );
    }
    if rule.commands.len() > 1 {
        report(
            "a rule takes one `sh` block; merge these or split the rule".to_string(),
            false,
        );
    }
    if let Some(pattern) = &rule.only {
        if let Err(err) = globset::Glob::new(pattern) {
            report(
                format!("`only: {pattern}` is not a valid glob: {err}"),
                false,
            );
        }
    }
    for command in &rule.commands {
        for message in shell::problems(command) {
            report(message, false);
        }
    }
    if rule.is_semantic() && looks_countable(rule) {
        report(
            "this rule sets a numeric limit but has no `sh` block, so a model would judge it; \
             models count unreliably, so add a command that measures it"
                .to_string(),
            true,
        );
    }
}

pub fn problems(parsed: &Parsed) -> Vec<Problem> {
    let mut all = parsed.problems.clone();
    let mut seen = HashSet::new();
    for rule in &parsed.rules {
        check_rule(rule, &mut seen, &mut all);
    }
    all.sort_by_key(|p| p.line);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse;

    fn messages(text: &str) -> Vec<(bool, String)> {
        problems(&parse(text))
            .into_iter()
            .map(|p| (p.warning, p.message))
            .collect()
    }

    #[test]
    fn minimal_rule_is_valid() {
        assert!(messages("## Keep handlers thin\n\nHandlers only translate HTTP.\n").is_empty());
    }

    #[test]
    fn removed_keys_and_empty_rules_are_errors() {
        let found = messages("## A rule\nseverity: block\n\n## A rule\n\nSome prose.\n");
        assert_eq!(found.iter().filter(|(warning, _)| !warning).count(), 3);
    }

    #[test]
    fn numeric_prose_rule_warns() {
        let found = messages("## Small files\n\nFiles stay under 400 lines.\n");
        assert_eq!(found.len(), 1);
        assert!(found[0].0);
    }
}
