use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Git {
    root: PathBuf,
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

impl Git {
    pub fn discover() -> Result<Git> {
        let out = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .context("could not run git; is it installed?")?;
        if !out.status.success() {
            bail!("not inside a git repository; run uc from within one");
        }
        Ok(Git {
            root: PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let out = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .output()?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    fn exists(&self, reference: &str) -> bool {
        self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ])
        .is_ok()
    }

    pub fn prefix(&self) -> Result<String> {
        let out = Command::new("git")
            .args(["rev-parse", "--show-prefix"])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn resolve_base(&self, requested: Option<&str>) -> Result<String> {
        if let Some(base) = requested {
            if !self.exists(base) {
                bail!("base `{base}` is not a known commit; fetch it first (in CI, check out with full history)");
            }
            return Ok(base.to_string());
        }
        let head = self.run(&["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]);
        let head = head.map(|out| String::from_utf8_lossy(&out).trim().to_string());
        let candidates = head
            .into_iter()
            .chain(["origin/main", "origin/master", "main", "master"].map(String::from));
        for candidate in candidates {
            if self.exists(&candidate) {
                return Ok(candidate);
            }
        }
        bail!("could not find a base branch; pass --base <ref>, or --all to check every file")
    }

    pub fn merge_base(&self, base: &str) -> Result<String> {
        let merge_base = self.run(&["merge-base", base, "HEAD"]).with_context(|| {
            format!(
                "no common history between `{base}` and HEAD; in CI, check out with full history"
            )
        })?;
        Ok(String::from_utf8_lossy(&merge_base).trim().to_string())
    }

    pub fn diff(&self, merge_base: &str, path: &str, context: u32) -> Result<String> {
        let unified = format!("--unified={context}");
        let out = self.run(&["diff", "--no-color", &unified, merge_base, "--", path])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub fn git_dir(&self) -> Result<PathBuf> {
        let out = self.run(&["rev-parse", "--absolute-git-dir"])?;
        Ok(PathBuf::from(String::from_utf8_lossy(&out).trim()))
    }

    pub fn changed(&self, merge_base: &str) -> Result<Vec<String>> {
        let mut files = split_nul(&self.run(&[
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=d",
            merge_base,
            "--",
        ])?);
        files.extend(split_nul(&self.run(&[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ])?));
        Ok(self.on_disk(files))
    }

    pub fn touched(&self, merge_base: &str) -> Result<Vec<String>> {
        let mut files = split_nul(&self.run(&[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            merge_base,
            "--",
        ])?);
        files.extend(split_nul(&self.run(&[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
        ])?));
        files.sort();
        files.dedup();
        Ok(files)
    }

    pub fn all_files(&self) -> Result<Vec<String>> {
        let files = split_nul(&self.run(&[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])?);
        Ok(self.on_disk(files))
    }

    fn on_disk(&self, mut files: Vec<String>) -> Vec<String> {
        files.retain(|file| self.root.join(file).is_file());
        files.sort();
        files.dedup();
        files
    }

    pub fn tree_files(&self, reference: &str) -> Result<Vec<String>> {
        Ok(split_nul(&self.run(&[
            "ls-tree",
            "-r",
            "-z",
            "--name-only",
            reference,
        ])?))
    }

    pub fn show(&self, reference: &str, path: &str) -> Result<String> {
        let out = self.run(&["show", &format!("{reference}:{path}")])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    pub fn generated(&self, paths: &[String]) -> Result<HashSet<String>> {
        let mut child = Command::new("git")
            .current_dir(&self.root)
            .args(["check-attr", "-z", "--stdin", "linguist-generated"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().context("git check-attr has no stdin")?;
        let input: Vec<u8> = paths.iter().flat_map(|p| p.bytes().chain([0])).collect();
        let writer = std::thread::spawn(move || stdin.write_all(&input));
        let out = child.wait_with_output()?;
        writer.join().ok();
        let fields = split_nul(&out.stdout);
        let generated = fields
            .chunks(3)
            .filter(|chunk| chunk.len() == 3 && matches!(chunk[2].as_str(), "set" | "true"))
            .map(|chunk| chunk[0].clone());
        Ok(generated.collect())
    }
}
