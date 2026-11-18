use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

const MAX_READ_LINES: usize = 400;
const MAX_MATCHES: usize = 60;
const MAX_LISTED: usize = 200;
const MAX_SEARCH_BYTES: u64 = 1_000_000;

pub struct Workspace<'a> {
    pub root: &'a Path,
    pub files: &'a BTreeSet<String>,
}

pub fn definitions() -> Value {
    json!([
        {
            "name": "read_file",
            "description": "Read a file in the repository, with line numbers. Returns at most 400 lines per call; pass start_line to read further into a long file.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path from the repository root" },
                    "start_line": { "type": "integer", "description": "First line to return; defaults to 1" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "search",
            "description": "Find lines containing an exact piece of text across the repository. Case sensitive, no patterns. Returns path:line: text for up to 60 matches.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Exact text to look for" },
                    "under": { "type": "string", "description": "Only search paths starting with this prefix; empty for the whole repository" }
                },
                "required": ["text"]
            }
        },
        {
            "name": "list_files",
            "description": "List repository files whose path starts with a prefix. Returns up to 200 paths.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "under": { "type": "string", "description": "Path prefix; empty for the whole repository" }
                },
                "required": ["under"]
            }
        }
    ])
}

impl Workspace<'_> {
    fn read(&self, path: &str) -> Result<String, String> {
        let path = path.trim_start_matches("./");
        if !self.files.contains(path) {
            return Err(format!(
                "{path} is not a file in this repository; use list_files to find the path"
            ));
        }
        let full = self.root.join(path);
        let inside = full
            .canonicalize()
            .ok()
            .zip(self.root.canonicalize().ok())
            .is_some_and(|(full, root)| full.starts_with(root));
        if !inside {
            return Err(format!(
                "{path} points outside the repository and cannot be read"
            ));
        }
        std::fs::read_to_string(full).map_err(|_| format!("{path} is not readable as text"))
    }

    fn read_file(&self, input: &Value) -> Result<String, String> {
        let path = input["path"]
            .as_str()
            .ok_or("read_file needs a `path` string")?;
        let text = self.read(path)?;
        let lines: Vec<&str> = text.lines().collect();
        let start = input["start_line"].as_u64().unwrap_or(1).max(1) as usize;
        let shown = lines
            .iter()
            .enumerate()
            .skip(start - 1)
            .take(MAX_READ_LINES);
        let mut out: String = shown
            .map(|(index, line)| format!("{:>5}| {line}\n", index + 1))
            .collect();
        let end = (start - 1 + MAX_READ_LINES).min(lines.len());
        out.push_str(&format!("(lines {start}-{end} of {})\n", lines.len()));
        Ok(out)
    }

    fn search(&self, input: &Value) -> Result<String, String> {
        let text = input["text"]
            .as_str()
            .filter(|text| !text.is_empty())
            .ok_or("search needs non-empty `text`")?;
        let under = input["under"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("./");
        let mut matches = Vec::new();
        for path in self.files.iter().filter(|path| path.starts_with(under)) {
            let small = std::fs::metadata(self.root.join(path))
                .is_ok_and(|meta| meta.len() <= MAX_SEARCH_BYTES);
            let Some(body) = small.then(|| self.read(path).ok()).flatten() else {
                continue;
            };
            for (index, line) in body
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(text))
            {
                matches.push(format!("{path}:{}: {}", index + 1, line.trim()));
                if matches.len() == MAX_MATCHES {
                    matches.push(format!(
                        "(stopped at {MAX_MATCHES} matches; narrow the search with `under`)"
                    ));
                    return Ok(matches.join("\n"));
                }
            }
        }
        if matches.is_empty() {
            return Ok(format!("no line contains {text:?}"));
        }
        Ok(matches.join("\n"))
    }

    fn list_files(&self, input: &Value) -> Result<String, String> {
        let under = input["under"]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches("./");
        let found: Vec<&str> = self
            .files
            .iter()
            .filter(|path| path.starts_with(under))
            .map(String::as_str)
            .collect();
        if found.is_empty() {
            return Ok(format!("no file path starts with {under:?}"));
        }
        let mut out = found
            .iter()
            .take(MAX_LISTED)
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        if found.len() > MAX_LISTED {
            out.push_str(&format!(
                "\n({} more; use a longer prefix)",
                found.len() - MAX_LISTED
            ));
        }
        Ok(out)
    }

    pub fn execute(&self, name: &str, input: &Value) -> Result<String, String> {
        match name {
            "read_file" => self.read_file(input),
            "search" => self.search(input),
            "list_files" => self.list_files(input),
            _ => Err(format!(
                "there is no tool named {name}; use read_file, search or list_files"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> BTreeSet<String> {
        ["Cargo.toml", "src/main.rs"]
            .map(String::from)
            .into_iter()
            .collect()
    }

    #[test]
    fn reads_and_searches_listed_files() {
        let files = files();
        let workspace = Workspace {
            root: Path::new(env!("CARGO_MANIFEST_DIR")),
            files: &files,
        };
        let read = workspace.execute("read_file", &json!({ "path": "Cargo.toml" }));
        assert!(read.is_ok_and(|text| text.contains("undercurrent")));
        let found = workspace.execute("search", &json!({ "text": "fn main()", "under": "src" }));
        assert!(found.is_ok_and(|text| text.starts_with("src/main.rs:")));
    }

    #[test]
    fn refuses_paths_outside_the_file_list() {
        let files = files();
        let workspace = Workspace {
            root: Path::new(env!("CARGO_MANIFEST_DIR")),
            files: &files,
        };
        assert!(workspace
            .execute("read_file", &json!({ "path": "../../etc/passwd" }))
            .is_err());
        assert!(workspace
            .execute("read_file", &json!({ "path": ".git/config" }))
            .is_err());
        assert!(workspace
            .execute("read_file", &json!({ "path": "/etc/passwd" }))
            .is_err());
    }
}
