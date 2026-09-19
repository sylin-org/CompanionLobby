//! The command-line edge: one local intake for scripts, operators and humans. It owns
//! argument parsing, rendering and exit codes, and makes no product decision — every call
//! crosses the same hub, journal and completion path an MCP client's call does. Setup and
//! stewardship (enrollment, checks, companion management) also live here, never in the
//! model-facing catalog.

use std::io::{BufRead, Write};
use std::process::exit;
use std::sync::Arc;

use serde_json::{json, Value};

use companion_lobby::adapters::lockfile;
use companion_lobby::adapters::mcp;
use companion_lobby::adapters::manager;
use companion_lobby::adapters::poller;
use companion_lobby::application::hub::ConnectorHub;
use companion_core::domain::events::DomainEvent;
use companion_core::domain::companion::CallerId;
use companion_core::domain::intake::IntakeChannel;
use companion_lobby::{build_hub, data_directory};

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 1;
const EXIT_PENDING: i32 = 2;
const EXIT_BLOCKED: i32 = 3;
const EXIT_FAILED: i32 = 4;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = run(&arguments);
    exit(code);
}

fn run(arguments: &[String]) -> i32 {
    let Some(command) = arguments.first() else {
        usage();
        return EXIT_USAGE;
    };
    let rest = &arguments[1..];
    match command.as_str() {
        "serve" => serve(rest),
        "manager" => manager::manager(rest),
        "call" => call(rest),
        "catalog" => catalog(rest),
        "companions" => companions(rest),
        "enrollments" => enrollments(rest),
        "check" => check(rest),
        "forget" => forget(rest),
        "--help" | "-h" | "help" => {
            usage();
            EXIT_OK
        }
        other => {
            eprintln!("unknown command '{other}'");
            usage();
            EXIT_USAGE
        }
    }
}

fn usage() {
    eprintln!(
        "companion-lobby — the personal local MCP connector for Tangent\n\
         \n\
         serve [--force]                MCP stdio server (the agent-facing intake);\n\
                                        also hosts the loopback companion manager in-process\n\
                                        (its URL goes to stderr, never stdout)\n\
         manager [--port N] [--force]\n\
                                        local operator web page + tray (companions,\n\
                                        atproto sign-in via the /bind route, enrollments,\n\
                                        status). Fixed default port 5219 (stable URL);\n\
                                        COMPANION_LOBBY_PORT or --port overrides\n\
         call <tool> [json] [--view V]  invoke one participation tool through the same hub\n\
         call --stdin [--json]          read `<tool> <json>` lines from standard input\n\
         catalog [--json]               list the tool catalog\n\
         companions [--json]            list local companions\n\
         enrollments [--json]           list enrollments (companion + server sessions)\n\
         check [--name N]               run one background digest check (no model)\n\
         forget --name N                remove an enrollment and its stored session\n\
         \n\
         Environment: COMPANION_LOBBY_HOME (state directory);\n\
         COMPANION_LOBBY_PORT (companion manager port; default 5219);\n\
         COMPANION_LOBBY_NO_BROWSER=1 (never open a browser);\n\
         COMPANION_LOBBY_AUTHSERVER (atproto authorization server; default bsky.social)."
    );
}

// ---------- the MCP intake ----------

