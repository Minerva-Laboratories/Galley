use std::net::SocketAddr;
use std::path::PathBuf;

mod doctor;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use galley_server::auth::Role;
use galley_server::{AppState, Config, ServeOptions};

#[derive(Parser)]
#[command(name = "galley", version, about = "Write together. Compile anywhere.")]
struct Cli {
    /// Path to galley.toml (default: ~/.galley/galley.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override the data directory from the config file
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the server
    Serve {
        #[arg(long)]
        bind: Option<SocketAddr>,
        #[arg(long)]
        port: Option<u16>,
        /// Development mode: expect the Vite dev server to proxy here
        #[arg(long)]
        dev: bool,
    },
    /// Manage projects
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Manage accounts
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
    },
    /// Manage compile engines
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    /// Print the structure of a project: sections, labels, figures, citations
    Map {
        /// Project id (see `galley project list`)
        project: String,
        /// Rank the map around a label, citation key, section title or file
        #[arg(long, default_value = "")]
        focus: String,
        /// Roughly how many tokens the map may use
        #[arg(long, default_value_t = 1500)]
        budget: usize,
    },
    /// Find the passages of a project that discuss something
    Find {
        /// Project id (see `galley project list`)
        project: String,
        /// What to look for, in words
        query: String,
        #[arg(long, default_value_t = 5)]
        limit: usize,
        #[arg(long, default_value_t = 1200)]
        budget: usize,
        /// Show the passages themselves, not one line each
        #[arg(long)]
        detail: bool,
    },
    /// Search the project's formulas by structure, or survey them with no query
    Math {
        /// Project id (see `galley project list`)
        project: String,
        /// An expression, e.g. 'a^{*}(s) = -B^{+}(s - s^{*})'. Omit it to survey the mathematics
        #[arg(default_value = "")]
        query: String,
        #[arg(long, default_value_t = 8)]
        limit: usize,
        /// Show the full source of each hit
        #[arg(long)]
        detail: bool,
    },
    /// Check a project's bibliography: missing, duplicate, uncited and incomplete entries
    Bib {
        /// Project id (see `galley project list`)
        project: String,
    },
    /// Look the bibliography up in open catalogues: fill entries in, and find work you may be missing
    Lit {
        /// Project id (see `galley project list`)
        project: String,
        /// Turn the lookups on for this project first. They are off by default
        #[arg(long)]
        enable: bool,
        /// How many candidates to show
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Check that everything Galley needs is in place
    Doctor,
    /// Write a default galley.toml
    Init,
    /// Run a model on your own machine against a project. It needs no TeX. Compiles stay on the server
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    /// Check a project out, let a local model edit the copy, and submit the changes as suggestions
    Run {
        /// The project's MCP URL from the Share panel, e.g. https://host/mcp/<project-id>
        #[arg(long)]
        url: String,
        /// A device token from the Share panel (or GALLEY_TOKEN)
        #[arg(long, env = "GALLEY_TOKEN", hide_env_values = true)]
        token: String,
        /// Built-in task: fix-build, proofread, tighten, cite, table, reviewer, explain
        #[arg(long, default_value = "fix-build")]
        prompt: String,
        /// Task arguments as key=value, e.g. --arg path=main.tex --arg target=15%
        #[arg(long = "arg")]
        args: Vec<String>,
        /// OpenAI-compatible endpoint (Ollama, LM Studio, llama.cpp, vLLM, a hosted provider)
        #[arg(long, default_value = "http://localhost:11434/v1")]
        endpoint: String,
        /// Model name as the endpoint knows it
        #[arg(long, default_value = "qwen2.5-coder:14b")]
        model: String,
        /// API key for the endpoint, if it needs one (or GALLEY_MODEL_KEY)
        #[arg(long, env = "GALLEY_MODEL_KEY", hide_env_values = true)]
        api_key: Option<String>,
        /// Give up after this many model turns
        #[arg(long, default_value_t = 12)]
        max_turns: usize,
        /// Where to place the checkout (default: a fresh temporary directory, removed afterwards)
        #[arg(long)]
        workdir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Create a blank project owned by an account
    New {
        name: String,
        /// Email of the owner (default: the sole admin)
        #[arg(long)]
        owner: Option<String>,
    },
    /// List all projects on the server
    List,
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Create an account
    CreateUser {
        email: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        password: String,
        /// Grant server-admin rights
        #[arg(long)]
        admin: bool,
    },
    /// Reset an account's password
    ResetPassword {
        email: String,
        #[arg(long)]
        password: String,
    },
    /// List accounts
    List,
}

#[derive(Subcommand)]
enum EngineCommand {
    /// Download an engine into the data directory
    Install {
        #[arg(default_value = "tectonic")]
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    // The runner is a client. It needs no config, data directory, or server-side state.
    let command = match cli.command {
        Command::Agent { command } => return run_agent(command).await,
        other => other,
    };

    let config_path = cli.config.clone().unwrap_or_else(Config::default_path);
    let config = Config::load(&config_path)?;
    let data_dir = cli.data_dir.clone().unwrap_or_else(|| config.data_dir());
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("could not create data directory {}", data_dir.display()))?;

    match command {
        Command::Serve { bind, port, dev } => {
            let mut addr: SocketAddr = bind
                .map(Ok)
                .unwrap_or_else(|| config.server.bind.parse())
                .with_context(|| format!("invalid bind address {}", config.server.bind))?;
            if let Some(p) = port {
                addr.set_port(p);
            }
            let state = AppState::new(&data_dir, &config).await?;
            galley_server::serve(state, ServeOptions { bind: addr, dev })
                .await
                .with_context(|| format!("could not listen on {addr}. Is another Galley running? Try --port."))?;
        }
        Command::Project { command } => {
            let state = AppState::new(&data_dir, &config).await?;
            match command {
                ProjectCommand::New { name, owner } => {
                    let owner_id = resolve_owner(&state, owner.as_deref())?;
                    let meta = state.registry.create(&name).await?;
                    state.store.set_member(&meta.id, &owner_id, Role::Admin)?;
                    println!("Created {} ({})", meta.name, meta.id);
                }
                ProjectCommand::List => {
                    let list = state.registry.list()?;
                    if list.is_empty() {
                        println!("No projects yet. Create one with: galley project new <name> --owner <email>");
                    }
                    for p in list {
                        println!("{:<24} {}  edited {}", p.id, p.name, p.updated_at.format("%Y-%m-%d %H:%M"));
                    }
                }
            }
        }
        Command::Admin { command } => {
            let state = AppState::new(&data_dir, &config).await?;
            match command {
                AdminCommand::CreateUser { email, name, password, admin } => {
                    let user = state.store.create_user(&email, &name, &password, admin)?;
                    println!("Created {} ({}){}", user.name, user.email, if admin { " — admin" } else { "" });
                }
                AdminCommand::ResetPassword { email, password } => {
                    let user = state
                        .store
                        .user_by_email(&email)?
                        .with_context(|| format!("no account for {email}"))?;
                    state.store.reset_password(&user.id, &password)?;
                    println!("Password reset for {}", user.email);
                }
                AdminCommand::List => {
                    for u in state.store.list_users()? {
                        println!("{:<28} {}{}", u.email, u.name, if u.is_admin { "  (admin)" } else { "" });
                    }
                }
            }
        }
        Command::Engine { command } => match command {
            EngineCommand::Install { name } => {
                anyhow::ensure!(name == "tectonic", "only the tectonic engine can be installed; TeX Live is configured through [build] texlive_path");
                let dir = galley_build::engine::Tectonic::install_dir(&data_dir);
                let progress = |m: String| println!("{m}");
                let path = galley_build::install::install_tectonic(&dir, &progress).await?;
                println!("Installed {}", path.display());
            }
        },
        Command::Agent { .. } => unreachable!("handled before config is loaded"),
        Command::Lit { project, enable, limit } => {
            let state = AppState::new(&data_dir, &config).await?;
            let mut meta = state.registry.meta(&project)?;
            if enable && !meta.literature {
                meta = state.registry.update_meta(&project, |m| m.literature = true)?;
                println!("Catalogue lookups are on for {project}. Citation keys, DOIs and titles are sent to");
                println!("Crossref and OpenAlex; the document itself never is.\n");
            }
            if !meta.literature {
                bail!(
                    "Catalogue lookups are off for {project}. They send citation keys, DOIs and titles to \
                     Crossref and OpenAlex (never the document). Turn them on with: galley lit {project} --enable"
                );
            }
            let p = state.registry.open(&project).await?;
            p.flush_now().await?;
            let paper = galley_index::scan(p.workdir(), &meta.main_file);
            let contact = (!config.server.contact_email.is_empty()).then(|| config.server.contact_email.clone());
            let client = galley_lit::Client::new(&p.workdir().join(".galley").join("lit"), contact);

            println!("Filling in entries…");
            let (suggestions, mut problems) = galley_lit::enrich(&client, &paper).await;
            print!("{}", galley_lit::enrich_report(&suggestions, &problems));
            println!("\nLooking for work you may be missing…");
            let (candidates, from, more) = galley_lit::coverage(&client, &paper, limit).await;
            problems.extend(more);
            print!("{}", galley_lit::coverage_report(&candidates, from, &problems));
            println!("\n{} request(s) to the catalogues; answers are cached for 30 days.", client.requests_made());
        }
        Command::Math { project, query, limit, detail } => {
            let state = AppState::new(&data_dir, &config).await?;
            let meta = state.registry.meta(&project)?;
            let p = state.registry.open(&project).await?;
            let paper = galley_index::scan(p.workdir(), &meta.main_file);
            let formulas = galley_index::math::formulas(p.workdir(), &paper);
            if query.trim().is_empty() {
                print!("{}", galley_index::math::render_overview(&paper, &formulas));
            } else {
                let hits = galley_index::math::search(&formulas, &query, limit);
                print!("{}", galley_index::math::render_matches(&paper, &formulas, &query, &hits, detail));
            }
        }
        Command::Bib { project } => {
            let state = AppState::new(&data_dir, &config).await?;
            let meta = state.registry.meta(&project)?;
            let p = state.registry.open(&project).await?;
            let paper = galley_index::scan(p.workdir(), &meta.main_file);
            let findings = galley_index::bib::audit(&paper);
            print!("{}", galley_index::bib::render(&paper, &findings));
        }
        Command::Find { project, query, limit, budget, detail } => {
            let state = AppState::new(&data_dir, &config).await?;
            let meta = state.registry.meta(&project)?;
            let p = state.registry.open(&project).await?;
            let paper = galley_index::scan(p.workdir(), &meta.main_file);
            let hits = galley_index::search(&paper, &query, limit);
            print!("{}", galley_index::render_hits(&paper, &query, &hits, budget, detail));
        }
        Command::Map { project, focus, budget } => {
            let state = AppState::new(&data_dir, &config).await?;
            let meta = state.registry.meta(&project)?;
            let p = state.registry.open(&project).await?;
            let paper = galley_index::scan(p.workdir(), &meta.main_file);
            let ranked = galley_index::rank(&paper, &galley_index::Focus::parse(&focus));
            print!("{}", galley_index::render(&paper, &ranked, budget));
        }
        Command::Doctor => {
            let checks = doctor::run(&config, &config_path, &data_dir).await;
            let code = doctor::report(&checks);
            if code != 0 {
                std::process::exit(code);
            }
        }
        Command::Init => {
            if config_path.exists() {
                println!("{} already exists; leaving it alone.", config_path.display());
            } else {
                if let Some(parent) = config_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&config_path, Config::default_toml())?;
                println!("Wrote {}", config_path.display());
            }
        }
    }
    Ok(())
}

