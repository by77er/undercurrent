use crate::git::Git;
use crate::resolve::{self, RuleFile, Source};
use crate::run::{self, Finding, Job};
use crate::semantic;
use crate::validate;
use anyhow::{bail, Result};

pub struct Options {
    pub base: Option<String>,
    pub all: bool,
    pub rules_from_base: bool,
    pub skip_commands: bool,
    pub skip_semantic: bool,
    pub require_semantic: bool,
    pub separate_instructions: bool,
}

#[derive(Default)]
pub struct Outcome {
    pub findings: Vec<Finding>,
    pub files: usize,
    pub commands_run: usize,
    pub unevaluated: Vec<(String, String)>,
    pub unevaluated_reason: &'static str,
    pub bootstrapped: bool,
    pub semantic: Option<semantic::Report>,
    pub commands_complete: bool,
    pub semantic_complete: bool,
}

const SAFE_PATH_PROSE: &str = "File paths may contain only letters, digits and `. _ - / @ +`. \
Paths are handed to rule commands, so anything else is refused rather than risk a shell \
reading it as code. Rename the file.";

const STAND_ALONE_PROSE: &str = "A pull request changes rule files or other files, never both. Rules are \
loaded from the base branch, so a rule change does not apply to the pull request that makes it, and mixing \
the two can hide a weakened rule among code. Move the rule changes into a pull request of their own and \
merge it first.";
const MAX_LISTED: usize = 10;

fn is_guidance(path: &str, with_instructions: bool) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let rule_side = resolve::is_rule_file(path) || path == semantic::THRESHOLDS_FILE;
    let in_skills = path.starts_with(".claude/skills/") || path.contains("/.claude/skills/");
    let instruction = matches!(name, "CLAUDE.md" | "AGENTS.md" | "SKILL.md") || in_skills;
    rule_side || (with_instructions && instruction)
}

fn listed(paths: &[&String]) -> String {
    let mut shown: Vec<&str> = paths
        .iter()
        .take(MAX_LISTED)
        .map(|path| path.as_str())
        .collect();
    if paths.len() > MAX_LISTED {
        shown.push("...");
    }
    format!("{} ({})", shown.join(", "), paths.len())
}

fn mixed_change(touched: &[String], with_instructions: bool) -> Option<Finding> {
    let (guidance, other): (Vec<&String>, Vec<&String>) = touched
        .iter()
        .partition(|path| is_guidance(path, with_instructions));
    if guidance.is_empty() || other.is_empty() {
        return None;
    }
    Some(Finding {
        path: None,
        line: None,
        rule_id: "rule-changes-stand-alone".to_string(),
        title: "Rule changes stand alone".to_string(),
        rule_file: "built in".to_string(),
        prose: STAND_ALONE_PROSE.to_string(),
        detail: format!(
            "rule and instruction files: {}\nother files: {}",
            listed(&guidance),
            listed(&other)
        ),
        semantic: false,
    })
}

fn is_safe_path(path: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || "._-/@+".contains(c);
    !path.is_empty()
        && path.chars().all(allowed)
        && !path.split('/').any(|part| part.starts_with('-'))
}

fn unsafe_path_finding(path: &str) -> Finding {
    Finding {
        path: None,
        line: None,
        rule_id: "safe-file-paths".to_string(),
        title: "File paths are safe to pass to a shell".to_string(),
        rule_file: "built in".to_string(),
        prose: SAFE_PATH_PROSE.to_string(),
        detail: format!("offending path: {path:?}"),
        semantic: false,
    }
}

fn ensure_valid(rule_files: &[RuleFile]) -> Result<()> {
    let mut errors = Vec::new();
    for file in rule_files {
        for problem in validate::problems(&file.parsed)
            .iter()
            .filter(|p| !p.warning)
        {
            errors.push(format!(
                "{}:{}: {}",
                file.path, problem.line, problem.message
            ));
        }
    }
    if !errors.is_empty() {
        bail!(
            "rule files have errors, so nothing was checked:\n{}",
            errors.join("\n")
        );
    }
    Ok(())
}

fn semantic_rules(rule_files: &[RuleFile], files: &[String]) -> Vec<(String, String)> {
    let mut applicable = Vec::new();
    for file in rule_files {
        for rule in file.parsed.rules.iter().filter(|rule| rule.is_semantic()) {
            if files.iter().any(|path| file.applies(rule, path)) {
                applicable.push((file.path.clone(), rule.title.clone()));
            }
        }
    }
    applicable
}

fn command_jobs<'a>(rule_files: &'a [RuleFile], files: &'a [String]) -> Vec<Job<'a>> {
    let mut jobs = Vec::new();
    for file in rule_files {
        for rule in file.parsed.rules.iter().filter(|rule| !rule.is_semantic()) {
            let mut matching = files
                .iter()
                .filter(|path| file.applies(rule, path))
                .peekable();
            if matching.peek().is_none() {
                continue;
            }
            if rule.per_file() {
                jobs.extend(matching.map(|path| Job {
                    file,
                    rule,
                    target: Some(path.as_str()),
                }));
            } else {
                jobs.push(Job {
                    file,
                    rule,
                    target: None,
                });
            }
        }
    }
    jobs
}

