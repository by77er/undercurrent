mod cache;
mod prompt;
mod review;
mod target;
mod tools;

use crate::api::Client;
use crate::git::Git;
use crate::pool;
use crate::resolve::RuleFile;
use crate::run::Finding;
use anyhow::Result;
use cache::{Cache, Stored};
use review::{Candidate, SemRule};
use std::collections::BTreeSet;
use target::{Loaded, Target};
use tools::Workspace;

const TRIAGE_WORKERS: usize = 8;
const CONFIRM_WORKERS: usize = 4;
const MAX_WHOLE_BYTES: usize = 300_000;

#[derive(Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub checks: usize,
    pub cached: usize,
    pub flagged: usize,
    pub errors: Vec<String>,
    pub notes: Vec<String>,
}

struct Pending {
    target: usize,
    rules: Vec<usize>,
}

fn cache_key(rule: &SemRule, target: &Target) -> String {
    let examples: String = rule
        .rule
        .examples
        .iter()
        .map(|e| format!("{}\u{0}{}\u{0}", e.good, e.code))
        .collect();
    cache::key(&[
        prompt::VERSION,
        prompt::TRIAGE_MODEL,
        prompt::CONFIRM_MODEL,
        &rule.file.parsed.context,
        &rule.rule.title,
        &rule.rule.prose,
        &examples,
        &target.path,
        &target.text(),
        &target.diff,
    ])
}

fn finding(rule: &SemRule, stored: &Stored) -> Finding {
    Finding {
        path: Some(stored.path.clone()),
        line: Some(stored.line),
        rule_id: rule.rule.id.clone(),
        title: rule.rule.title.clone(),
        rule_file: rule.file.path.clone(),
        prose: rule.rule.prose.clone(),
        detail: stored.detail.clone(),
    }
}

fn verify(raw: &review::Raw, targets: &[Target]) -> Option<Stored> {
    let path = raw.path.trim_start_matches("./");
    let target = targets.iter().find(|target| target.path == path)?;
    let line = target.locate(raw.line, &raw.evidence)?;
    let quoted = target.lines.get(line as usize - 1)?.trim();
    let detail = format!(
        "{quoted}\n{}\nFix: {}",
        raw.explanation.trim(),
        raw.fix.trim()
    );
    Some(Stored {
        path: target.path.clone(),
        line,
        detail,
    })
}

fn load_targets(
    git: &Git,
    merge_base: Option<&str>,
    files: &[String],
    rules: &[SemRule],
    report: &mut Report,
) -> Result<Vec<Target>> {
    let mut targets = Vec::new();
    for path in files {
        if !rules.iter().any(|rule| rule.file.applies(rule.rule, path)) {
            continue;
        }
        match Target::load(git, merge_base, path)? {
            Loaded::Ready(target) => targets.push(target),
            Loaded::Skipped(note) => report.notes.push(note),
        }
    }
    Ok(targets)
}

fn whole_change(
    client: &Client,
    rules: &[SemRule],
    targets: &[Target],
    report: &mut Report,
) -> Vec<Candidate> {
    if targets.len() < 2 {
        return Vec::new();
    }
    let size: usize = targets
        .iter()
        .map(|t| {
            if t.diff.is_empty() {
                t.text().len()
            } else {
                t.diff.len()
            }
        })
        .sum();
    if size > MAX_WHOLE_BYTES {
        report.notes.push(format!(
            "the change is {size} bytes, over the {MAX_WHOLE_BYTES} byte limit for the whole-change pass, \
             so rules that span files were not judged; split the change"
        ));
        return Vec::new();
    }
    let applies = |rule: &SemRule| {
        targets
            .iter()
            .any(|target| rule.file.applies(rule.rule, &target.path))
    };
    let indices: Vec<usize> = (0..rules.len())
        .filter(|index| applies(&rules[*index]))
        .collect();
    match review::whole(client, rules, &indices, targets) {
        Ok(candidates) => candidates,
        Err(err) => {
            report.errors.push(format!("whole-change pass: {err:#}"));
            Vec::new()
        }
    }
}

pub fn run(
    client: &Client,
    git: &Git,
    merge_base: Option<&str>,
    files: &[String],
    rule_files: &[RuleFile],
) -> Result<Report> {
    let mut report = Report::default();
    let rules: Vec<SemRule> = rule_files
        .iter()
        .flat_map(|file| {
            file.parsed
                .rules
                .iter()
                .filter(|rule| rule.is_semantic())
                .map(move |rule| SemRule { file, rule })
        })
        .collect();
    let targets = load_targets(git, merge_base, files, &rules, &mut report)?;
    let all_files: BTreeSet<String> = git.all_files()?.into_iter().collect();
    let workspace = Workspace {
        root: git.root(),
        files: &all_files,
    };
    let mut cache = Cache::load(git);
    let mut stored: Vec<(usize, Stored)> = Vec::new();

    let mut pending = Vec::new();
    for (target_index, target) in targets.iter().enumerate() {
        let mut fresh = Vec::new();
        for (index, rule) in rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.file.applies(rule.rule, &target.path))
        {
            report.checks += 1;
            match cache.get(&cache_key(rule, target)) {
                Some(hits) => {
                    report.cached += 1;
                    stored.extend(hits.into_iter().map(|hit| (index, hit)));
                }
                None => fresh.push(index),
            }
        }
        if !fresh.is_empty() {
            pending.push(Pending {
                target: target_index,
                rules: fresh,
            });
        }
    }

    let triaged = pool::map(&pending, TRIAGE_WORKERS, |job| {
        review::triage(client, &rules, &job.rules, job.target, &targets[job.target])
    });
    let mut candidates = Vec::new();
    for (job, result) in pending.iter().zip(triaged) {
        let target = &targets[job.target];
        match result {
            Ok(flagged) => {
                for index in job
                    .rules
                    .iter()
                    .filter(|index| !flagged.iter().any(|c| c.rule == **index))
                {
                    cache.put(cache_key(&rules[*index], target), Vec::new());
                }
                candidates.extend(flagged);
            }
            Err(err) => report.errors.push(format!("{}: {err:#}", target.path)),
        }
    }
    candidates.extend(whole_change(client, &rules, &targets, &mut report));
    report.flagged = candidates.len();

    let confirmed = pool::map(&candidates, CONFIRM_WORKERS, |candidate| {
        review::confirm(
            client,
            &workspace,
            &rules[candidate.rule],
            candidate,
            &targets,
        )
    });
    for (candidate, result) in candidates.iter().zip(confirmed) {
        let rule = &rules[candidate.rule];
        match result {
            Ok(raw) => {
                let verified: Vec<Stored> =
                    raw.iter().filter_map(|raw| verify(raw, &targets)).collect();
                if let Some(target) = candidate.target.filter(|_| !candidate.spans_files) {
                    cache.put(cache_key(rule, &targets[target]), verified.clone());
                }
                stored.extend(verified.into_iter().map(|hit| (candidate.rule, hit)));
            }
            Err(err) => report.errors.push(format!("{}: {err:#}", rule.rule.title)),
        }
    }
    if let Err(err) = cache.save() {
        report.notes.push(format!("{err:#}"));
    }

    report.findings = stored
        .iter()
        .map(|(index, hit)| finding(&rules[*index], hit))
        .collect();
    report.findings.sort();
    report
        .findings
        .dedup_by(|a, b| a.path == b.path && a.line == b.line && a.rule_id == b.rule_id);
    Ok(report)
}
