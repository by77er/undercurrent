mod api;
mod check;
mod git;
mod pool;
mod report;
mod resolve;
mod rules;
#[cfg(test)]
mod rules_tests;
mod run;
mod semantic;
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
    #[command(about = "Run the rules against changed files; exits 1 on any violation")]
    Check {
        #[arg(long, help = "Branch or commit the change is measured against")]
        base: Option<String>,
        #[arg(
            long,
            help = "Check every file in the repository, not only changed ones"
        )]
        all: bool,
        #[arg(
            long,
            help = "Load rule files from the base instead of the working tree, so a change cannot weaken the rules it is judged by"
        )]
        rules_from_base: bool,
        #[arg(
            long,
            help = "Do not run command rules; use where code from the change must not execute"
        )]
        skip_commands: bool,
        #[arg(long, help = "Do not judge semantic rules")]
        skip_semantic: bool,
        #[arg(
            long,
            help = "Fail if semantic rules apply and no model API key is set"
        )]
        require_semantic: bool,
        #[arg(
            long,
            help = "Also require CLAUDE.md, AGENTS.md and skill files to change in pull requests of their own, like rule files"
        )]
        separate_instructions: bool,
    },
    #[command(about = "List the rules that govern a path, and where each comes from")]
    Explain { path: String },
    #[command(about = "Check every rule file in the working tree for mistakes")]
    Validate,
    #[command(
        about = "Score each semantic rule's good and bad examples and fit its blocking threshold"
    )]
    Test,
}

fn check(options: &check::Options) -> Result<bool> {
    let outcome = check::check(options)?;
    print!("{}", report::text(&outcome));
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        print!("{}", report::annotations(&outcome));
        report::write_step_summary(&outcome);
    }
    let failed_to_run = outcome
        .semantic
        .as_ref()
        .is_some_and(|semantic| !semantic.errors.is_empty());
    if failed_to_run {
        anyhow::bail!("some semantic rules could not be judged, so the check is incomplete; see the errors above");
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

fn probabilities(values: &[f64]) -> String {
    values
        .iter()
        .map(|value| format!("{value:.2}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn test() -> Result<bool> {
    let git = git::Git::discover()?;
    let files = resolve::load(&git, &Source::WorkTree)?;
    let fits = semantic::test(&git, &files)?;
    let mut failed = 0;
    for fit in &fits {
        println!("{}: {}", fit.rule_file, fit.title);
        println!(
            "  violating examples scored {}",
            probabilities(&fit.violating)
        );
        println!(
            "  complying examples scored {}",
            probabilities(&fit.complying)
        );
        match (fit.threshold(), fit.problem()) {
            (Some(threshold), _) => println!("  blocks at {threshold:.2}"),
            (None, problem) => {
                failed += 1;
                println!("  error: {}", problem.unwrap_or_default());
            }
        }
    }
    let semantic = files
        .iter()
        .flat_map(|file| &file.parsed.rules)
        .filter(|rule| rule.is_semantic())
        .count();
    println!(
        "{} of {semantic} semantic rule(s) have a fitted threshold in {}; the rest only block when a second reviewer confirms",
        fits.len() - failed,
        semantic::THRESHOLDS_FILE
    );
    Ok(failed == 0)
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Commands::Check {
            base,
            all,
            rules_from_base,
            skip_commands,
            skip_semantic,
            require_semantic,
            separate_instructions,
        } => check(&check::Options {
            base,
            all,
            rules_from_base,
            skip_commands,
            skip_semantic,
            require_semantic,
            separate_instructions,
        }),
        Commands::Explain { path } => explain(&path),
        Commands::Validate => validate(),
        Commands::Test => test(),
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
