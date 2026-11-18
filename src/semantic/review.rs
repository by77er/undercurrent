use super::prompt;
use super::target::Target;
use super::tools::{self, Workspace};
use crate::api::{Client, Reply};
use crate::resolve::RuleFile;
use crate::rules::Rule;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const MAX_TURNS: usize = 24;
const MAX_LISTED_FILES: usize = 100;
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

pub struct SemRule<'a> {
    pub file: &'a RuleFile,
    pub rule: &'a Rule,
}

#[derive(Default)]
pub struct Candidate {
    pub rule: usize,
    pub target: Option<usize>,
    pub units: Vec<usize>,
    pub line: u32,
    pub reason: String,
    pub spans_files: bool,
    pub probability: Option<f64>,
    pub unsettled: bool,
}

pub struct Raw {
    pub path: String,
    pub line: u32,
    pub evidence: String,
    pub explanation: String,
    pub fix: String,
}

fn rules_block(rules: &[SemRule], indices: &[usize]) -> String {
    let blocks = indices.iter().enumerate().map(|(local, index)| {
        let rule = &rules[*index];
        prompt::rule_block(local + 1, rule.file, rule.rule)
    });
    format!("<rules>\n{}</rules>\n", blocks.collect::<String>())
}

fn file_block(target: &Target) -> String {
    let mut out = format!(
        "<file path=\"{}\">\n{}</file>\n",
        target.path,
        target.numbered()
    );
    out.push_str(&format!(
        "<changed_lines>{}</changed_lines>\n",
        target.changed_summary()
    ));
    if !target.diff.is_empty() {
        out.push_str(&format!("<diff>\n{}</diff>\n", target.diff));
    }
    out
}

fn parse(reply: &Reply, stage: &str) -> Result<Value> {
    match reply.stop_reason.as_str() {
        "end_turn" => {}
        "refusal" => bail!("the model declined the {stage} request, so the rule was not judged; re-run, and if it repeats, reword the rule"),
        "max_tokens" => bail!("the {stage} reply was cut off at the token limit; split the file or the rule set"),
        other => bail!("the {stage} request ended with unexpected stop reason `{other}`; re-run the check"),
    }
    serde_json::from_str(&reply.text())
        .with_context(|| format!("the {stage} reply was not the expected JSON; re-run the check"))
}

