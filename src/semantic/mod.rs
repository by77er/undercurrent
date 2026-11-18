mod cache;
mod hunks;
mod jev;
mod prompt;
mod review;
mod target;
mod thresholds;
mod tools;

use crate::api::Client;
use crate::git::Git;
use crate::pool;
use crate::resolve::RuleFile;
use crate::run::Finding;
use anyhow::Result;
use cache::{Cache, Stored};
use hunks::Hunk;
use jev::Jev;
use review::{Candidate, SemRule};
use std::collections::{BTreeMap, BTreeSet};
use target::{Loaded, Target};
use thresholds::Thresholds;
use tools::Workspace;

const TRIAGE_WORKERS: usize = 8;
const CONFIRM_WORKERS: usize = 4;
const MAX_WHOLE_BYTES: usize = 300_000;
const FLAG_FOR_REVIEW_AT: f64 = 0.5;

pub struct Engines {
    jev: Option<Jev>,
    claude: Option<Client>,
}

impl Engines {
    pub fn from_env() -> Option<Engines> {
        let engines = Engines {
            jev: Jev::from_env(),
            claude: Client::from_env(),
        };
        (engines.jev.is_some() || engines.claude.is_some()).then_some(engines)
    }

    fn tag(&self) -> &'static str {
        match (&self.jev, &self.claude) {
            (Some(_), Some(_)) => "jev+claude",
            (Some(_), None) => "jev",
            _ => "claude",
        }
    }
}

#[derive(Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub checks: usize,
    pub cached: usize,
    pub flagged: usize,
    pub errors: Vec<String>,
    pub notes: Vec<String>,
    pub uncertain: Vec<String>,
}

struct Unit {
    target: usize,
    hunk: Hunk,
}

struct Pending {
    unit: usize,
    rules: Vec<usize>,
}

struct Review<'a> {
    engines: &'a Engines,
    thresholds: Thresholds,
    rules: Vec<SemRule<'a>>,
    targets: Vec<Target>,
    units: Vec<Unit>,
    cache: Cache,
    stored: Vec<(usize, Stored)>,
    report: Report,
}

