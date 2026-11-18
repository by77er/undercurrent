use crate::api::{self, Failure};
use crate::findings::{self, Item, Report};
use crate::git::Git;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;

const MARKER: &str = "<!-- undercurrent:";
const SUMMARY_MARKER: &str = "<!-- undercurrent:summary -->";
const THREADS: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{id isResolved path comments(first:1){nodes{databaseId body author{__typename}}}}}}}}";
const RESOLVE: &str =
    "mutation($thread:ID!){resolveReviewThread(input:{threadId:$thread}){thread{id}}}";
const REOPEN: &str =
    "mutation($thread:ID!){unresolveReviewThread(input:{threadId:$thread}){thread{id}}}";

struct GitHub {
    agent: ureq::Agent,
    api: String,
    graphql: String,
    token: String,
    repository: String,
    number: u64,
    head: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Thread {
    pub id: String,
    pub resolved: bool,
    pub semantic: bool,
    pub fingerprint: String,
    pub rule: Option<(String, String)>,
    pub path: Option<String>,
    pub comment: u64,
    pub body: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub post: Vec<usize>,
    pub resolve: Vec<String>,
    pub reopen: Vec<String>,
}

fn marker(item: &Item) -> String {
    let kind = if item.semantic {
        "semantic"
    } else {
        "commands"
    };
    format!(
        "{MARKER}{kind}:{}:{}@{} -->",
        item.fingerprint, item.rule_id, item.rule_file
    )
}

#[derive(Debug, PartialEq)]
struct Marker {
    semantic: bool,
    fingerprint: String,
    rule: Option<(String, String)>,
}

fn parse_marker(body: &str) -> Option<Marker> {
    let rest = &body[body.find(MARKER)? + MARKER.len()..];
    let (kind, rest) = rest.split_once(':')?;
    let inner = rest.split_once(" -->")?.0;
    let semantic = match kind {
        "semantic" => true,
        "commands" => false,
        _ => return None,
    };
    let (fingerprint, rule) = match inner.split_once(':') {
        Some((fingerprint, rule)) => (fingerprint, rule.split_once('@')),
        None => (inner, None),
    };
    let rule = rule.map(|(id, file)| (id.to_string(), file.to_string()));
    Some(Marker {
        semantic,
        fingerprint: fingerprint.to_string(),
        rule,
    })
}

fn flagged_line_remains(thread: &Thread, root: Option<&Path>) -> bool {
    let (Some((rule_id, rule_file)), Some(path)) = (&thread.rule, &thread.path) else {
        return false;
    };
    let Some(root) = root else { return true };
    let Ok(text) = std::fs::read_to_string(root.join(path)) else {
        return false;
    };
    text.lines().any(|line| {
        findings::line_fingerprint(rule_id, rule_file, path, line) == thread.fingerprint
    })
}

fn closed(thread: &Thread, head: &str) -> String {
    let short: String = head.chars().take(7).collect();
    let retired = thread.body.replacen(MARKER, "<!-- undercurrent-closed:", 1);
    format!("**Resolved:** this finding no longer appears as of {short}.\n\n---\n\n{retired}")
}

fn fenced(text: &str) -> String {
    format!("\n```\n{}\n```\n", text.replace("```", "'''"))
}

fn detail(item: &Item) -> String {
    if item.detail.is_empty() {
        return String::new();
    }
    if !item.semantic {
        return fenced(&item.detail);
    }
    let (quoted, explanation) = item.detail.split_once('\n').unwrap_or((&item.detail, ""));
    format!(
        "{}\n{}\n",
        fenced(quoted),
        explanation.replace("\nFix: ", "\n\n**Fix:** ")
    )
}

fn body(item: &Item) -> String {
    format!(
        "**{}**\n\n{}\n{}\n<sub>Undercurrent rule `{}` in `{}`</sub>\n{}",
        item.title,
        item.prose,
        detail(item),
        item.rule_id,
        item.rule_file,
        marker(item)
    )
}

pub fn plan(report: &Report, threads: &[Thread], lingering: &HashSet<String>) -> Plan {
    let mut plan = Plan::default();
    for thread in threads {
        let present = report
            .items
            .iter()
            .any(|item| item.fingerprint == thread.fingerprint);
        let judged = if thread.semantic {
            report.semantic_complete
        } else {
            report.commands_complete
        };
        let unsettled = thread.semantic && lingering.contains(&thread.id);
        if present && thread.resolved {
            plan.reopen.push(thread.id.clone());
        } else if !present && !thread.resolved && judged && !unsettled {
            plan.resolve.push(thread.id.clone());
        }
    }
    for (index, item) in report.items.iter().enumerate() {
        let known = threads
            .iter()
            .any(|thread| thread.fingerprint == item.fingerprint);
        if item.path.is_some() && !known {
            plan.post.push(index);
        }
    }
    plan
}

fn env(name: &str, hint: &str) -> Result<String> {
    api::env_key(name).with_context(|| format!("{name} is not set; {hint}"))
}

impl GitHub {
    fn from_env() -> Result<GitHub> {
        let in_actions = "run `uc comment` inside a GitHub Actions job triggered by a pull request";
        let event_path = env("GITHUB_EVENT_PATH", in_actions)?;
        let text = std::fs::read_to_string(&event_path)
            .with_context(|| format!("could not read {event_path}; {in_actions}"))?;
        let event: Value = serde_json::from_str(&text)
            .with_context(|| format!("{event_path} is not JSON; {in_actions}"))?;
        let pull = &event["pull_request"];
        let (Some(number), Some(head)) = (pull["number"].as_u64(), pull["head"]["sha"].as_str())
        else {
            bail!("the triggering event is not a pull request, so there is nothing to comment on; {in_actions}");
        };
        let api =
            api::env_key("GITHUB_API_URL").unwrap_or_else(|| "https://api.github.com".to_string());
        Ok(GitHub {
            agent: api::agent(),
            graphql: api::env_key("GITHUB_GRAPHQL_URL").unwrap_or_else(|| format!("{api}/graphql")),
            api,
            token: env(
                "GITHUB_TOKEN",
                "pass a token that can write to pull requests",
            )?,
            repository: env("GITHUB_REPOSITORY", in_actions)?,
            number,
            head: head.to_string(),
        })
    }

