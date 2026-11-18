use super::cache;
use super::hunks::Hunk;
use super::jev::{self, Jev};
use super::review::SemRule;
use super::target::Target;
use crate::rules::Example;
use anyhow::{Context, Result};
use std::collections::HashMap;

const MIN_GAP: f64 = 0.1;
const JITTER: f64 = 0.05;

#[derive(Default)]
pub struct Thresholds {
    by_fingerprint: HashMap<String, f64>,
}

pub struct Fit {
    pub rule_file: String,
    pub title: String,
    pub fingerprint: String,
    pub violating: Vec<f64>,
    pub complying: Vec<f64>,
}

pub fn fingerprint(rule: &SemRule) -> String {
    let examples: String = rule
        .rule
        .examples
        .iter()
        .map(|e| format!("{}\u{0}{}\u{0}", e.good, e.code))
        .collect();
    cache::key(&[
        jev::MODEL,
        &rule.file.parsed.context,
        &rule.rule.title,
        &rule.rule.prose,
        &examples,
    ])
}

impl Thresholds {
    pub fn set(&mut self, rule: &SemRule, threshold: f64) {
        self.by_fingerprint.insert(fingerprint(rule), threshold);
    }

    pub fn get(&self, rule: &SemRule) -> Option<f64> {
        self.by_fingerprint.get(&fingerprint(rule)).copied()
    }
}

impl Fit {
    pub fn threshold(&self) -> Option<f64> {
        let lowest_violation = self.violating.iter().copied().reduce(f64::min)?;
        let highest_compliant = self.complying.iter().copied().reduce(f64::max)?;
        (lowest_violation - highest_compliant >= MIN_GAP).then_some(lowest_violation - JITTER)
    }

    pub fn problem(&self) -> Option<String> {
        if self.violating.is_empty() || self.complying.is_empty() {
            return Some(
                "needs at least one `bad` and one `good` example before a threshold can be fitted"
                    .to_string(),
            );
        }
        if self.threshold().is_none() {
            return Some(format!(
                "the model cannot tell this rule's examples apart by at least {MIN_GAP}; reword the rule so the \
                 condition is stated literally, or replace the example it gets wrong"
            ));
        }
        None
    }
}

fn score(jev: &Jev, rule: &SemRule, example: &Example) -> Result<f64> {
    let lines: Vec<String> = example.code.lines().map(String::from).collect();
    let count = lines.len().max(1) as u32;
    let target = Target {
        path: "example".to_string(),
        lines,
        changed: None,
        diff: String::new(),
    };
    let hunk = Hunk {
        start: 1,
        end: count,
        context_start: 1,
        context_end: count,
    };
    let rules = [SemRule {
        file: rule.file,
        rule: rule.rule,
    }];
    let verdicts = jev.judge(&rules, &[0], &target, &hunk, Some(&example.code))?;
    verdicts
        .first()
        .map(|verdict| verdict.violates)
        .context("the TypeSafe API returned no verdict for an example; re-run uc test")
}

pub fn fit(jev: &Jev, rule: &SemRule) -> Result<Fit> {
    let mut fit = Fit {
        rule_file: rule.file.path.clone(),
        title: rule.rule.title.clone(),
        fingerprint: fingerprint(rule),
        violating: Vec::new(),
        complying: Vec::new(),
    };
    for example in rule
        .rule
        .examples
        .iter()
        .filter(|example| !example.code.trim().is_empty())
    {
        let probability = score(jev, rule, example)?;
        if example.good {
            fit.complying.push(probability);
        } else {
            fit.violating.push(probability);
        }
    }
    Ok(fit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(violating: &[f64], complying: &[f64]) -> Fit {
        Fit {
            rule_file: String::new(),
            title: String::new(),
            fingerprint: "f".to_string(),
            violating: violating.to_vec(),
            complying: complying.to_vec(),
        }
    }

    #[test]
    fn threshold_sits_just_under_the_weakest_violating_example() {
        let threshold = fit(&[0.9, 0.8], &[0.2, 0.4]).threshold();
        assert!(threshold.is_some_and(|value| (value - 0.75).abs() < 1e-9));
    }

    #[test]
    fn overlapping_or_missing_examples_give_no_threshold() {
        assert!(fit(&[0.5], &[0.45]).threshold().is_none());
        assert!(fit(&[0.9], &[]).problem().is_some());
        assert!(fit(&[0.5], &[0.45]).problem().is_some());
        assert!(fit(&[0.9], &[0.1]).problem().is_none());
    }
}