fn finding(rule: &SemRule, stored: &Stored) -> Finding {
    Finding {
        path: Some(stored.path.clone()),
        line: Some(stored.line).filter(|line| *line > 0),
        rule_id: rule.rule.id.clone(),
        title: rule.rule.title.clone(),
        rule_file: rule.file.path.clone(),
        prose: rule.rule.prose.clone(),
        detail: stored.detail.clone(),
        semantic: true,
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

fn unconfirmed(candidate: &Candidate, target: &Target, threshold: f64) -> Stored {
    let quoted = target
        .lines
        .get((candidate.line as usize).wrapping_sub(1))
        .map(|line| line.trim())
        .unwrap_or_default();
    let probability = candidate.probability.unwrap_or_default();
    let detail = format!(
        "{quoted}\nA decision model judged the changed hunk starting here a violation with probability {probability:.2}, \
         above the {threshold:.2} fitted to this rule's examples. No second reviewer checked it.\n\
         Fix: change the hunk to follow the rule. If it already complies, add it to the rule as a `good` \
         example and run `uc test`, or set ANTHROPIC_API_KEY so a second reviewer confirms or dismisses this."
    );
    Stored {
        path: target.path.clone(),
        line: candidate.line,
        detail,
    }
}

fn merged(candidates: Vec<Candidate>) -> Vec<Candidate> {
    let mut groups: BTreeMap<(usize, Option<usize>, bool), Candidate> = BTreeMap::new();
    for candidate in candidates {
        match groups.get_mut(&(candidate.rule, candidate.target, candidate.spans_files)) {
            Some(first) => {
                first.reason = format!("{}; {}", first.reason, candidate.reason);
                first.units.extend(candidate.units);
            }
            None => {
                groups.insert(
                    (candidate.rule, candidate.target, candidate.spans_files),
                    candidate,
                );
            }
        }
    }
    groups.into_values().collect()
}

impl Review<'_> {
    fn key(&self, rule: usize, unit: usize) -> String {
        let (rule, unit) = (&self.rules[rule], &self.units[unit]);
        let target = &self.targets[unit.target];
        let hunk = &unit.hunk;
        let examples: String = rule
            .rule
            .examples
            .iter()
            .map(|e| format!("{}\u{0}{}\u{0}", e.good, e.code))
            .collect();
        let code =
            target.lines[hunk.context_start as usize - 1..hunk.context_end as usize].join("\n");
        let span = format!(
            "{}-{}",
            hunk.start - hunk.context_start,
            hunk.end - hunk.context_start
        );
        cache::key(&[
            self.engines.tag(),
            &format!("{:?}", self.thresholds.get(rule)),
            jev::MODEL,
            prompt::VERSION,
            prompt::TRIAGE_MODEL,
            prompt::CONFIRM_MODEL,
            &rule.file.parsed.context,
            &rule.rule.title,
            &rule.rule.prose,
            &examples,
            &target.path,
            &code,
            &span,
        ])
    }

    fn remember(&mut self, rule: usize, unit: usize, hits: &[Stored]) {
        let origin = self.units[unit].hunk.context_start;
        let relative = hits.iter().map(|hit| Stored {
            line: hit.line.saturating_sub(origin),
            ..hit.clone()
        });
        let key = self.key(rule, unit);
        self.cache.put(key, relative.collect());
    }

    fn recall(&mut self, rule: usize, unit: usize) -> Option<Vec<Stored>> {
        let key = self.key(rule, unit);
        let hits = self.cache.get(&key)?;
        let (origin, path) = (
            self.units[unit].hunk.context_start,
            &self.targets[self.units[unit].target].path,
        );
        let absolute = hits.into_iter().map(|hit| Stored {
            path: path.clone(),
            line: origin + hit.line,
            detail: hit.detail,
        });
        Some(absolute.collect())
    }

    fn pending(&mut self) -> Vec<Pending> {
        let mut pending = Vec::new();
        for unit in 0..self.units.len() {
            let path = &self.targets[self.units[unit].target].path;
            let applicable: Vec<usize> = (0..self.rules.len())
                .filter(|index| {
                    self.rules[*index]
                        .file
                        .applies(self.rules[*index].rule, path)
                })
                .collect();
            let mut fresh = Vec::new();
            for rule in applicable {
                self.report.checks += 1;
                match self.recall(rule, unit) {
                    Some(hits) => {
                        self.report.cached += 1;
                        self.stored.extend(hits.into_iter().map(|hit| (rule, hit)));
                    }
                    None => fresh.push(rule),
                }
            }
            if !fresh.is_empty() {
                pending.push(Pending { unit, rules: fresh });
            }
        }
        pending
    }

    fn triage_with_jev(&mut self, jev: &Jev, pending: &[Pending]) -> Vec<Candidate> {
        let (rules, targets, units) = (&self.rules, &self.targets, &self.units);
        let judged = pool::map(pending, TRIAGE_WORKERS, |job| {
            let unit = &units[job.unit];
            jev.judge(rules, &job.rules, &targets[unit.target], &unit.hunk, None)
        });
        let mut candidates = Vec::new();
        for (job, result) in pending.iter().zip(judged) {
            let (target, hunk) = (
                self.units[job.unit].target,
                self.units[job.unit].hunk.clone(),
            );
            let verdicts = match result {
                Ok(verdicts) => verdicts,
                Err(err) => {
                    self.report
                        .errors
                        .push(format!("{}: {err:#}", self.targets[target].path));
                    continue;
                }
            };
            for (rule, verdict) in job.rules.iter().zip(verdicts) {
                let fitted = self.thresholds.get(&self.rules[*rule]);
                let flag_at =
                    fitted.map_or(FLAG_FOR_REVIEW_AT, |fitted| fitted.min(FLAG_FOR_REVIEW_AT));
                let Some(unsettled) = verdict.flag(flag_at) else {
                    self.remember(*rule, job.unit, &[]);
                    continue;
                };
                let doubt = if unsettled {
                    ", and judged that it depends on code outside the hunk"
                } else {
                    ""
                };
                candidates.push(Candidate {
                    rule: *rule,
                    target: Some(target),
                    units: vec![job.unit],
                    line: hunk.start,
                    reason: format!("a decision model put the probability that {} break the rule at {:.2}{doubt}", hunk.span(), verdict.violates),
                    probability: Some(verdict.violates),
                    unsettled,
                    ..Candidate::default()
                });
            }
        }
        candidates
    }

    fn triage_with_claude(&mut self, claude: &Client, pending: &[Pending]) -> Vec<Candidate> {
        let mut by_target: BTreeMap<usize, BTreeMap<usize, Vec<usize>>> = BTreeMap::new();
        for job in pending {
            let per_rule = by_target.entry(self.units[job.unit].target).or_default();
            for rule in &job.rules {
                per_rule.entry(*rule).or_default().push(job.unit);
            }
        }
        let jobs: Vec<(usize, Vec<usize>)> = by_target
            .iter()
            .map(|(target, per_rule)| (*target, per_rule.keys().copied().collect()))
            .collect();
        let (rules, targets) = (&self.rules, &self.targets);
        let triaged = pool::map(&jobs, TRIAGE_WORKERS, |(target, indices)| {
            review::triage(claude, rules, indices, *target, &targets[*target])
        });
        let mut candidates = Vec::new();
        for ((target, indices), result) in jobs.iter().zip(triaged) {
            let mut flagged = match result {
                Ok(flagged) => flagged,
                Err(err) => {
                    self.report
                        .errors
                        .push(format!("{}: {err:#}", self.targets[*target].path));
                    continue;
                }
            };
            for rule in indices {
                let units = by_target[target][rule].clone();
                match flagged.iter_mut().find(|candidate| candidate.rule == *rule) {
                    Some(candidate) => candidate.units = units,
                    None => units
                        .iter()
                        .for_each(|unit| self.remember(*rule, *unit, &[])),
                }
            }
            candidates.extend(flagged);
        }
        candidates
    }

    fn whole_change(&mut self, claude: &Client) -> Vec<Candidate> {
        if self.targets.len() < 2 {
            return Vec::new();
        }
        let sizes = self.targets.iter().map(|t| {
            if t.diff.is_empty() {
                t.text().len()
            } else {
                t.diff.len()
            }
        });
        let size: usize = sizes.sum();
        if size > MAX_WHOLE_BYTES {
            self.report.notes.push(format!(
                "the change is {size} bytes, over the {MAX_WHOLE_BYTES} byte limit for the whole-change pass, \
                 so rules that span files were not judged; split the change"
            ));
            return Vec::new();
        }
        let applies = |rule: &SemRule| {
            self.targets
                .iter()
                .any(|target| rule.file.applies(rule.rule, &target.path))
        };
        let indices: Vec<usize> = (0..self.rules.len())
            .filter(|index| applies(&self.rules[*index]))
            .collect();
        match review::whole(claude, &self.rules, &indices, &self.targets) {
            Ok(candidates) => candidates,
            Err(err) => {
                self.report
                    .errors
                    .push(format!("whole-change pass: {err:#}"));
                Vec::new()
            }
        }
    }

    fn confirm(&mut self, claude: &Client, workspace: &Workspace, candidates: &[Candidate]) {
        let (rules, targets) = (&self.rules, &self.targets);
        let confirmed = pool::map(candidates, CONFIRM_WORKERS, |candidate| {
            review::confirm(
                claude,
                workspace,
                &rules[candidate.rule],
                candidate,
                targets,
            )
        });
        for (candidate, result) in candidates.iter().zip(confirmed) {
            let raw = match result {
                Ok(raw) => raw,
                Err(err) => {
                    self.report.errors.push(format!(
                        "{}: {err:#}",
                        self.rules[candidate.rule].rule.title
                    ));
                    continue;
                }
            };
            let verified: Vec<Stored> = raw
                .iter()
                .filter_map(|raw| verify(raw, &self.targets))
                .collect();
            if !candidate.spans_files {
                self.remember_confirmed(candidate, &verified);
            }
            self.stored
                .extend(verified.into_iter().map(|hit| (candidate.rule, hit)));
        }
    }

    fn remember_confirmed(&mut self, candidate: &Candidate, verified: &[Stored]) {
        let mut by_unit: BTreeMap<usize, Vec<Stored>> = candidate
            .units
            .iter()
            .map(|unit| (*unit, Vec::new()))
            .collect();
        for hit in verified {
            let holds = |unit: &Unit| {
                self.targets[unit.target].path == hit.path
                    && (unit.hunk.start..=unit.hunk.end).contains(&hit.line)
            };
            if let Some(unit) = (0..self.units.len()).find(|index| holds(&self.units[*index])) {
                by_unit.entry(unit).or_default().push(hit.clone());
            }
        }
        for (unit, hits) in by_unit {
            self.remember(candidate.rule, unit, &hits);
        }
    }

    fn accept(&mut self, candidates: &[Candidate]) {
        for candidate in candidates {
            let (Some(target), Some(unit)) = (candidate.target, candidate.units.first().copied())
            else {
                continue;
            };
            let probability = candidate.probability.unwrap_or_default();
            let fitted = self.thresholds.get(&self.rules[candidate.rule]);
            let why = match fitted {
                _ if candidate.unsettled => "it depends on code outside the hunk".to_string(),
                None => "the rule has no fitted threshold".to_string(),
                Some(threshold) if probability < threshold => {
                    format!("below the rule's threshold of {threshold:.2}")
                }
                Some(threshold) => {
                    let hit = unconfirmed(candidate, &self.targets[target], threshold);
                    self.remember(candidate.rule, unit, std::slice::from_ref(&hit));
                    self.stored.push((candidate.rule, hit));
                    continue;
                }
            };
            let (path, title) = (
                &self.targets[target].path,
                &self.rules[candidate.rule].rule.title,
            );
            self.report.uncertain.push(format!(
                "{path}:{}: {title} (probability {probability:.2}; {why})",
                candidate.line
            ));
        }
    }
}

fn semantic_rules(rule_files: &[RuleFile]) -> Vec<SemRule<'_>> {
    let rules = rule_files.iter().flat_map(|file| {
        file.parsed
            .rules
            .iter()
            .filter(|rule| rule.is_semantic())
            .map(move |rule| SemRule { file, rule })
    });
    rules.collect()
}

