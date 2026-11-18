use crate::rules::{parse, slug, Example};

const SAMPLE: &str = "\
Handlers translate HTTP into domain calls.

## Handlers don't query the database directly

Go through a repository.

```ts bad
db.query(\"select 1\");
```

```ts
// an ordinary block stays in the prose
## not a heading
```

## No default exports
only: *.ts

Use named exports.

```sh
! grep -Hn 'export default' \"$FILE\"
```
";

#[test]
fn parses_context_rules_and_blocks() {
    let parsed = parse(SAMPLE);
    assert!(parsed.problems.is_empty());
    assert_eq!(parsed.context, "Handlers translate HTTP into domain calls.");
    assert_eq!(parsed.rules.len(), 2);

    let first = &parsed.rules[0];
    assert_eq!(first.id, "handlers-don-t-query-the-database-directly");
    assert!(first.is_semantic());
    assert!(first.prose.contains("## not a heading"));
    let expected = Example {
        good: false,
        lang: "ts".to_string(),
        code: "db.query(\"select 1\");".to_string(),
    };
    assert_eq!(first.examples, vec![expected]);

    let second = &parsed.rules[1];
    assert_eq!(second.only.as_deref(), Some("*.ts"));
    assert_eq!(second.prose, "Use named exports.");
    assert!(second.per_file());
}

#[test]
fn metadata_must_touch_the_heading() {
    let parsed = parse("## A rule\n\nnote: this is prose, not a key.\n");
    assert!(parsed.problems.is_empty());
    assert_eq!(parsed.rules[0].prose, "note: this is prose, not a key.");
}

#[test]
fn explicit_id_overrides_the_slug() {
    let parsed = parse("## Renamed heading\nid: old-name\n\nProse.\n");
    assert_eq!(parsed.rules[0].id, "old-name");
    assert_eq!(slug("One handler per file!"), "one-handler-per-file");
}

#[test]
fn command_outside_a_rule_is_an_error() {
    let parsed = parse("```sh\ntrue\n```\n");
    assert_eq!(parsed.problems.len(), 1);
}
