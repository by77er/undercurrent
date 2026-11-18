use crate::pool;
use crate::resolve::RuleFile;
use crate::rules::Rule;
use std::path::Path;
use std::process::Command;

const MAX_DETAIL_LINES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: Option<String>,
    pub line: Option<u32>,
    pub rule_id: String,
    pub title: String,
    pub rule_file: String,
    pub prose: String,
    pub detail: String,
}

pub struct Job<'a> {
    pub file: &'a RuleFile,
    pub rule: &'a Rule,
    pub target: Option<&'a str>,
}

impl Job<'_> {
    fn finding(&self, path: Option<String>, line: Option<u32>, detail: String) -> Finding {
        Finding {
            path,
            line,
            rule_id: self.rule.id.clone(),
            title: self.rule.title.clone(),
            rule_file: self.file.path.clone(),
            prose: self.rule.prose.clone(),
            detail,
        }
    }
}

fn anchor(line: &str, root: &Path, dir: &str) -> Option<(String, u32, String)> {
    let (path, rest) = line.split_once(':')?;
    let (number, rest) = rest.split_once(':')?;
    let number: u32 = number.trim().parse().ok()?;
    let message = match rest.split_once(':') {
        Some((column, tail)) if column.trim().parse::<u32>().is_ok() => tail,
        _ => rest,
    };
    let path = path.trim().trim_start_matches("./");
    if path.is_empty() || path.split('/').any(|part| part == "..") {
        return None;
    }
    let in_dir = if dir.is_empty() {
        path.to_string()
    } else {
        format!("{dir}/{path}")
    };
    let resolved = [in_dir, path.to_string()]
        .into_iter()
        .find(|p| root.join(p).is_file())?;
    Some((resolved, number, message.trim().to_string()))
}

fn clip(output: &str) -> String {
    let lines: Vec<&str> = output.trim().lines().collect();
    if lines.len() <= MAX_DETAIL_LINES {
        return lines.join("\n");
    }
    let hidden = lines.len() - MAX_DETAIL_LINES;
    format!(
        "{}\n... {hidden} more lines",
        lines[..MAX_DETAIL_LINES].join("\n")
    )
}

fn run_job(job: &Job, root: &Path) -> Vec<Finding> {
    let Some(script) = job.rule.command() else {
        return Vec::new();
    };
    let mut command = Command::new("sh");
    command
        .args(["-e", "-c", script])
        .current_dir(root.join(&job.file.dir))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("TYPESAFE_API_KEY");
    if let Some(relative) = job.target.and_then(|target| job.file.relative(target)) {
        command.env("FILE", format!("./{relative}"));
    }
    let target = job.target.map(String::from);
    let output = match command.output() {
        Ok(output) => output,
        Err(err) => return vec![job.finding(target, None, format!("could not start sh: {err}"))],
    };
    if output.status.success() {
        return Vec::new();
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let anchored: Vec<Finding> = text
        .lines()
        .filter_map(|line| anchor(line, root, &job.file.dir))
        .map(|(path, line, message)| job.finding(Some(path), Some(line), message))
        .collect();
    if anchored.is_empty() {
        vec![job.finding(target, None, clip(&text))]
    } else {
        anchored
    }
}

pub fn run_all(jobs: &[Job], root: &Path) -> Vec<Finding> {
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut findings: Vec<Finding> = pool::map(jobs, workers, |job| run_job(job, root))
        .into_iter()
        .flatten()
        .collect();
    findings.sort();
    findings.dedup();
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_resolve_against_the_rule_directory() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let found = anchor("./run.rs:12:5: too clever", root, "src");
        assert_eq!(
            found,
            Some(("src/run.rs".to_string(), 12, "too clever".to_string()))
        );
        let found = anchor("Cargo.toml:3: bad key", root, "src");
        assert_eq!(
            found,
            Some(("Cargo.toml".to_string(), 3, "bad key".to_string()))
        );
    }

    #[test]
    fn other_output_is_not_an_anchor() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(anchor("error: something broke", root, ""), None);
        assert_eq!(anchor("missing.rs:1: gone", root, ""), None);
        assert_eq!(anchor("../etc/passwd:1: nope", root, "src"), None);
    }
}