fn verdicts(value: &Value) -> impl Iterator<Item = (usize, &Value)> {
    let list = value["verdicts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    list.iter()
        .filter_map(|verdict| Some((usize::try_from(verdict["rule"].as_u64()?).ok()?, verdict)))
}

fn line_of(value: &Value) -> u32 {
    value["line"]
        .as_u64()
        .and_then(|line| u32::try_from(line).ok())
        .unwrap_or(0)
}

pub fn triage(
    client: &Client,
    rules: &[SemRule],
    indices: &[usize],
    target_index: usize,
    target: &Target,
) -> Result<Vec<Candidate>> {
    let body = json!({
        "model": prompt::TRIAGE_MODEL,
        "max_tokens": 8000,
        "system": prompt::triage_system(),
        "output_config": { "effort": "medium", "format": prompt::triage_format() },
        "messages": [{ "role": "user", "content": format!("{}{}", rules_block(rules, indices), file_block(target)) }],
    });
    let value = parse(&client.messages(&body, None)?, "triage")?;
    let mut passed = vec![false; indices.len()];
    let mut candidates = Vec::new();
    for (local, verdict) in verdicts(&value) {
        let Some(index) = local.checked_sub(1).and_then(|local| indices.get(local)) else {
            continue;
        };
        if verdict["verdict"] == "pass" {
            passed[local - 1] = true;
            continue;
        }
        candidates.push(Candidate {
            rule: *index,
            target: Some(target_index),
            line: line_of(verdict),
            reason: verdict["reason"].as_str().unwrap_or_default().to_string(),
            ..Candidate::default()
        });
    }
    for (local, index) in indices.iter().enumerate() {
        let flagged = candidates.iter().any(|candidate| candidate.rule == *index);
        if !passed[local] && !flagged {
            let reason = "the first reviewer gave no verdict for this rule".to_string();
            let target = Some(target_index);
            candidates.push(Candidate {
                rule: *index,
                target,
                reason,
                ..Candidate::default()
            });
        }
    }
    Ok(candidates)
}

pub fn whole(
    client: &Client,
    rules: &[SemRule],
    indices: &[usize],
    targets: &[Target],
) -> Result<Vec<Candidate>> {
    let mut content = rules_block(rules, indices);
    for target in targets {
        if target.diff.is_empty() {
            content.push_str(&file_block(target));
        } else {
            content.push_str(&format!(
                "<file path=\"{}\">\n<diff>\n{}</diff>\n</file>\n",
                target.path, target.diff
            ));
        }
    }
    let body = json!({
        "model": prompt::TRIAGE_MODEL,
        "max_tokens": 8000,
        "system": prompt::whole_system(),
        "output_config": { "effort": "medium", "format": prompt::whole_format() },
        "messages": [{ "role": "user", "content": content }],
    });
    let value = parse(&client.messages(&body, None)?, "whole-change")?;
    let mut candidates = Vec::new();
    for (local, verdict) in verdicts(&value) {
        let Some(index) = local.checked_sub(1).and_then(|local| indices.get(local)) else {
            continue;
        };
        if verdict["verdict"] != "violation" {
            continue;
        }
        let path = verdict["path"].as_str().unwrap_or_default();
        candidates.push(Candidate {
            rule: *index,
            target: targets.iter().position(|target| target.path == path),
            line: line_of(verdict),
            reason: format!(
                "across the whole change: {}",
                verdict["reason"].as_str().unwrap_or_default()
            ),
            spans_files: true,
            ..Candidate::default()
        });
    }
    Ok(candidates)
}

fn confirm_request(rule: &SemRule, candidate: &Candidate, targets: &[Target]) -> String {
    let mut out = prompt::rule_block(1, rule.file, rule.rule);
    let focus = candidate.target.map(|index| &targets[index]);
    let place = focus
        .map(|target| format!(" in {} near line {}", target.path, candidate.line))
        .unwrap_or_default();
    out.push_str(&format!(
        "<flag>\nThe first reviewer flagged this rule{place}: {}\n</flag>\n",
        candidate.reason
    ));
    out.push_str("<changed_files>\n");
    for target in targets.iter().take(MAX_LISTED_FILES) {
        out.push_str(&format!(
            "{}: lines {}\n",
            target.path,
            target.changed_summary()
        ));
    }
    if targets.len() > MAX_LISTED_FILES {
        out.push_str(&format!(
            "(and {} more changed files)\n",
            targets.len() - MAX_LISTED_FILES
        ));
    }
    out.push_str("</changed_files>\n");
    if let Some(target) = focus {
        out.push_str(&file_block(target));
    }
    out
}

fn tool_results(workspace: &Workspace, content: &[Value]) -> Vec<Value> {
    let calls = content.iter().filter(|block| block["type"] == "tool_use");
    let results = calls.map(|call| {
        let outcome = workspace.execute(call["name"].as_str().unwrap_or_default(), &call["input"]);
        let (text, failed) = match outcome {
            Ok(text) => (text, false),
            Err(text) => (text, true),
        };
        json!({ "type": "tool_result", "tool_use_id": call["id"], "content": text, "is_error": failed })
    });
    results.collect()
}

pub fn confirm(
    client: &Client,
    workspace: &Workspace,
    rule: &SemRule,
    candidate: &Candidate,
    targets: &[Target],
) -> Result<Vec<Raw>> {
    let mut messages =
        vec![json!({ "role": "user", "content": confirm_request(rule, candidate, targets) })];
    for _ in 0..MAX_TURNS {
        let body = json!({
            "model": prompt::CONFIRM_MODEL,
            "max_tokens": 16000,
            "system": prompt::confirm_system(),
            "tools": tools::definitions(),
            "output_config": { "effort": "high", "format": prompt::confirm_format() },
            "fallbacks": "default",
            "messages": messages,
        });
        let reply = client.messages(&body, Some(FALLBACK_BETA))?;
        messages.push(json!({ "role": "assistant", "content": reply.content }));
        match reply.stop_reason.as_str() {
            "tool_use" => messages.push(
                json!({ "role": "user", "content": tool_results(workspace, &reply.content) }),
            ),
            "pause_turn" => {}
            _ => {
                let value = parse(&reply, "confirmation")?;
                let list = value["findings"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let text = |finding: &Value, field: &str| {
                    finding[field].as_str().unwrap_or_default().to_string()
                };
                let findings = list.iter().map(|finding| Raw {
                    path: text(finding, "path"),
                    line: line_of(finding),
                    evidence: text(finding, "evidence"),
                    explanation: text(finding, "explanation"),
                    fix: text(finding, "fix"),
                });
                return Ok(findings.collect());
            }
        }
    }
    bail!("the confirming reviewer did not reach a verdict in {MAX_TURNS} turns; re-run the check")
}