pub const THRESHOLDS_FILE: &str = thresholds::FILE;

pub fn digest(parts: &[&str]) -> String {
    cache::key(parts)
}

pub fn test(git: &Git, rule_files: &[RuleFile]) -> Result<Vec<thresholds::Fit>> {
    let Some(jev) = Jev::from_env() else {
        anyhow::bail!("uc test scores rule examples with Jev and TYPESAFE_API_KEY is not set; set it and run again");
    };
    let rules = semantic_rules(rule_files);
    let with_examples: Vec<&SemRule> = rules
        .iter()
        .filter(|rule| !rule.rule.examples.is_empty())
        .collect();
    let fits = pool::map(&with_examples, TRIAGE_WORKERS, |rule| {
        thresholds::fit(&jev, rule)
    });
    let fits = fits.into_iter().collect::<Result<Vec<_>>>()?;
    thresholds::write(git.root(), &fits)?;
    Ok(fits)
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
        let is_rule_file = path.rsplit('/').next() == Some(crate::resolve::FILE_NAME);
        if is_rule_file || !rules.iter().any(|rule| rule.file.applies(rule.rule, path)) {
            continue;
        }
        match Target::load(git, merge_base, path)? {
            Loaded::Ready(target) => targets.push(target),
            Loaded::Skipped(note) => report.notes.push(note),
        }
    }
    Ok(targets)
}