pub fn check(options: &Options) -> Result<Outcome> {
    let git = Git::discover()?;
    let base = if options.all && !options.rules_from_base {
        None
    } else {
        Some(git.resolve_base(options.base.as_deref())?)
    };
    let merge_base = match (&base, options.all) {
        (Some(base), false) => Some(git.merge_base(base)?),
        _ => None,
    };
    let mut files = match &merge_base {
        Some(merge_base) => git.changed(merge_base)?,
        None => git.all_files()?,
    };
    let generated = git.generated(&files)?;
    files.retain(|file| !generated.contains(file));

    let source = match (&base, options.rules_from_base) {
        (Some(base), true) => Source::Commit(base),
        _ => Source::WorkTree,
    };
    let mut rule_files = resolve::load(&git, &source)?;
    let mut outcome = Outcome::default();
    if rule_files.is_empty() && matches!(source, Source::Commit(_)) {
        rule_files = resolve::load(&git, &Source::WorkTree)?;
        outcome.bootstrapped = !rule_files.is_empty();
    }
    ensure_valid(&rule_files)?;

    let (files, hostile): (Vec<String>, Vec<String>) =
        files.into_iter().partition(|f| is_safe_path(f));
    outcome.findings = hostile
        .iter()
        .map(|path| unsafe_path_finding(path))
        .collect();
    outcome.files = files.len();
    if let (Some(base), Some(merge_base)) = (&base, &merge_base) {
        let base_has_rules = git
            .tree_files(base)?
            .iter()
            .any(|path| resolve::is_rule_file(path));
        if base_has_rules {
            let touched = git.touched(merge_base)?;
            outcome
                .findings
                .extend(mixed_change(&touched, options.separate_instructions));
        }
    }

    outcome.commands_complete = !options.skip_commands;
    if !options.skip_commands {
        let jobs = command_jobs(&rule_files, &files);
        outcome.commands_run = jobs.len();
        outcome.findings.extend(run::run_all(&jobs, git.root()));
    }

    let applicable = semantic_rules(&rule_files, &files);
    if applicable.is_empty() {
        outcome.semantic_complete = !options.skip_semantic;
        return Ok(outcome);
    }
    if options.skip_semantic {
        outcome.unevaluated = applicable;
        outcome.unevaluated_reason =
            "--skip-semantic was passed; run again without it to have these rules judged";
        return Ok(outcome);
    }
    let Some(engines) = semantic::Engines::from_env() else {
        if options.require_semantic {
            bail!(
                "{} semantic rule(s) apply but no model key is set; set TYPESAFE_API_KEY or ANTHROPIC_API_KEY, or drop --require-semantic",
                applicable.len()
            );
        }
        outcome.unevaluated = applicable;
        outcome.unevaluated_reason = "neither TYPESAFE_API_KEY nor ANTHROPIC_API_KEY is set; set one of them to have these rules judged";
        return Ok(outcome);
    };
    let fitted = match (&source, outcome.bootstrapped) {
        (Source::Commit(reference), false) => git
            .show(reference, semantic::THRESHOLDS_FILE)
            .unwrap_or_default(),
        _ => {
            std::fs::read_to_string(git.root().join(semantic::THRESHOLDS_FILE)).unwrap_or_default()
        }
    };
    let report = semantic::run(
        &engines,
        &git,
        merge_base.as_deref(),
        &files,
        &rule_files,
        &fitted,
    )?;
    outcome.findings.extend(report.findings.iter().cloned());
    outcome.semantic_complete = report.errors.is_empty();
    outcome.semantic = Some(report);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::{is_safe_path, mixed_change};

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|path| path.to_string()).collect()
    }

    #[test]
    fn rule_files_and_other_files_do_not_mix() {
        assert!(mixed_change(&paths(&["src/UNDERCURRENT.md", "src/main.rs"]), false).is_some());
        assert!(mixed_change(
            &paths(&[".undercurrent/thresholds.json", "action.yml"]),
            false
        )
        .is_some());
        assert!(mixed_change(
            &paths(&["UNDERCURRENT.md", ".undercurrent/thresholds.json"]),
            false
        )
        .is_none());
        assert!(mixed_change(&paths(&["src/main.rs", "src/check.rs"]), false).is_none());
    }

    #[test]
    fn instruction_files_join_the_rule_side_only_on_request() {
        let change = paths(&["CLAUDE.md", ".claude/skills/review/SKILL.md", "src/main.rs"]);
        assert!(mixed_change(&change, false).is_none());
        assert!(mixed_change(&change, true).is_some());
        assert!(mixed_change(&paths(&["CLAUDE.md", "docs/UNDERCURRENT.md"]), true).is_none());
        assert!(mixed_change(&paths(&["CLAUDE.md", "docs/UNDERCURRENT.md"]), false).is_some());
    }

    #[test]
    fn hostile_paths_are_refused() {
        assert!(is_safe_path("src/handlers/user_v2.rs"));
        assert!(is_safe_path("packages/@scope/a+b.ts"));
        assert!(!is_safe_path("src/$(curl evil).ts"));
        assert!(!is_safe_path("src/a b.ts"));
        assert!(!is_safe_path("src/-rf"));
        assert!(!is_safe_path("src/a\nb.ts"));
    }
}