fn resolve_owner(state: &AppState, email: Option<&str>) -> Result<String> {
    match email {
        Some(e) => Ok(state
            .store
            .user_by_email(e)?
            .with_context(|| format!("no account for {e}; create it with: galley admin create-user"))?
            .id),
        None => {
            let admins = state.store.list_admins()?;
            match admins.as_slice() {
                [one] => Ok(one.id.clone()),
                [] => bail!("no accounts yet. Create one with: galley admin create-user <email> --name … --password …"),
                _ => bail!("more than one admin exists; pass --owner <email>"),
            }
        }
    }
}

async fn run_agent(command: AgentCommand) -> Result<()> {
    let AgentCommand::Run { url, token, prompt, args, endpoint, model, api_key, max_turns, workdir } = command;
    let mut prompt_args = serde_json::Map::new();
    for a in &args {
        let Some((k, v)) = a.split_once('=') else { bail!("--arg expects key=value, got {a:?}") };
        prompt_args.insert(k.trim().to_string(), serde_json::Value::String(v.to_string()));
    }
    let temp = match &workdir {
        Some(_) => None,
        None => Some(tempfile::tempdir().context("could not create a temporary checkout directory")?),
    };
    let dir = workdir.clone().unwrap_or_else(|| temp.as_ref().unwrap().path().to_path_buf());
    let opts = galley_agents::RunOptions {
        mcp: galley_agents::McpClient::new(&url, &token),
        chat: galley_agents::ChatBackend::new(&endpoint, &model, api_key),
        prompt: prompt.clone(),
        prompt_args: serde_json::Value::Object(prompt_args),
        max_turns,
        workdir: dir,
    };
    println!("{prompt} with {model} at {endpoint}");
    let report = galley_agents::run(opts, |line| println!("  {line}")).await?;
    println!();
    if !report.summary.trim().is_empty() {
        println!("{}", report.summary.trim());
        println!();
    }
    for (path, msg) in &report.proposals {
        println!("{path}: {msg}");
    }
    for (path, why) in &report.refused {
        println!("{path}: not proposed. {why}");
    }
    if report.proposals.is_empty() && report.refused.is_empty() {
        println!("No files changed after {} turn(s); nothing was proposed.", report.turns);
    }
    Ok(())
}