pub fn run(
    engines: &Engines,
    git: &Git,
    merge_base: Option<&str>,
    files: &[String],
    rule_files: &[RuleFile],
    fitted: &str,
) -> Result<Report> {
    let mut report = Report::default();
    let rules = semantic_rules(rule_files);
    let targets = load_targets(git, merge_base, files, &rules, &mut report)?;
    let all_files: BTreeSet<String> = git.all_files()?.into_iter().collect();
    let workspace = Workspace {
        root: git.root(),
        files: &all_files,
    };
    let thresholds = Thresholds::parse(fitted);
    let units: Vec<Unit> = targets
        .iter()
        .enumerate()
        .flat_map(|(target, loaded)| {
            hunks::hunks(loaded)
                .into_iter()
                .map(move |hunk| Unit { target, hunk })
        })
        .collect();
    let mut review = Review {
        engines,
        thresholds,
        rules,
        targets,
        units,
        cache: Cache::load(git),
        stored: Vec::new(),
        report,
    };

    let pending = review.pending();
    let mut candidates = match (&engines.jev, &engines.claude) {
        (Some(jev), _) => review.triage_with_jev(jev, &pending),
        (None, Some(claude)) => review.triage_with_claude(claude, &pending),
        (None, None) => Vec::new(),
    };
    match &engines.claude {
        Some(claude) => {
            candidates.extend(review.whole_change(claude));
            candidates = merged(candidates);
            review.confirm(claude, &workspace, &candidates);
        }
        None => review.accept(&candidates),
    }
    review.report.flagged = candidates.len();
    if !review.report.uncertain.is_empty() {
        review.report.uncertain.sort();
        review.report.notes.push(format!(
            "{} possible violation(s) did not fail the check. Without a second reviewer a rule blocks only above a \
             threshold fitted to its own examples: add `good` and `bad` examples to the rule and run `uc test`, \
             or set ANTHROPIC_API_KEY to have each one confirmed or dismissed",
            review.report.uncertain.len()
        ));
    }
    if let Err(err) = review.cache.save() {
        review.report.notes.push(format!("{err:#}"));
    }

    let mut report = review.report;
    report.findings = review
        .stored
        .iter()
        .map(|(index, hit)| finding(&review.rules[*index], hit))
        .collect();
    report.findings.sort();
    report
        .findings
        .dedup_by(|a, b| a.path == b.path && a.line == b.line && a.rule_id == b.rule_id);
    Ok(report)
}
