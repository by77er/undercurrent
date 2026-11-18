## Git is called only from git.rs
only: *.rs

Add a method to `Git` in `git.rs` rather than spawning git elsewhere, so the
way paths are passed to git stays in one place.

```sh
case "$FILE" in ./git.rs) exit 0 ;; esac
! grep -Hn 'Command::new("git")' "$FILE"
```

## No unwrap or expect outside tests
only: *.rs

Return the error with `?` and context, or handle the missing case. A panic
in CI tells the agent nothing about what to fix.

```sh
! awk '/#\[cfg\(test\)\]/ { exit } /\.unwrap\(\)|\.expect\(/ { print FILENAME ":" FNR ": " $0; bad = 1 } END { exit !bad }' "$FILE"
```

## Nothing from the change under review is built into a command

File paths, file contents and anything else a pull request author controls
reach a child process only as a separate argument or an environment
variable. They are never formatted into a shell string or a git revision
expression, because the author could then run code in the check.
