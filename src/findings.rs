use crate::check::Outcome;
use crate::run::Finding;
use crate::semantic;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

pub struct Item {
    pub fingerprint: String,
    pub semantic: bool,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub title: String,
    pub rule_id: String,
    pub rule_file: String,
    pub prose: String,
    pub detail: String,
}

#[derive(Default)]
pub struct Report {
    pub commands_complete: bool,
    pub semantic_complete: bool,
    pub items: Vec<Item>,
}

pub fn line_fingerprint(rule_id: &str, rule_file: &str, path: &str, line_text: &str) -> String {
    semantic::digest(&[rule_id, rule_file, path, line_text.trim()])
}

fn fingerprint(finding: &Finding, root: &Path) -> String {
    let path = finding.path.as_deref().unwrap_or_default();
    let flagged_line = finding
        .path
        .as_ref()
        .zip(finding.line)
        .and_then(|(path, line)| {
            let text = std::fs::read_to_string(root.join(path)).ok()?;
            text.lines().nth(line as usize - 1).map(String::from)
        });
    let anchor = match (&finding.path, flagged_line) {
        (_, Some(text)) => text,
        (Some(_), None) => String::new(),
        (None, None) => finding.detail.clone(),
    };
    line_fingerprint(&finding.rule_id, &finding.rule_file, path, &anchor)
}

pub fn write(destination: &str, outcome: &Outcome, root: &Path) -> Result<()> {
    let findings = outcome.findings.iter().map(|finding| {
        json!({
            "fingerprint": fingerprint(finding, root),
            "semantic": finding.semantic,
            "path": finding.path,
            "line": finding.line,
            "title": finding.title,
            "rule_id": finding.rule_id,
            "rule_file": finding.rule_file,
            "prose": finding.prose,
            "detail": finding.detail,
        })
    });
    let body = json!({
        "commands_complete": outcome.commands_complete,
        "semantic_complete": outcome.semantic_complete,
        "findings": findings.collect::<Vec<_>>(),
    });
    std::fs::write(destination, serde_json::to_string(&body)?).with_context(|| {
        format!(
            "could not write findings to {destination}; check the directory exists and is writable"
        )
    })
}

fn item(value: &Value) -> Option<Item> {
    let text = |field: &str| value[field].as_str().map(String::from);
    Some(Item {
        fingerprint: text("fingerprint")?,
        semantic: value["semantic"].as_bool()?,
        path: text("path"),
        line: value["line"]
            .as_u64()
            .and_then(|line| u32::try_from(line).ok()),
        title: text("title")?,
        rule_id: text("rule_id")?,
        rule_file: text("rule_file")?,
        prose: text("prose")?,
        detail: text("detail")?,
    })
}

pub fn read(paths: &[String]) -> Result<Report> {
    let mut report = Report::default();
    for path in paths {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "could not read findings from {path}; pass files written by `uc check --findings`"
            )
        })?;
        let value: Value = serde_json::from_str(&text).with_context(|| {
            format!("{path} is not a findings file; pass files written by `uc check --findings`")
        })?;
        report.commands_complete |= value["commands_complete"].as_bool().unwrap_or(false);
        report.semantic_complete |= value["semantic_complete"].as_bool().unwrap_or(false);
        let list = value["findings"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for found in list.iter().filter_map(item) {
            if !report
                .items
                .iter()
                .any(|existing| existing.fingerprint == found.fingerprint)
            {
                report.items.push(found);
            }
        }
    }
    Ok(report)
}
