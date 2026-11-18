use super::cache;
use super::hunks::Hunk;
use super::jev::{self, Jev};
use super::review::SemRule;
use super::target::Target;
use crate::rules::Example;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

pub const FILE: &str = ".undercurrent/thresholds.json";
const MIN_GAP: f64 = 0.1;
const JITTER: f64 = 0.05;

#[derive(Default)]
pub struct Thresholds {
    by_fingerprint: HashMap<String, f64>,
}

pub struct Fit {
    pub rule_file: String,
    pub id: String,
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
    pub fn parse(text: &str) -> Thresholds {
        let value: Value = serde_json::from_str(text).unwrap_or_default();
        let rules = value["rules"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let pairs = rules.iter().filter_map(|rule| {
            Some((
                rule["fingerprint"].as_str()?.to_string(),
                rule["threshold"].as_f64()?,
            ))
        });
        Thresholds {
            by_fingerprint: pairs.collect(),
        }
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
        id: rule.rule.id.clone(),
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

pub fn write(root: &Path, fits: &[Fit]) -> Result<()> {
    let round = |value: f64| (value * 100.0).round() / 100.0;
    let rounded =
        |values: &[f64]| -> Vec<f64> { values.iter().map(|value| round(*value)).collect() };
    let fitted = fits.iter().filter_map(|fit| {
        let threshold = fit.threshold()?;
        Some(json!({
            "rule_file": fit.rule_file,
            "id": fit.id,
            "fingerprint": fit.fingerprint,
            "threshold": round(threshold),
            "violating": rounded(&fit.violating),
            "complying": rounded(&fit.complying),
        }))
    });
    let body = json!({ "model": jev::MODEL, "rules": fitted.collect::<Vec<_>>() });
    let path = root.join(FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = format!("{}\n", serde_json::to_string_pretty(&body)?);
    std::fs::write(&path, text)
        .with_context(|| format!("could not write {FILE}; check the directory is writable"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fit(violating: &[f64], complying: &[f64]) -> Fit {
        Fit {
            rule_file: String::new(),
            id: String::new(),
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

    #[test]
    fn stored_thresholds_are_read_back_by_fingerprint() {
        let thresholds = Thresholds::parse(r#"{"rules":[{"fingerprint":"abc","threshold":0.62}]}"#);
        assert_eq!(thresholds.by_fingerprint.get("abc"), Some(&0.62));
        assert!(Thresholds::parse("not json").by_fingerprint.is_empty());
    }
}