    fn send(&self, method: &str, url: &str, body: Option<&Value>) -> Result<Value, Failure> {
        let bearer = format!("Bearer {}", self.token);
        let headers = [
            ("authorization", bearer.as_str()),
            ("accept", "application/vnd.github+json"),
            ("x-github-api-version", "2022-11-28"),
            ("user-agent", "undercurrent"),
        ];
        api::send(&self.agent, method, url, &headers, body)
    }

    fn rest(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, Failure> {
        self.send(
            method,
            &format!("{}/repos/{}/{path}", self.api, self.repository),
            body,
        )
    }

    fn query(&self, query: &str, variables: Value) -> Result<Value, Failure> {
        let value = self.send(
            "POST",
            &self.graphql,
            Some(&json!({ "query": query, "variables": variables })),
        )?;
        match value["errors"]
            .as_array()
            .filter(|errors| !errors.is_empty())
        {
            Some(errors) => Err(Failure::Rejected {
                code: 403,
                body: Value::Array(errors.clone()).to_string(),
            }),
            None => Ok(value),
        }
    }

    fn threads(&self) -> Result<Vec<Thread>, Failure> {
        let (owner, name) = self
            .repository
            .split_once('/')
            .unwrap_or((&self.repository, ""));
        let mut threads = Vec::new();
        let mut after = Value::Null;
        loop {
            let variables =
                json!({ "owner": owner, "name": name, "number": self.number, "after": after });
            let value = self.query(THREADS, variables)?;
            let page = &value["data"]["repository"]["pullRequest"]["reviewThreads"];
            for node in page["nodes"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
            {
                let first = &node["comments"]["nodes"][0];
                let ours = first["author"]["__typename"] == "Bot";
                let text = first["body"].as_str().unwrap_or_default();
                let parsed = parse_marker(text).filter(|_| ours);
                if let (Some(marker), Some(id)) = (parsed, node["id"].as_str()) {
                    threads.push(Thread {
                        id: id.to_string(),
                        resolved: node["isResolved"].as_bool().unwrap_or(false),
                        semantic: marker.semantic,
                        fingerprint: marker.fingerprint,
                        rule: marker.rule,
                        path: node["path"].as_str().map(String::from),
                        comment: first["databaseId"].as_u64().unwrap_or_default(),
                        body: text.to_string(),
                    });
                }
            }
            if page["pageInfo"]["hasNextPage"] != true {
                return Ok(threads);
            }
            after = page["pageInfo"]["endCursor"].clone();
        }
    }

    fn post_one(&self, item: &Item) -> Result<(), Failure> {
        let path = format!("pulls/{}/comments", self.number);
        let on_line = item.line.map(|line| {
            json!({ "body": body(item), "commit_id": self.head, "path": item.path, "line": line, "side": "RIGHT" })
        });
        if let Some(Ok(_)) = on_line.map(|comment| self.rest("POST", &path, Some(&comment))) {
            return Ok(());
        }
        let on_file = json!({ "body": body(item), "commit_id": self.head, "path": item.path, "subject_type": "file" });
        self.rest("POST", &path, Some(&on_file)).map(|_| ())
    }

    fn post(&self, items: &[&Item]) -> Vec<usize> {
        let with_line: Vec<&&Item> = items.iter().filter(|item| item.line.is_some()).collect();
        let all_have_lines = with_line.len() == items.len() && !items.is_empty();
        if all_have_lines {
            let comments: Vec<Value> = items
                .iter()
                .map(|item| json!({ "path": item.path, "line": item.line, "side": "RIGHT", "body": body(item) }))
                .collect();
            let review =
                json!({ "commit_id": self.head, "event": "COMMENT", "comments": comments });
            if self
                .rest(
                    "POST",
                    &format!("pulls/{}/reviews", self.number),
                    Some(&review),
                )
                .is_ok()
            {
                return Vec::new();
            }
        }
        let failed = items
            .iter()
            .enumerate()
            .filter(|(_, item)| self.post_one(item).is_err());
        failed.map(|(index, _)| index).collect()
    }

    fn summary(&self, items: &[&Item]) -> Result<(), Failure> {
        let path = format!("issues/{}/comments?per_page=100", self.number);
        let existing = self.rest("GET", &path, None)?;
        let ours = existing.as_array().and_then(|comments| {
            comments.iter().find(|comment| {
                comment["user"]["type"] == "Bot"
                    && comment["body"]
                        .as_str()
                        .is_some_and(|body| body.contains(SUMMARY_MARKER))
            })
        });
        let id = ours.and_then(|comment| comment["id"].as_u64());
        if items.is_empty() {
            return match id {
                Some(id) => self
                    .rest("DELETE", &format!("issues/comments/{id}"), None)
                    .map(|_| ()),
                None => Ok(()),
            };
        }
        let mut text = String::from("### Undercurrent findings that are not tied to a line\n\n");
        for item in items {
            let place = item
                .path
                .as_deref()
                .map(|path| format!(" in `{path}`"))
                .unwrap_or_default();
            text.push_str(&format!(
                "**{}**{place}\n\n{}\n\n```\n{}\n```\n\n",
                item.title,
                item.prose,
                item.detail.replace("```", "'''")
            ));
        }
        text.push_str(SUMMARY_MARKER);
        let comment = json!({ "body": text });
        match id {
            Some(id) => self
                .rest("PATCH", &format!("issues/comments/{id}"), Some(&comment))
                .map(|_| ()),
            None => self
                .rest(
                    "POST",
                    &format!("issues/{}/comments", self.number),
                    Some(&comment),
                )
                .map(|_| ()),
        }
    }
}

fn explain(action: &str, failure: Failure) -> String {
    let detail = match failure {
        Failure::Rejected { code, body } => format!("HTTP {code}: {body}"),
        Failure::Unavailable(detail) => detail,
    };
    format!("could not {action} ({detail}); give the job `pull-requests: write` permission, which pull requests from forks cannot have")
}

pub fn sync(report: &Report) -> Result<Vec<String>> {
    let github = GitHub::from_env()?;
    let mut problems = Vec::new();
    let threads = match github.threads() {
        Ok(threads) => threads,
        Err(failure) => {
            return Ok(vec![explain(
                "read the pull request's review threads",
                failure,
            )])
        }
    };
    let root = Git::discover().ok().map(|git| git.root().to_path_buf());
    let lingering: HashSet<String> = threads
        .iter()
        .filter(|thread| flagged_line_remains(thread, root.as_deref()))
        .map(|thread| thread.id.clone())
        .collect();
    let plan = plan(report, &threads, &lingering);
    let to_post: Vec<&Item> = plan
        .post
        .iter()
        .map(|index| &report.items[*index])
        .collect();
    let unposted = github.post(&to_post);
    let mut loose: Vec<&Item> = report
        .items
        .iter()
        .filter(|item| item.path.is_none())
        .collect();
    loose.extend(unposted.iter().map(|index| to_post[*index]));
    for id in &plan.resolve {
        if github.query(RESOLVE, json!({ "thread": id })).is_ok() {
            continue;
        }
        let Some(thread) = threads.iter().find(|thread| &thread.id == id) else {
            continue;
        };
        let edit = json!({ "body": closed(thread, &github.head) });
        if let Err(failure) = github.rest(
            "PATCH",
            &format!("pulls/comments/{}", thread.comment),
            Some(&edit),
        ) {
            problems.push(explain(
                "mark a fixed finding's comment as resolved",
                failure,
            ));
        }
    }
    for id in &plan.reopen {
        if let Err(failure) = github.query(REOPEN, json!({ "thread": id })) {
            problems.push(explain("reopen a thread whose finding came back", failure));
        }
    }
    if let Err(failure) = github.summary(&loose) {
        problems.push(explain("update the summary comment", failure));
    }
    println!(
        "{} new comment(s), {} resolved, {} reopened, {} in the summary comment, {} already posted.",
        to_post.len() - unposted.len(),
        plan.resolve.len(),
        plan.reopen.len(),
        loose.len(),
        report.items.len() - to_post.len() - report.items.iter().filter(|item| item.path.is_none()).count()
    );
    problems.dedup();
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(fingerprint: &str, semantic: bool, path: Option<&str>) -> Item {
        Item {
            fingerprint: fingerprint.to_string(),
            semantic,
            path: path.map(String::from),
            line: Some(3),
            title: "T".to_string(),
            rule_id: "r".to_string(),
            rule_file: "src/UNDERCURRENT.md".to_string(),
            prose: "P".to_string(),
            detail: "quoted\nwhy\nFix: do this".to_string(),
        }
    }

    fn thread(id: &str, fingerprint: &str, semantic: bool, resolved: bool) -> Thread {
        Thread {
            id: id.to_string(),
            resolved,
            semantic,
            fingerprint: fingerprint.to_string(),
            ..Thread::default()
        }
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    #[test]
    fn markers_round_trip_through_a_comment_body() {
        let finding = item("abc123", true, Some("a.rs"));
        let rule = Some(("r".to_string(), "src/UNDERCURRENT.md".to_string()));
        let expected = Marker {
            semantic: true,
            fingerprint: "abc123".to_string(),
            rule,
        };
        assert_eq!(parse_marker(&body(&finding)), Some(expected));
        let legacy = Marker {
            semantic: false,
            fingerprint: "abc".to_string(),
            rule: None,
        };
        assert_eq!(
            parse_marker("x <!-- undercurrent:commands:abc -->"),
            Some(legacy)
        );
        assert_eq!(parse_marker("an ordinary comment"), None);
    }

    #[test]
    fn a_closed_comment_is_no_longer_recognised() {
        let open = Thread {
            body: body(&item("abc", false, Some("a.rs"))),
            ..thread("T", "abc", false, false)
        };
        let edited = closed(&open, "0123456789");
        assert!(edited.starts_with("**Resolved:** this finding no longer appears as of 0123456."));
        assert_eq!(parse_marker(&edited), None);
    }

    #[test]
    fn known_findings_are_not_posted_again_and_fixed_ones_are_resolved() {
        let report = Report {
            commands_complete: true,
            semantic_complete: true,
            items: vec![
                item("still", true, Some("a.rs")),
                item("new", false, Some("b.rs")),
                item("loose", false, None),
            ],
        };
        let threads = [
            thread("T1", "still", true, false),
            thread("T2", "fixed", false, false),
            thread("T3", "old", true, true),
        ];
        let expected = Plan {
            post: vec![1],
            resolve: vec!["T2".to_string()],
            reopen: vec![],
        };
        assert_eq!(plan(&report, &threads, &none()), expected);
    }

    #[test]
    fn threads_stay_open_when_their_kind_was_not_judged_and_reopen_when_a_finding_returns() {
        let report = Report {
            commands_complete: true,
            semantic_complete: false,
            items: vec![item("back", false, Some("a.rs"))],
        };
        let threads = [
            thread("T1", "unjudged", true, false),
            thread("T2", "back", false, true),
        ];
        let expected = Plan {
            post: vec![],
            resolve: vec![],
            reopen: vec!["T2".to_string()],
        };
        assert_eq!(plan(&report, &threads, &none()), expected);
    }

    #[test]
    fn a_semantic_thread_stays_open_while_its_flagged_line_is_still_in_the_file() {
        let report = Report {
            commands_complete: true,
            semantic_complete: true,
            items: vec![],
        };
        let threads = [
            thread("T1", "unreported", true, false),
            thread("T2", "gone", true, false),
            thread("T3", "command", false, false),
        ];
        let lingering: HashSet<String> = ["T1".to_string(), "T3".to_string()].into_iter().collect();
        let expected = Plan {
            post: vec![],
            resolve: vec!["T2".to_string(), "T3".to_string()],
            reopen: vec![],
        };
        assert_eq!(plan(&report, &threads, &lingering), expected);
    }

    #[test]
    fn the_flagged_line_is_found_by_its_fingerprint() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let rule = ("r".to_string(), "UNDERCURRENT.md".to_string());
        let fingerprint =
            findings::line_fingerprint(&rule.0, &rule.1, "Cargo.toml", "edition = \"2021\"");
        let found = Thread {
            rule: Some(rule.clone()),
            path: Some("Cargo.toml".to_string()),
            ..thread("T", &fingerprint, true, false)
        };
        assert!(flagged_line_remains(&found, Some(root)));
        assert!(flagged_line_remains(&found, None));
        let missing = Thread {
            rule: Some(rule),
            path: Some("Cargo.toml".to_string()),
            ..thread("T", "other", true, false)
        };
        assert!(!flagged_line_remains(&missing, Some(root)));
        assert!(!flagged_line_remains(
            &thread("T", "legacy", true, false),
            Some(root)
        ));
    }
}
