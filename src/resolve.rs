//! Finding rule files and deciding which rules govern a path.

use crate::git::Git;
use crate::rules::{self, Parsed, Rule};
use anyhow::Result;
use globset::GlobBuilder;

pub const FILE_NAME: &str = "UNDERCURRENT.md";

pub struct RuleFile {
    /// Path of the rule file from the repository root.
    pub path: String,
    /// Directory the rules govern; empty for the repository root.
    pub dir: String,
    pub parsed: Parsed,
}

pub enum Source<'a> {
    WorkTree,
    Commit(&'a str),
}

fn is_rule_file(path: &str) -> bool {
    path.rsplit('/').next() == Some(FILE_NAME)
}

pub fn load(git: &Git, source: &Source) -> Result<Vec<RuleFile>> {
    let mut paths: Vec<String> = match source {
        Source::WorkTree => git.all_files()?,
        Source::Commit(reference) => git.tree_files(reference)?,
    };
    paths.retain(|path| is_rule_file(path));
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        let text = match source {
            Source::WorkTree => std::fs::read_to_string(git.root().join(&path))?,
            Source::Commit(reference) => git.show(reference, &path)?,
        };
        let dir = path
            .strip_suffix(FILE_NAME)
            .unwrap_or("")
            .trim_end_matches('/')
            .to_string();
        files.push(RuleFile {
            path,
            dir,
            parsed: rules::parse(&text),
        });
    }
    Ok(files)
}

impl RuleFile {
    /// `path` relative to this rule file's directory, if it lies inside it.
    pub fn relative<'a>(&self, path: &'a str) -> Option<&'a str> {
        if self.dir.is_empty() {
            return Some(path);
        }
        path.strip_prefix(self.dir.as_str())?.strip_prefix('/')
    }

    pub fn applies(&self, rule: &Rule, path: &str) -> bool {
        let Some(relative) = self.relative(path) else {
            return false;
        };
        let Some(pattern) = &rule.only else {
            return true;
        };
        // Like .gitignore: a pattern without a slash matches the file name alone.
        let subject = if pattern.contains('/') {
            relative
        } else {
            relative.rsplit('/').next().unwrap_or(relative)
        };
        GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .is_ok_and(|glob| glob.compile_matcher().is_match(subject))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &str) -> RuleFile {
        RuleFile {
            path: String::new(),
            dir: dir.to_string(),
            parsed: Parsed::default(),
        }
    }

    fn rule(only: Option<&str>) -> Rule {
        Rule {
            only: only.map(String::from),
            ..Rule::default()
        }
    }

    #[test]
    fn scope_follows_the_directory() {
        assert!(file("").applies(&rule(None), "src/a.rs"));
        assert!(file("src").applies(&rule(None), "src/deep/a.rs"));
        assert!(!file("src").applies(&rule(None), "srcs/a.rs"));
        assert!(!file("src").applies(&rule(None), "docs/a.md"));
    }

    #[test]
    fn only_filters_by_name_or_relative_path() {
        assert!(file("").applies(&rule(Some("*.rs")), "src/deep/a.rs"));
        assert!(!file("").applies(&rule(Some("*.rs")), "README.md"));
        assert!(file("src").applies(&rule(Some("handlers/*.rs")), "src/handlers/a.rs"));
        assert!(!file("src").applies(&rule(Some("handlers/*.rs")), "src/handlers/x/a.rs"));
    }
}
