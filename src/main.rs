use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use scratchpilerd::lanes::LaneSizes;
use scratchpilerd::lsp::{self, Config, transport};

const HELP: &str = "\
scratchpilerd — language server for Scratchpiler (.sdsl)

USAGE
  scratchpilerd [MODE] --project <dir> [OPTIONS]

MODES
  --stdio                 Talk LSP over stdin/stdout (default)
  --node-ipc              Talk over a Node IPC channel (NODE_CHANNEL_FD)
  --pipe=<path>           Connect to the editor's named pipe or Unix socket
  --socket=<port>         Connect to the editor's TCP port on 127.0.0.1
  daemon --listen=<at>    Run in the background; editors connect to <at>
                          (<at> is a socket path, or tcp:<port>)
  check                   Analyze once, print every problem, exit 1 on errors
  warm                    Analyze once and fill the disk cache, print nothing

OPTIONS
  --project <dir>         Project folder to serve (required, never guessed;
                          editors may pass initializationOptions.project)
  --cache-dir <dir>       Where analysis caches are kept
  --no-cache              Never read or write the disk cache
  --threads <n>           Background analysis threads   (default: cores - 1)
  --interactive <n>       Threads kept free for completion, hover and
                          navigation                    (default: 2-4)
  --idle-exit <seconds>   daemon: quit after this long with no editor
  -h, --help              Show this help
  -V, --version           Show the version

PROJECT FOLDER
  project.json or one .sb3     Sprites, variables, lists, costumes, sounds
  Stage.sdsl, <Sprite>.sdsl    One source file per sprite
  *.h, include/, headers/      Headers for #include <name.h>
  scratchpiler.json            Optional: { \"project\", \"stage\", \"sprites\", \"headers\" }

EXAMPLES
  scratchpilerd --project ~/games/platformer
  scratchpilerd daemon --listen=/tmp/scratchpilerd.sock --project ~/games/platformer
  scratchpilerd check --project .
";

#[derive(Default)]
struct Args {
    mode: Option<String>,
    pipe: Option<String>,
    socket: Option<u16>,
    listen: Option<String>,
    project: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    no_cache: bool,
    threads: Option<usize>,
    interactive: Option<usize>,
    idle_exit: Option<u64>,
    debug: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut raw = std::env::args().skip(1).peekable();
    while let Some(arg) = raw.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| inline.clone().or_else(|| raw.next()).ok_or(format!("{name} needs a value"));
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("scratchpilerd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--stdio" => args.mode = Some("stdio".into()),
            "--node-ipc" => args.mode = Some("node-ipc".into()),
            "--pipe" => args.pipe = Some(value("--pipe")?),
            "--socket" | "--port" => args.socket = Some(value("--socket")?.parse().map_err(|_| "--socket expects a port number")?),
            "--listen" => args.listen = Some(value("--listen")?),
            "--project" => args.project = Some(PathBuf::from(value("--project")?)),
            "--cache-dir" => args.cache_dir = Some(PathBuf::from(value("--cache-dir")?)),
            "--no-cache" => args.no_cache = true,
            "--threads" => args.threads = Some(value("--threads")?.parse().map_err(|_| "--threads expects a number")?),
            "--interactive" => args.interactive = Some(value("--interactive")?.parse().map_err(|_| "--interactive expects a number")?),
            "--idle-exit" => args.idle_exit = Some(value("--idle-exit")?.parse().map_err(|_| "--idle-exit expects seconds")?),
            "--clientProcessId" => {
                value("--clientProcessId")?;
            }
            "daemon" | "check" | "warm" if args.mode.is_none() => args.mode = Some(flag),
            "debug" => {
                args.debug = raw.by_ref().collect();
            }
            other => return Err(format!("unknown argument {other} (see --help)")),
        }
    }
    Ok(args)
}

fn lanes(args: &Args) -> LaneSizes {
    let mut sizes = LaneSizes::for_machine();
    if let Some(n) = args.threads {
        sizes.background = n.max(1);
    }
    if let Some(n) = args.interactive {
        sizes.interactive = n.max(1);
    }
    sizes
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("scratchpilerd: {message}");
            return ExitCode::from(2);
        }
    };
    if !args.debug.is_empty() {
        scratchpilerd::debug::run(&args.debug);
        return ExitCode::SUCCESS;
    }
    let cache_dir = if args.no_cache { None } else { args.cache_dir.clone().or_else(scratchpilerd::cache::default_dir) };
    let mode = args.mode.clone().unwrap_or_else(|| "stdio".into());

    if mode == "check" || mode == "warm" {
        let Some(project) = &args.project else {
            eprintln!("scratchpilerd: {mode} needs --project <dir>");
            return ExitCode::from(2);
        };
        return match scratchpilerd::batch::analyze_project(project, cache_dir.as_deref(), lanes(&args), mode == "check") {
            Ok(report) => {
                eprintln!(
                    "scratchpilerd: {} files ({} from cache) in {} ms — {} errors, {} warnings",
                    report.files, report.cached, report.millis, report.errors, report.warnings
                );
                if mode == "check" && report.errors > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
            }
            Err(e) => {
                eprintln!("scratchpilerd: {e}");
                ExitCode::from(2)
            }
        };
    }

    let config = Config {
        project: args.project.clone(),
        cache_dir: args.cache_dir.clone(),
        no_cache: args.no_cache,
        lanes: lanes(&args),
        daemon: mode == "daemon",
        idle_exit: args.idle_exit.map(Duration::from_secs),
    };
    let result = if mode == "daemon" {
        let Some(at) = &args.listen else {
            eprintln!("scratchpilerd: daemon needs --listen=<socket path | tcp:port>");
            return ExitCode::from(2);
        };
        if config.project.is_none() {
            eprintln!("scratchpilerd: daemon needs --project <dir>");
            return ExitCode::from(2);
        }
        transport::Listener::bind(at).map(|listener| {
            eprintln!("scratchpilerd: listening on {at}");
            lsp::run(config, None, Some(listener))
        })
    } else {
        let connection = if let Some(pipe) = &args.pipe {
            transport::pipe(pipe)
        } else if let Some(port) = args.socket {
            transport::socket(port)
        } else if mode == "node-ipc" {
            transport::node_ipc()
        } else {
            Ok(transport::stdio())
        };
        connection.map(|c| lsp::run(config, Some(c), None))
    };
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(e) => {
            eprintln!("scratchpilerd: {e}");
            ExitCode::from(2)
        }
    }
}
