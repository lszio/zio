//! `grove` — the product binary. Every subcommand maps onto a library
//! contract; this file only parses arguments and prints results.

use std::path::PathBuf;

use grove_app::{demo, inspect, load_protocol, modular, open_store, operator, population, publisher, select, usage, Paths};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(feature = "http")]
    if args.first().map(|a| a == "serve").unwrap_or(false) {
        match run_serve(&args[1..]) {
            Ok(()) => return,
            Err(err) => {
                eprintln!("grove: {err}");
                std::process::exit(2);
            }
        }
    }
    match run(&args) {
        Ok(report) => {
            print!("{report}");
            if !report.ends_with('\n') {
                println!();
            }
        }
        Err(err) => {
            eprintln!("grove: {err}");
            std::process::exit(2);
        }
    }
}

fn run(args: &[String]) -> Result<String, grove::contracts::Error> {
    let Some(command) = args.first() else {
        return Err(usage(
            "usage: grove <demo|inspect|checkpoint|fork|resume|compare|select|publish> [options]",
        ));
    };
    const COMMANDS: &[&str] = &[
        "demo", "inspect", "checkpoint", "fork", "resume", "compare", "select", "publish",
        "serve",
    ];
    if !COMMANDS.contains(&command.as_str()) {
        return Err(usage(&format!(
            "unknown command {command:?}; try {}",
            COMMANDS.join(", ")
        )));
    }
    let opts = parse_opts(&args[1..])?;
    if command == "serve" && opts.bind.is_some() && opts.root.is_none() {
        return Err(usage("serve needs --root PATH (the store it serves)"));
    }
    let root = opts
        .root
        .ok_or_else(|| usage("--root PATH is required"))?;
    let store = open_store(&root)?;
    let paths = Paths::from_repo_root();

    match command.as_str() {
        "demo" => {
            let case = opts
                .case
                .ok_or_else(|| usage("demo needs --case dual|population|modular"))?;
            let device = opts.device.unwrap_or_else(|| "cpu".to_string());
            match case.as_str() {
                "dual" => demo::run_dual(&root, &paths, &device),
                "modular" => modular::run_modular(&root, &paths, &device),
                "population" => population::run_population(
                    &root,
                    &paths,
                    &device,
                    opts.workers.unwrap_or(2),
                ),
                other => Err(usage(&format!(
                    "unknown case {other:?}; the delivered cases are `dual`, \
                     `population` and `modular`"
                ))),
            }
        }
        "inspect" => inspect(&store),
        "checkpoint" => {
            let run = opts.run.ok_or_else(|| usage("checkpoint needs --run ID"))?;
            let state = opts
                .state
                .ok_or_else(|| usage("checkpoint needs --state PATH (a worker state file)"))?;
            let cp = grove::checkpoint::pause(
                &store,
                &operator(),
                &run,
                Path::new(&state),
                now_ms(),
            )?;
            Ok(format!(
                "checkpoint {} committed; run {run} is paused at {} steps\n",
                cp.id, cp.budget_spent_steps
            ))
        }
        "fork" => {
            let parent = opts
                .checkpoint
                .ok_or_else(|| usage("fork needs --checkpoint ID"))?;
            let id = opts.branch.ok_or_else(|| usage("fork needs --branch ID"))?;
            let quota = opts.quota.unwrap_or(100);
            let branch = grove::checkpoint::fork_branch(
                &store,
                &operator(),
                &parent,
                &id,
                "explore@1",
                quota,
            )?;
            Ok(format!(
                "branch {} forked from {parent} (head {:?}, quota {} steps)\n",
                branch.id,
                branch.head,
                branch.budget_quota
            ))
        }
        "resume" => {
            let checkpoint = opts
                .checkpoint
                .ok_or_else(|| usage("resume needs --checkpoint ID"))?;
            let run = opts.run.ok_or_else(|| usage("resume needs --run NEW-ID"))?;
            let budget = opts.quota.unwrap_or(1000);
            let plan = grove::checkpoint::resume_plan(
                &store,
                &operator(),
                &checkpoint,
                grove::contracts::ResumeLevel::LearningContinuation,
                false,
                &run,
                budget,
            )?;
            Ok(format!(
                "resume planned: new run {} continues at {} steps ({} of {budget} already spent)\n",
                plan.run.id, plan.state_step, plan.run.steps_consumed
            ))
        }
        "compare" | "select" => {
            let protocol = load_protocol(
                &store,
                opts.protocol
                    .as_deref()
                    .ok_or_else(|| usage("compare/select needs --protocol ID"))?,
            )?;
            let ids = opts
                .snapshots
                .ok_or_else(|| usage("compare/select needs --snapshots hex,hex"))?;
            let mut snapshots = Vec::new();
            for hex in ids.split(',') {
                snapshots.push(grove::contracts::ArtifactRef::parse_hex(hex)?);
            }
            select(&store, protocol, &snapshots)
        }
        "publish" => {
            let snapshot = opts
                .snapshot
                .ok_or_else(|| usage("publish needs --snapshot HEX"))?;
            let digest = grove::contracts::ArtifactRef::parse_hex(&snapshot)?;
            let protocol = load_protocol(
                &store,
                opts.protocol
                    .as_deref()
                    .ok_or_else(|| usage("publish needs --protocol ID"))?,
            )?;
            let version = grove::evaluation::publish_candidate(
                &store,
                &publisher(),
                &protocol,
                &digest,
                opts.expected_version,
            )?;
            Ok(format!("publication: v{version} active → {snapshot}\n"))
        }
        other => unreachable!("{other} is validated above"),
    }
}

