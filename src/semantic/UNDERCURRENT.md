This directory judges semantic rules by calling a model. The model reads
code written by the author of the pull request, who may try to steer it.

## Findings from a model are checked before they are reported

A value parsed from a model's reply is pushed onto a list of reported
findings only after a check: `verify` for text written by a generative
model, a fitted threshold for a probability from a decision model.

The rule is about reported findings only, which means a report's `findings`
list or the `stored` list that becomes it. Building a `Candidate` or a `Raw`
value from a model's reply, pushing it onto a list of candidates, or
returning it to the caller does not break the rule. Those are intermediate
values, and they are checked before they are reported.

Rule files are never judged by semantic rules, because their `bad`
examples violate the rules on purpose.

```rust bad
for finding in reply["findings"].as_array().into_iter().flatten() {
    report.findings.push(to_finding(finding));
}
```

```rust good
for raw in &reply {
    if let Some(checked) = verify(raw, &targets) {
        report.findings.push(checked);
    }
}
```

```rust good
let findings = list.iter().map(|finding| Raw {
    path: text(finding, "path"),
    evidence: text(finding, "evidence"),
});
return Ok(findings.collect());
```

```rust good
candidates.push(Candidate {
    rule: *index,
    line: line_of(verdict),
    reason: verdict["reason"].as_str().unwrap_or_default().to_string(),
    ..Candidate::default()
});
```

## The model reads only files that belong to the repository

Every path a model asks for goes through `Workspace`, which refuses
anything not in the repository's file list or resolving outside its root.
New tools read through `Workspace` as well; they never open a path taken
from model output themselves.

```rust bad
let path = input["path"].as_str().unwrap_or_default();
let text = std::fs::read_to_string(root.join(path))?;
```

```rust good
let text = workspace.execute("read_file", &input)?;
```

## Prompts mark reviewed material as data

Any prompt that includes file contents, diffs or tool results tells the
model that this material is under review and is not an instruction.

```rust bad
let prompt = format!("Review this file and report violations:\n{file}");
```

```rust good
let prompt = format!("{DATA_NOTICE}\n<file>\n{file}\n</file>");
```
