mod check;
mod git;
mod report;
mod resolve;
mod rules;
#[cfg(test)]
mod rules_tests;
mod run;
mod shell;
mod validate;

use anyhow::Result;
use clap::{Parser, Subcommand};
use resolve::Source;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "uc",
    version,
    about = "Directory-scoped lint rules for agent-written code"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the rules against changed files; exits 1 on any violation
    Check {
        /// Branch or commit the change is measured against
        #[arg(long)]
        base: Option<String>,
        /// Check every file in the repository, not only changed ones
        #[arg(long)]
        all: bool,
        /// Load rule files from the base instead of the working tree, so a
        /// change cannot weaken the rules it is judged by
        #[arg(long)]
        rules_from_base: bool,
    },
    /// List the rules that govern a path, and where each comes from
    Explain { path: String },
    /// Check every rule file in the working tree for mistakes
    Validate,
}

fn check(options: &check::Options) -> Result<bool> {
    let outcome = check::check(options)?;
    print!("{}", report::text(&outcome));
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        print!("{}", report::annotations(&outcome));
        report::write_step_summary(&outcome);
    }
    Ok(outcome.findings.is_empty())
}

fn explain(path: &str) -> Result<bool> {
    let git = git::Git::discover()?;
    let path = format!("{}{}", git.prefix()?, path.trim_start_matches("./"));
    let mut count = 0;
    for file in resolve::load(&git, &Source::WorkTree)? {
        for rule in file
            .parsed
            .rules
            .iter()
            .filter(|rule| file.applies(rule, &path))
        {
            let kind = if rule.is_semantic() {
                "semantic"
            } else {
                "command"
            };
            println!(
                "{}:{}: {} [{kind}, id {}]",
                file.path, rule.line, rule.title, rule.id
            );
            count += 1;
        }
    }
    println!("{count} rule(s) govern {path}");
    Ok(true)
}

fn validate() -> Result<bool> {
    let git = git::Git::discover()?;
    let files = resolve::load(&git, &Source::WorkTree)?;
    let mut errors = 0;
    let mut rules = 0;
    for file in &files {
        rules += file.parsed.rules.len();
        for problem in validate::problems(&file.parsed) {
            let level = if problem.warning { "warning" } else { "error" };
            println!(
                "{}:{}: {level}: {}",
                file.path, problem.line, problem.message
            );
            errors += usize::from(!problem.warning);
        }
    }
    println!(
        "{} rule file(s), {rules} rule(s), {errors} error(s)",
        files.len()
    );
    Ok(errors == 0)
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Commands::Check {
            base,
            all,
            rules_from_base,
        } => check(&check::Options {
            base,
            all,
            rules_from_base,
        }),
        Commands::Explain { path } => explain(&path),
        Commands::Validate => validate(),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("uc: {err:#}");
            ExitCode::from(2)
        }
    }
}
