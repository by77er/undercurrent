fn file_var_at(bytes: &[u8], at: usize) -> Option<usize> {
    let rest = &bytes[at..];
    if rest.starts_with(b"${FILE}") {
        return Some(7);
    }
    let ends = rest
        .get(5)
        .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
    (rest.starts_with(b"$FILE") && ends).then_some(5)
}

pub fn mentions_file(script: &str) -> bool {
    let bytes = script.as_bytes();
    (0..bytes.len()).any(|i| bytes[i] == b'$' && file_var_at(bytes, i).is_some())
}

#[derive(Default)]
struct Frame {
    double: bool,
    parens: usize,
}

pub fn unquoted_file_use(script: &str) -> bool {
    let bytes = script.as_bytes();
    let mut frames = vec![Frame::default()];
    let mut single = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let top = frames.len() - 1;
        let double = frames[top].double;
        if single {
            single = c != b'\'';
            i += 1;
            continue;
        }
        match c {
            b'\\' => i += 1,
            b'\'' if !double => single = true,
            b'"' => frames[top].double = !double,
            b'#' if !double && (i == 0 || bytes[i - 1].is_ascii_whitespace()) => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'$' if bytes.get(i + 1) == Some(&b'(') => {
                frames.push(Frame::default());
                i += 1;
            }
            b'$' => {
                if let Some(len) = file_var_at(bytes, i) {
                    if !double {
                        return true;
                    }
                    i += len - 1;
                }
            }
            b'(' if !double => frames[top].parens += 1,
            b')' if !double && frames[top].parens > 0 => frames[top].parens -= 1,
            b')' if !double && top > 0 => {
                frames.pop();
            }
            _ => {}
        }
        i += 1;
    }
    false
}

pub fn problems(script: &str) -> Vec<String> {
    let mut found = Vec::new();
    if !mentions_file(script) {
        return found;
    }
    if unquoted_file_use(script) {
        found.push("write `\"$FILE\"` in double quotes; unquoted, a filename can split into extra arguments".to_string());
    }
    let words: Vec<&str> = script
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
        .collect();
    if words.contains(&"eval") {
        found.push(
            "do not use `eval` in a rule that reads `$FILE`; it would execute the filename"
                .to_string(),
        );
    }
    let reshell = words
        .windows(2)
        .any(|w| matches!(w[0], "sh" | "bash" | "zsh") && w[1] == "-c");
    if reshell {
        found.push(
            "do not pass `$FILE` through `sh -c`; the inner shell would evaluate the filename"
                .to_string(),
        );
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_uses_are_accepted() {
        assert!(!unquoted_file_use(r#"test "$(wc -l < "$FILE")" -le 400"#));
        assert!(!unquoted_file_use(
            r#"case "$FILE" in ./git.rs) exit 0 ;; esac; grep -Hn x "${FILE}""#
        ));
        assert!(!unquoted_file_use("echo '$FILE' # it's $FILE"));
    }

    #[test]
    fn unquoted_uses_are_caught() {
        assert!(unquoted_file_use("wc -l $FILE"));
        assert!(unquoted_file_use(r#"test "$(wc -l < $FILE)" -le 400"#));
        assert!(unquoted_file_use(r#"echo "a" ${FILE}"#));
    }

    #[test]
    fn file_variable_is_matched_exactly() {
        assert!(mentions_file(r#"cat "$FILE""#));
        assert!(!mentions_file(r#"cat "$FILES" "$FILENAME""#));
    }

    #[test]
    fn re_evaluation_is_rejected() {
        assert_eq!(problems(r#"eval "cat $FILE""#).len(), 1);
        assert_eq!(problems(r#"sh -c "cat $FILE""#).len(), 1);
        assert!(problems("cargo clippy").is_empty());
    }
}
