This directory judges semantic rules by calling a model. The model reads
code written by the author of the pull request, who may try to steer it.

## A model's output is reported only after it is checked

Nothing a model returns is added to the report without a check. Code that
parses a model's reply may hand the parsed values back to its caller
unchecked; the rule is broken only where unchecked values are pushed into
the report's findings.

Text written by a generative model is kept only when its quoted evidence is
found on a changed line of a changed file, and the line number reported is
the one where the evidence was found, not the one the model claimed.

A decision model returns only a probability for a hunk. Without a second
reviewer it blocks only when that probability reaches the threshold fitted
to the rule's own examples; anything else is printed as a possible
violation.

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