fn serve(rest: &[String]) -> i32 {
    let force = rest.iter().any(|argument| argument == "--force");
    if !force && !rest.is_empty() {
        eprintln!("serve takes no options besides --force");
        return EXIT_USAGE;
    }
    let data_dir = data_directory();
    // Long-running verbs are mutually exclusive per data directory: whole-file state
    // saves from two processes would clobber each other.
    let _lock = match lockfile::DataDirLock::acquire(&data_dir, force) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    // The SAME loopback manager server the manager verb hosts, in-process — on the
    // same fixed port (5219, or the one COMPANION_LOBBY_PORT names).
    // Its URL goes to stderr and the diagnostics journal — NEVER stdout, which is
    // protocol-owned JSON-RPC and nothing else.
    let port = match companion_lobby::adapters::manager::resolve_manager_port(None) {
        Ok(port) => port,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    let listener = match companion_lobby::adapters::manager::bind_listener(&data_dir, port) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    let url = format!("http://127.0.0.1:{port}/");
    eprintln!("Companion Lobby connector companion manager: {url}");
    eprintln!("The connector records it in its state so any Connect can pop this page.");
    // The hub is constructed when the initialize request names the connecting client;
    // clientInfo.name becomes the caller (attribution + feed labeling, never a domain
    // input). One process still serves exactly one client. The manager server joins
    // at that moment, sharing the one hub (one state store, one lock).
    let build_data_dir = data_dir.clone();
    let build = move |client_name: &str| -> Result<Arc<ConnectorHub>, String> {
        let hub = build_hub(CallerId(format!("mcp:{client_name}")), build_data_dir.clone())?;
        // The URL enters the diagnostics journal (the operator's recovery path once
        // stderr has scrolled away) and durable state (so a Connect in any process —
        // the CLI one-shots included — can pop this page); no model-visible surface.
        hub.events().publish(DomainEvent::ManagerPageReady { url: url.clone() });
        hub.announce_manager_page(&url);
        let manager_hub = hub.clone();
        std::thread::Builder::new()
            .name("companion-manager".into())
            .spawn(move || manager::serve(listener, manager_hub))
            .expect("manager server thread");
        let (auto, poll_seconds) = {
            let store = hub.store().lock().expect("state lock");
            let auto = store.enrollments().iter().filter(|entry| entry.auto_check).map(|entry| entry.enrollment_id.clone()).collect();
            let poll_seconds = store.policy().poll_seconds;
            (auto, poll_seconds)
        };
        poller::spawn_checkers(hub.clone(), auto, poll_seconds);
        Ok(hub)
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let code = mcp::serve(build, &mut out);
    // Clean shutdown (stdin ended): the companion manager died with this process, so the
    // recorded URL must not point a later Connect at a dead port. The reachability
    // probe would catch it too, but honest state beats a probe.
    if let Ok(mut store) = companion_lobby::adapters::store::StateStore::open(&data_dir) {
        store.clear_manager_page_url();
        let _ = store.save();
    }
    // stdin ended: the lock releases on drop as the process winds down.
    code
}

// ---------- the CLI intake ----------

struct CallOptions {
    view: Option<String>,
    json: bool,
    stdin: bool,
    positional: Vec<String>,
}

fn parse_call(rest: &[String]) -> Result<CallOptions, String> {
    let mut options = CallOptions { view: None, json: false, stdin: false, positional: Vec::new() };
    let mut remaining = rest.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--json" => options.json = true,
            "--stdin" => options.stdin = true,
            "--view" => {
                options.view = Some(remaining.next().ok_or("--view needs a value")?.clone());
            }
            other if other.starts_with("--view=") => options.view = Some(other["--view=".len()..].to_string()),
            other if other.starts_with("--") => return Err(format!("unknown option {other}")),
            other => options.positional.push(other.to_owned()),
        }
    }
    Ok(options)
}

fn call(rest: &[String]) -> i32 {
    let options = match parse_call(rest) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_USAGE;
        }
    };
    let hub = match build_hub(CallerId("cli".into()), data_directory()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("cannot open connector state: {error}");
            return EXIT_FAILED;
        }
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if options.stdin {
        let stdin = std::io::stdin();
        let mut worst = EXIT_OK;
        for line in stdin.lock().lines() {
            let Ok(line) = line else { return EXIT_USAGE };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (tool, arguments) = line.split_once(char::is_whitespace).unwrap_or((line, "{}"));
            let code = invoke_once(&hub, tool, arguments, &options, &mut out);
            if code != EXIT_OK {
                worst = code;
            }
        }
        return worst;
    }
    match options.positional.as_slice() {
        [tool] => invoke_once(&hub, tool, "{}", &options, &mut out),
        [tool, input] => invoke_once(&hub, tool, input, &options, &mut out),
        [] => {
            eprintln!("call needs a tool name");
            EXIT_USAGE
        }
        _ => {
            eprintln!("one tool and at most one JSON input per call");
            EXIT_USAGE
        }
    }
}

fn invoke_once(hub: &Arc<ConnectorHub>, tool: &str, input: &str, options: &CallOptions, out: &mut impl Write) -> i32 {
    let arguments: Value = match serde_json::from_str(input) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("the input is not valid JSON");
            return EXIT_USAGE;
        }
    };
    let mut arguments = arguments;
    if let Some(view) = &options.view {
        arguments["view"] = json!(view);
    }
    let outcome = hub.invoke(IntakeChannel::Cli, tool, &arguments);
    if options.json {
        let _ = writeln!(out, "{}", serde_json::to_string_pretty(&outcome.structured).unwrap_or_default());
    } else {
        let _ = writeln!(out, "{}", outcome.text);
    }
    match outcome.status.as_str() {
        "ok" if !outcome.is_error => EXIT_OK,
        "ok" => EXIT_BLOCKED,
        "pending" => EXIT_PENDING,
        "blocked" => EXIT_BLOCKED,
        _ => EXIT_FAILED,
    }
}

fn catalog(rest: &[String]) -> i32 {
    let json = rest.iter().any(|argument| argument == "--json");
    let tools = mcp::catalog();
    let Some(tools) = tools.as_array() else { return EXIT_FAILED };
    if json {
        println!("{}", serde_json::to_string_pretty(&tools).unwrap_or_default());
    } else {
        for tool in tools {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
            let description = tool.get("description").and_then(Value::as_str).unwrap_or_default();
            println!("{name}\n    {description}");
        }
    }
    EXIT_OK
}

// ---------- setup and stewardship ----------

