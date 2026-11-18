use crate::api::{self, Failure};
use crate::findings::{Item, Report};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

const MARKER: &str = "<!-- undercurrent:";
const SUMMARY_MARKER: &str = "<!-- undercurrent:summary -->";
const THREADS: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{id isResolved comments(first:1){nodes{body author{__typename}}}}}}}}";
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

#[derive(Debug, PartialEq)]
pub struct Thread {
    pub id: String,
    pub resolved: bool,
    pub semantic: bool,
    pub fingerprint: String,
}

#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub post: Vec<usize>,
    pub resolve: Vec<String>,
    pub reopen: Vec<String>,
}

fn marker(item: &Item) -> String {
    format!(
        "{MARKER}{}:{} -->",
        if item.semantic {
            "semantic"
        } else {
            "commands"
        },
        item.fingerprint
    )
}

fn parse_marker(body: &str) -> Option<(bool, String)> {
    let rest = &body[body.find(MARKER)? + MARKER.len()..];
    let (kind, rest) = rest.split_once(':')?;
    let fingerprint = rest.split_once(" -->")?.0;
    let semantic = match kind {
        "semantic" => true,
        "commands" => false,
        _ => return None,
    };
    Some((semantic, fingerprint.to_string()))
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

pub fn plan(report: &Report, threads: &[Thread]) -> Plan {
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
        if present && thread.resolved {
            plan.reopen.push(thread.id.clone());
        } else if !present && !thread.resolved && judged {
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
        let event: Value = serde_json::from_str(&std::fs::read_to_string(&event_path)?).unwrap();
        let pull = &event["pull_request"];
        let (Some(number), Some(head)) = (pull["number"].as_u64(), pull["head"]["sha"].as_str())
        else {
            bail!("not a pull request");
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
                let parsed = first["body"]
                    .as_str()
                    .and_then(parse_marker)
                    .filter(|_| ours);
                if let (Some((semantic, fingerprint)), Some(id)) = (parsed, node["id"].as_str()) {
                    let resolved = node["isResolved"].as_bool().unwrap_or(false);
                    threads.push(Thread {
                        id: id.to_string(),
                        resolved,
                        semantic,
                        fingerprint,
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
    let plan = plan(report, &threads);
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
    for (mutation, ids, action) in [
        (RESOLVE, &plan.resolve, "resolve a fixed finding's thread"),
        (
            REOPEN,
            &plan.reopen,
            "reopen a thread whose finding came back",
        ),
    ] {
        for id in ids {
            if let Err(failure) = github.query(mutation, json!({ "thread": id })) {
                problems.push(explain(action, failure));
            }
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
            rule_file: "UNDERCURRENT.md".to_string(),
            prose: "P".to_string(),
            detail: "D".to_string(),
        }
    }

    fn thread(id: &str, fingerprint: &str, semantic: bool, resolved: bool) -> Thread {
        Thread {
            id: id.to_string(),
            resolved,
            semantic,
            fingerprint: fingerprint.to_string(),
        }
    }

    #[test]
    fn markers_round_trip_through_a_comment_body() {
        let finding = item("abc123", true, Some("a.rs"));
        assert_eq!(
            parse_marker(&body(&finding)),
            Some((true, "abc123".to_string()))
        );
        assert_eq!(parse_marker("an ordinary comment"), None);
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
        assert_eq!(plan(&report, &threads), expected);
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
        assert_eq!(plan(&report, &threads), expected);
    }
}