struct Opts {
    root: Option<PathBuf>,
    bind: Option<String>,
    case: Option<String>,
    device: Option<String>,
    run: Option<String>,
    state: Option<String>,
    checkpoint: Option<String>,
    branch: Option<String>,
    quota: Option<u32>,
    protocol: Option<String>,
    snapshots: Option<String>,
    snapshot: Option<String>,
    workers: Option<usize>,
    expected_version: Option<u32>,
}

fn parse_opts(args: &[String]) -> Result<Opts, grove::contracts::Error> {
    let mut opts = Opts {
        root: None,
        bind: None,
        case: None,
        device: None,
        run: None,
        state: None,
        checkpoint: None,
        branch: None,
        quota: None,
        protocol: None,
        snapshots: None,
        snapshot: None,
        workers: None,
        expected_version: None,
    };
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].trim_start_matches("--");
        let value = args.get(i + 1);
        match flag {
            "root" => opts.root = value.map(PathBuf::from),
            "bind" => opts.bind = value.cloned(),
            "case" => opts.case = value.cloned(),
            "device" => opts.device = value.cloned(),
            "run" => opts.run = value.cloned(),
            "state" => opts.state = value.cloned(),
            "checkpoint" => opts.checkpoint = value.cloned(),
            "branch" => opts.branch = value.cloned(),
            "quota" => {
                opts.quota = Some(
                    value
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| usage("--quota needs a number"))?,
                );
            }
            "protocol" => opts.protocol = value.cloned(),
            "snapshots" => opts.snapshots = value.cloned(),
            "workers" => {
                opts.workers = Some(
                    value
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| usage("--workers needs a number"))?,
                );
            }
            "snapshot" => opts.snapshot = value.cloned(),
            "expected-version" => {
                opts.expected_version = Some(
                    value
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| usage("--expected-version needs a number"))?,
                );
            }
            other => return Err(usage(&format!("unknown flag --{other}"))),
        }
        i += 2;
    }
    Ok(opts)
}

/// `grove serve` — the HTTP product surface. Its own async entry point:
/// the rest of the CLI is synchronous and needs no runtime.
#[cfg(feature = "http")]
fn run_serve(args: &[String]) -> Result<(), grove::contracts::Error> {
    let opts = parse_opts(args)?;
    let root = opts
        .root
        .ok_or_else(|| usage("serve needs --root PATH (the store it serves)"))?;
    let bind: std::net::SocketAddr = opts
        .bind
        .as_deref()
        .unwrap_or("127.0.0.1:8787")
        .parse()
        .map_err(|_| usage("--bind needs HOST:PORT"))?;
    // tokens come from the environment, never from argv: a token in a
    // shell history is a leaked grant
    let mut tokens: std::collections::HashMap<String, grove::contracts::ActorRole> =
        std::collections::HashMap::new();
    for (var, role) in [
        ("GROVE_TOKEN_READER", grove::contracts::ActorRole::Reader),
        ("GROVE_TOKEN_ANNOTATOR", grove::contracts::ActorRole::Annotator),
        ("GROVE_TOKEN_OPERATOR", grove::contracts::ActorRole::Operator),
        ("GROVE_TOKEN_PUBLISHER", grove::contracts::ActorRole::Publisher),
    ] {
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                tokens.insert(value, role);
            }
        }
    }
    grove_app::api::check_bind(bind, tokens.len())?;
    let store = open_store(&root)?;
    let state = grove_app::api::ApiState::new(std::sync::Arc::new(store), tokens);
    let app = grove_app::api::router(state);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| usage(&format!("no tokio runtime: {e}")))?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| usage(&format!("cannot bind {bind}: {e}")))?;
        eprintln!("grove serving on http://{bind}");
        axum::serve(listener, app)
            .await
            .map_err(|e| usage(&format!("server stopped: {e}")))
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

use std::path::Path;
