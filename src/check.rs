use crate::git::Git;
use crate::resolve::{self, RuleFile, Source};
use crate::run::{self, Finding, Job};
use crate::validate;
use anyhow::{bail, Result};

pub struct Options {
    pub base: Option<String>,
    pub all: bool,
    pub rules_from_base: bool,
}

pub struct Outcome {
    pub findings: Vec<Finding>,
    pub files: usize,
    pub commands_run: usize,
    pub unevaluated: Vec<(String, String)>,
}

const SAFE_PATH_PROSE: &str = "File paths may contain only letters, digits and `. _ - / @ +`. \
Paths are handed to rule commands, so anything else is refused rather than risk a shell \
reading it as code. Rename the file.";

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

pub fn check(options: &Options) -> Result<Outcome> {
    let git = Git::discover()?;
    let base = if options.all && !options.rules_from_base {
        None
    } else {
        Some(git.resolve_base(options.base.as_deref())?)
    };
    let mut files = match (&base, options.all) {
        (Some(base), false) => git.changed(base)?,
        _ => git.all_files()?,
    };
    let generated = git.generated(&files)?;
    files.retain(|file| !generated.contains(file));

    let source = match (&base, options.rules_from_base) {
        (Some(base), true) => Source::Commit(base),
        _ => Source::WorkTree,
    };
    let rule_files = resolve::load(&git, &source)?;
    ensure_valid(&rule_files)?;

    let (files, hostile): (Vec<String>, Vec<String>) =
        files.into_iter().partition(|f| is_safe_path(f));
    let mut findings: Vec<Finding> = hostile
        .iter()
        .map(|path| unsafe_path_finding(path))
        .collect();

    let mut jobs = Vec::new();
    let mut unevaluated = Vec::new();
    for file in &rule_files {
        for rule in &file.parsed.rules {
            let mut matching = files
                .iter()
                .filter(|path| file.applies(rule, path))
                .peekable();
            if matching.peek().is_none() {
                continue;
            }
            if rule.is_semantic() {
                unevaluated.push((file.path.clone(), rule.title.clone()));
            } else if rule.per_file() {
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
    findings.extend(run::run_all(&jobs, git.root()));
    Ok(Outcome {
        findings,
        files: files.len(),
        commands_run: jobs.len(),
        unevaluated,
    })
}

#[cfg(test)]
mod tests {
    use super::is_safe_path;

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
