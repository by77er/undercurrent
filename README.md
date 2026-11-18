# Undercurrent

Lint rules that push back on agent-written code. Instructions in `CLAUDE.md`
or `AGENTS.md` ask an agent to work a certain way; Undercurrent checks the
result on every pull request and blocks what breaks the rules.

## Rule files

Rules live in `UNDERCURRENT.md` files. A rule governs the directory its file
is in and everything below, so general rules sit at the root and specific
ones sit deeper.

````markdown
Handlers translate HTTP into calls on the domain layer.

## Handlers don't query the database directly

Go through a repository, so query logic stays in one place.

## Files stay under 400 lines
only: *.ts

```sh
test "$(wc -l < "$FILE")" -le 400
```
````

- A rule is a `## heading` and prose. The prose is what the agent sees when
  the rule fails, so it should say how to fix the problem.
- A rule with an `sh` block is a **command rule**. A non-zero exit is a
  violation. Lines printed as `path:line: message` become findings on those
  lines.
- A command that mentions `$FILE` runs once per changed file. Any other
  command runs once, in the rule file's directory, when a file under it
  changed.
- A rule without an `sh` block is a **semantic rule**, to be judged by a
  model. These are parsed and listed but **not judged yet**.
- `only: <glob>` directly under the heading narrows a rule to matching file
  names. `id: <slug>` pins the rule's id if you rename the heading.
- Code blocks tagged `good` or `bad` are examples for the semantic reviewer.
- Text above the first heading is background for every rule in the file.
- Every rule blocks. Files marked `linguist-generated` in `.gitattributes`
  are skipped.

## Commands

```sh
uc check                    # files changed since the base branch
uc check --all              # every file
uc check --rules-from-base  # judge by the base branch's rules, as CI does
uc explain src/main.rs      # which rules govern a path
uc validate                 # check the rule files themselves
```

## GitHub Action

```yaml
on: pull_request
permissions:
  contents: read
jobs:
  undercurrent:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: by77er/undercurrent@main
```

Rule files are loaded from the base branch by default, so a pull request
cannot weaken the rules it is judged by. Rule commands can still execute
code from the pull request (linter configs, build scripts), so run this job
without secrets and with a read-only token.

This repository checks itself with its own rules; see `UNDERCURRENT.md`,
`src/UNDERCURRENT.md` and `.github/workflows/undercurrent.yml`.
