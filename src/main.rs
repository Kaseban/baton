use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

mod canonical;
mod config;
mod convert;
mod detect;
mod formats;

#[cfg(feature = "mcp")]
mod mcp;

use canonical::Agent;

#[derive(Parser)]
#[command(
    name = "baton",
    bin_name = "baton",
    version,
    about = "Pass the baton between coding agents — convert any session to any other, and wire itself into every agent's MCP config.",
    long_about = "baton converts coding-agent session transcripts between formats (Claude Code, OpenCode, Codex, Cursor, Continue, Cline, Zed, Aider, Gemini CLI) and registers itself as an MCP server in every installed agent."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Convert a session from one agent format to another.
    Convert {
        /// Source agent.
        #[arg(long, value_parser = parse_agent)]
        from: Agent,
        /// Target agent.
        #[arg(long, value_parser = parse_agent)]
        to: Agent,
        /// Input session file path.
        input: PathBuf,
        /// Output file path (defaults to ./<to>-<source_id>.<ext>).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// After writing, run the target agent's own import command (e.g. `opencode import`).
        #[arg(long)]
        import: bool,
    },
    /// List sessions from one or all agents.
    List {
        /// Filter to a single agent. Omit to scan all.
        #[arg(long, value_parser = parse_agent)]
        agent: Option<Agent>,
    },
    /// Register baton as an MCP server in every detected agent's config.
    Install,
    /// Verify baton is registered in each agent config.
    Doctor,
    /// Remove baton from every agent's config.
    Uninstall,
    /// Run as an MCP server (stdio transport). Called by agents automatically.
    Serve {
        /// Use HTTP transport instead of stdio.
        #[arg(long)]
        http: bool,
        #[arg(long, default_value = "127.0.0.1:7777")]
        bind: String,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Convert {
            from,
            to,
            input,
            output,
            import,
        } => {
            convert::convert(from, to, &input, output.as_deref())?;
            if import {
                let out = output
                    .as_deref()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| std::env::current_dir().unwrap().join(format!(
                        "{}-{}.json",
                        to,
                        input.file_stem().and_then(|s| s.to_str()).unwrap_or("session")
                    )));
                convert::import_to_target(to, &out)?;
            }
        }
        Cmd::List { agent } => list(agent)?,
        Cmd::Install => install()?,
        Cmd::Doctor => doctor()?,
        Cmd::Uninstall => uninstall()?,
        Cmd::Serve { http: _, bind: _ } => {
            #[cfg(feature = "mcp")]
            mcp::serve()?;
            #[cfg(not(feature = "mcp"))]
            anyhow::bail!("baton was built without the `mcp` feature; rebuild with --features mcp");
        }
    }
    Ok(())
}

fn parse_agent(s: &str) -> Result<Agent, String> {
    Agent::parse(s).ok_or_else(|| format!("unknown agent '{s}'"))
}

fn list(agent: Option<Agent>) -> anyhow::Result<()> {
    let agents = match agent {
        Some(a) => vec![a],
        None => formats::ALL_AGENTS.to_vec(),
    };
    let mut any = false;
    for a in agents {
        let refs = formats::list(a);
        if refs.is_empty() {
            continue;
        }
        any = true;
        println!("\n# {}\n", a);
        for r in refs {
            let when = chrono::DateTime::from_timestamp_millis(r.mtime)
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            println!(
                "  {id}  {when}  {path}",
                id = r.id,
                path = r.path.display()
            );
        }
    }
    if !any {
        println!("no sessions found for the requested agent(s).");
    }
    Ok(())
}

fn install() -> anyhow::Result<()> {
    let detected = detect::detect_all();
    if detected.is_empty() {
        println!("no agents detected on this system.");
        return Ok(());
    }
    let mut changed = 0;
    let mut skipped = 0;
    for d in &detected {
        match config::register(d) {
            Ok(true) => {
                changed += 1;
                println!("✓ registered baton in {} → {}", d.agent, d.config_path.display());
            }
            Ok(false) => {
                skipped += 1;
                println!("· already registered: {} ({})", d.agent, d.config_path.display());
            }
            Err(e) => {
                println!("✗ failed: {} — {e}", d.agent);
            }
        }
    }
    println!("\n{changed} registered, {skipped} already present.");
    println!("restart your agents (or run `baton doctor`) to pick up the baton MCP server.");
    Ok(())
}

fn uninstall() -> anyhow::Result<()> {
    let detected = detect::detect_all();
    let mut removed = 0;
    for d in &detected {
        match config::unregister(d) {
            Ok(true) => {
                removed += 1;
                println!("✓ removed baton from {} → {}", d.agent, d.config_path.display());
            }
            Ok(false) => {}
            Err(e) => println!("✗ failed: {} — {e}", d.agent),
        }
    }
    println!("\nremoved from {removed} configs.");
    Ok(())
}

fn doctor() -> anyhow::Result<()> {
    let detected = detect::detect_all();
    if detected.is_empty() {
        println!("no agents detected.");
        return Ok(());
    }
    println!("baton doctor — checking {n} agent config(s)\n", n = detected.len());
    let mut ok = 0;
    let mut bad = 0;
    for d in &detected {
        let registered = is_registered(d).unwrap_or(false);
        let status = if registered { "✓" } else { "✗" };
        if registered {
            ok += 1;
        } else {
            bad += 1;
        }
        println!(
            "{status} {agent:<12} {path}",
            agent = d.agent.to_string(),
            path = d.config_path.display()
        );
    }
    println!("\n{ok} ok, {bad} missing. run `baton install` to fix.");
    Ok(())
}

fn is_registered(d: &detect::DetectedAgent) -> anyhow::Result<bool> {
    let raw = std::fs::read_to_string(&d.config_path).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(false);
    }
    Ok(raw.contains(detect::SERVER_NAME))
}
