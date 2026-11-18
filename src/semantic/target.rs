use crate::git::Git;
use anyhow::Result;
use std::collections::BTreeSet;

const MAX_FILE_BYTES: usize = 200_000;

pub struct Target {
    pub path: String,
    pub lines: Vec<String>,
    pub changed: Option<BTreeSet<u32>>,
    pub diff: String,
}

pub enum Loaded {
    Ready(Target),
    Skipped(String),
}

fn added_lines(diff: &str) -> BTreeSet<u32> {
    let mut lines = BTreeSet::new();
    for header in diff.lines().filter(|line| line.starts_with("@@ ")) {
        let Some(added) = header.split(' ').find(|part| part.starts_with('+')) else {
            continue;
        };
        let mut parts = added[1..].split(',');
        let start: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let count: u32 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);
        lines.extend(start..start + count);
    }
    lines
}

fn ranges(lines: &BTreeSet<u32>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut span: Option<(u32, u32)> = None;
    for &line in lines {
        span = match span {
            Some((start, end)) if line == end + 1 => Some((start, line)),
            Some((start, end)) => {
                out.push(if start == end {
                    start.to_string()
                } else {
                    format!("{start}-{end}")
                });
                Some((line, line))
            }
            None => Some((line, line)),
        };
    }
    if let Some((start, end)) = span {
        out.push(if start == end {
            start.to_string()
        } else {
            format!("{start}-{end}")
        });
    }
    out.join(", ")
}

impl Target {
    pub fn load(git: &Git, merge_base: Option<&str>, path: &str) -> Result<Loaded> {
        let bytes = std::fs::read(git.root().join(path))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Ok(Loaded::Skipped(format!(
                "{path} is over {MAX_FILE_BYTES} bytes, so no semantic rule judged it; split the file"
            )));
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return Ok(Loaded::Skipped(format!(
                "{path} is not UTF-8 text, so no semantic rule judged it"
            )));
        };
        let (diff, changed) = match merge_base {
            Some(merge_base) => {
                let diff = git.diff(merge_base, path, 3)?;
                let changed = match diff.is_empty() {
                    true => None,
                    false => Some(added_lines(&git.diff(merge_base, path, 0)?)),
                };
                (diff, changed)
            }
            None => (String::new(), None),
        };
        Ok(Loaded::Ready(Target {
            path: path.to_string(),
            lines: text.lines().map(String::from).collect(),
            changed,
            diff,
        }))
    }

    pub fn is_changed(&self, line: u32) -> bool {
        let exists = line >= 1 && (line as usize) <= self.lines.len();
        exists
            && self
                .changed
                .as_ref()
                .is_none_or(|changed| changed.contains(&line))
    }

    pub fn numbered(&self) -> String {
        let numbered = self.lines.iter().enumerate();
        numbered
            .map(|(index, line)| format!("{:>5}| {line}\n", index + 1))
            .collect()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn changed_summary(&self) -> String {
        match &self.changed {
            Some(changed) => ranges(changed),
            None => "every line (the whole file is new or under review)".to_string(),
        }
    }

    pub fn locate(&self, claimed: u32, evidence: &str) -> Option<u32> {
        let wanted = evidence
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())?;
        let numbered = self
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| (index as u32 + 1, line));
        let matches =
            numbered.filter(|(number, line)| line.contains(wanted) && self.is_changed(*number));
        matches
            .map(|(number, _)| number)
            .min_by_key(|number| number.abs_diff(claimed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(changed: Option<&[u32]>) -> Target {
        Target {
            path: "a.rs".to_string(),
            lines: [
                "fn a() {",
                "    x.unwrap();",
                "}",
                "fn b() {",
                "    x.unwrap();",
                "}",
            ]
            .map(String::from)
            .to_vec(),
            changed: changed.map(|lines| lines.iter().copied().collect()),
            diff: String::new(),
        }
    }

    #[test]
    fn hunk_headers_give_added_lines() {
        let diff = "@@ -1,0 +2,2 @@\n+a\n+b\n@@ -9 +12 @@\n-x\n+y\n@@ -20,3 +22,0 @@\n-gone\n";
        let lines = added_lines(diff);
        assert_eq!(lines.into_iter().collect::<Vec<_>>(), vec![2, 3, 12]);
        assert_eq!(ranges(&[2, 3, 4, 9].into_iter().collect()), "2-4, 9");
    }

    #[test]
    fn evidence_must_sit_on_a_changed_line() {
        assert_eq!(target(Some(&[5])).locate(2, "x.unwrap();"), Some(5));
        assert_eq!(target(Some(&[1])).locate(2, "x.unwrap();"), None);
        assert_eq!(target(None).locate(2, "x.unwrap();"), Some(2));
        assert_eq!(target(None).locate(2, "invented()"), None);
        assert_eq!(target(None).locate(2, "  \n"), None);
    }
}