fn companions(rest: &[String]) -> i32 {
    let json = rest.iter().any(|argument| argument == "--json");
    let hub = match build_hub(CallerId("cli".into()), data_directory()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("cannot open connector state: {error}");
            return EXIT_FAILED;
        }
    };
    let entries: Vec<(Value, usize)> = {
        let store = hub.store().lock().expect("state lock");
        store
            .companions()
            .iter()
            .map(|companion| {
                let enrollment_count = store.enrollments_of(&companion.local_id).len();
                (
                    json!({
                        "localId": companion.local_id,
                        "handle": companion.handle,
                        "displayName": companion.display_name,
                        "boundDid": companion.bound_did,
                    }),
                    enrollment_count,
                )
            })
            .collect()
    };
    if json {
        let plain: Vec<Value> = entries.into_iter().map(|(value, _)| value).collect();
        println!("{}", serde_json::to_string_pretty(&plain).unwrap_or_default());
    } else {
        if entries.is_empty() {
            println!("No companions exist yet. Create one in the companion manager (companion-lobby manager).");
        }
        for (companion, count) in &entries {
            println!(
                "{}\n    {} · {} enrollment(s)",
                companion.get("handle").and_then(Value::as_str).unwrap_or_default(),
                companion.get("localId").and_then(Value::as_str).unwrap_or_default(),
                count
            );
        }
    }
    EXIT_OK
}

fn enrollments(rest: &[String]) -> i32 {
    let json = rest.iter().any(|argument| argument == "--json");
    let hub = match build_hub(CallerId("cli".into()), data_directory()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    let store = hub.store().lock().expect("state lock");
    let companions: Vec<Value> = store
        .enrollments()
        .iter()
        .map(|entry| {
            json!({
                "enrollmentId": entry.enrollment_id,
                "companionId": entry.local_id,
                "name": entry.name,
                "participantRef": entry.participant_ref,
                "did": entry.did,
                "displayName": entry.display_name,
                "handle": entry.handle,
                "server": entry.origin,
                "autoCheck": entry.auto_check,
            })
        })
        .collect();
    drop(store);
    if json {
        println!("{}", serde_json::to_string_pretty(&companions).unwrap_or_default());
    } else {
        if companions.is_empty() {
            println!("No companions are enrolled. Use Connect (or the companion manager) to enroll one.");
        }
        for entry in &companions {
            println!(
                "{}\n    companion {} · {} · {} · auto-check {}",
                entry.get("name").and_then(Value::as_str).unwrap_or_default(),
                entry.get("companionId").and_then(Value::as_str).unwrap_or_default(),
                entry.get("participantRef").and_then(Value::as_str).unwrap_or_default(),
                entry.get("server").and_then(Value::as_str).unwrap_or_default(),
                entry.get("autoCheck").and_then(Value::as_bool).unwrap_or_default(),
            );
        }
    }
    EXIT_OK
}

fn check(rest: &[String]) -> i32 {
    let mut name = None;
    let mut remaining = rest.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--name" => name = remaining.next().cloned(),
            other => {
                eprintln!("unknown check option {other}");
                return EXIT_USAGE;
            }
        }
    }
    let hub = match build_hub(CallerId("cli".into()), data_directory()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    let targets: Vec<(String, String)> = {
        let store = hub.store().lock().expect("state lock");
        store
            .enrollments()
            .iter()
            .filter(|entry| name.as_deref().is_none_or(|value| entry.matches(value) || entry.enrollment_id == value))
            .map(|entry| (entry.enrollment_id.clone(), entry.name.clone()))
            .collect()
    };
    if targets.is_empty() {
        eprintln!("no matching enrolled companion");
        return EXIT_USAGE;
    }
    let mut worst = EXIT_OK;
    for (enrollment_id, name) in targets {
        match hub.background_check(&enrollment_id) {
            Ok(summary) => println!("{name}: {summary}"),
            Err(error) => {
                eprintln!("{name}: {error}");
                worst = EXIT_FAILED;
            }
        }
    }
    worst
}

fn forget(rest: &[String]) -> i32 {
    let mut name = None;
    let mut remaining = rest.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--name" => name = remaining.next().cloned(),
            other => {
                eprintln!("unknown forget option {other}");
                return EXIT_USAGE;
            }
        }
    }
    let Some(name) = name else {
        eprintln!("forget requires --name");
        return EXIT_USAGE;
    };
    let data_dir = data_directory();
    let hub = match build_hub(CallerId("cli".into()), data_dir.clone()) {
        Ok(hub) => hub,
        Err(error) => {
            eprintln!("{error}");
            return EXIT_FAILED;
        }
    };
    // The same hub path every intake forgets through: journaled, attributed, and
    // announced on the event bus like any other mutation.
    let enrollment_id = {
        let store = hub.store().lock().expect("state lock");
        match store.find_enrollment(&name) {
            Some(entry) => entry.enrollment_id.clone(),
            None => {
                eprintln!("no companion matches '{name}'");
                return EXIT_USAGE;
            }
        }
    };
    match hub.forget_enrollment(&enrollment_id) {
        Ok(_) => {
            println!("Forgot the connection and its stored session.");
            EXIT_OK
        }
        Err(error) => {
            eprintln!("{error}");
            EXIT_FAILED
        }
    }
}
