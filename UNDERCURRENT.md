Undercurrent is a lint tool that pushes back on agent-written code. Rules live
in `UNDERCURRENT.md` files; a rule governs the directory its file is in and
everything below. The tool is a single Rust binary, `uc`.

## Rust is formatted with rustfmt
only: *.rs

Run `cargo fmt` and commit the result.

```sh
rustfmt --check --edition 2021 "$FILE"
```

## Clippy passes with warnings denied

Fix what clippy reports instead of adding `#[allow]`.

```sh
cargo clippy --quiet --all-targets -- -D warnings
```

## The tests pass

```sh
cargo test --quiet
```

A failing test means the change is wrong or the test is out of date; fix
whichever it is. Never delete or ignore a test to get past this rule.

## Source files stay small
only: *.rs

A file over 1000 lines is doing more than one job. Split it into modules along
the seams between those jobs.

```sh
test "$(wc -l < "$FILE")" -le 1000
```

## Rule files are valid
only: UNDERCURRENT.md

```sh
cargo run --quiet -- validate
```

`uc validate` prints each problem with its line number.

## Code carries no comments
only: *.{rs,yml,toml}

Delete the comment. If the code needs explaining, rename or restructure it
until it does not; if a constraint must be recorded, write it as a rule in
an `UNDERCURRENT.md` file.

```sh
case "$FILE" in
  *.rs) ! grep -HnE '^[[:space:]]*//|[[:space:]]//[[:space:]]|/\*' "$FILE" ;;
  *) ! grep -HnE '^[[:space:]]*#|[[:space:]]#[[:space:]]' "$FILE" ;;
esac
```

## The only prose files are instructions
only: *.md

This repository holds code and instructions for agents, nothing else. Delete
READMEs, design documents, changelogs and notes about removed code. Guidance
that agents should follow belongs in `UNDERCURRENT.md`, `CLAUDE.md`,
`AGENTS.md` or a skill.

```sh
case "$FILE" in
  */UNDERCURRENT.md | */CLAUDE.md | */AGENTS.md | */SKILL.md) exit 0 ;;
esac
exit 1
```

## The rule format never asks for what can be inferred

A rule file states only what Undercurrent cannot work out for itself. Scope
comes from the file's position in the directory tree, the kind of rule from
whether it has an `sh` block, and the id from the heading. A change that adds
a metadata key, or any syntax a rule author must learn, has to justify why
the same information could not be inferred.

## Messages tell the reader what to do next

Every error, warning and finding the tool prints is read by a coding agent
that will act on it. A message names the problem and the fix, not only the
problem.

```rust bad
bail!("invalid base");
```

```rust good
bail!("base `{base}` is not a known commit; fetch it first");
```
