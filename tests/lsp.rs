use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const STAGE: &str = "on flag {\n    set [score] to 0\n    broadcast(\"start\")\n}\n";
const PLAYER: &str = "on receive \"start\" {\n    change [score] by 1\n    say([score])\n}\n\ndefine jump(height) {\n    changeY(height)\n}\n\non flag {\n    jump(10)\n    broadcast(\"nobody\")\n}\n";
const PROJECT: &str = r#"{"targets":[
  {"isStage":true,"name":"Stage","variables":{"v1":["score",0]},"lists":{},"broadcasts":{"b1":"start"},"blocks":{},"costumes":[{"name":"backdrop1"}],"sounds":[]},
  {"isStage":false,"name":"Player","variables":{},"lists":{},"blocks":{},"costumes":[{"name":"walk"}],"sounds":[{"name":"pop"}]}
],"monitors":[]}"#;

fn project() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("scratchpilerd-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Stage.sdsl"), STAGE).unwrap();
    std::fs::write(dir.join("Player.sdsl"), PLAYER).unwrap();
    std::fs::write(dir.join("project.json"), PROJECT).unwrap();
    dir
}

fn uri(path: &Path) -> String {
    scratchpilerd::workspace::path_to_uri(path)
}

struct Client {
    reader: Box<dyn BufRead + Send>,
    writer: Box<dyn Write + Send>,
    lines: bool,
    next: i64,
    backlog: Vec<Value>,
}

impl Client {
    fn new(reader: impl Read + Send + 'static, writer: impl Write + Send + 'static, lines: bool) -> Client {
        Client { reader: Box::new(BufReader::new(reader)), writer: Box::new(writer), lines, next: 1, backlog: Vec::new() }
    }

    fn send(&mut self, message: &Value) {
        let body = serde_json::to_vec(message).unwrap();
        if self.lines {
            self.writer.write_all(&body).unwrap();
            self.writer.write_all(b"\n").unwrap();
        } else {
            write!(self.writer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
            self.writer.write_all(&body).unwrap();
        }
        self.writer.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        if self.lines {
            let mut line = String::new();
            self.reader.read_line(&mut line).unwrap();
            return serde_json::from_str(&line).unwrap();
        }
        let mut length = 0;
        loop {
            let mut header = String::new();
            self.reader.read_line(&mut header).unwrap();
            if header.trim().is_empty() {
                break;
            }
            if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; length];
        self.reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let message = self.read();
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                let reply = json!({ "jsonrpc": "2.0", "id": message["id"], "result": null });
                self.send(&reply);
            }
            self.backlog.push(message);
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn wait_for(&mut self, test: impl Fn(&Value) -> bool) -> Value {
        if let Some(i) = self.backlog.iter().position(&test) {
            return self.backlog.remove(i);
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let message = self.read();
            if test(&message) {
                return message;
            }
            self.backlog.push(message);
        }
        panic!("timed out");
    }

    fn initialize(&mut self, options: Value) -> Value {
        let reply = self.request("initialize", json!({ "processId": null, "rootUri": null, "capabilities": {}, "initializationOptions": options }));
        self.notify("initialized", json!({}));
        reply
    }

    fn diagnostics_for(&mut self, file: &str) -> Vec<Value> {
        let message = self.wait_for(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"].as_str().is_some_and(|u| u.ends_with(file)));
        message["params"]["diagnostics"].as_array().cloned().unwrap_or_default()
    }
}

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_scratchpilerd"));
    command.arg("--no-cache").stderr(Stdio::inherit());
    command
}

fn stdio_server(args: &[&str]) -> (Child, Client) {
    let mut child = binary().args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let client = Client::new(child.stdout.take().unwrap(), child.stdin.take().unwrap(), false);
    (child, client)
}

fn exercise(client: &mut Client, dir: &Path) {
    let messages = client.diagnostics_for("Player.sdsl");
    assert!(messages.iter().any(|d| d["message"] == "Nothing receives \"nobody\""), "{messages:?}");

    let player = dir.join("Player.sdsl");
    client.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri(&player), "languageId": "scratchpiler", "version": 1, "text": PLAYER } }));
    let hover = client.request("textDocument/hover", json!({ "textDocument": { "uri": uri(&player) }, "position": { "line": 0, "character": 14 } }));
    let text = hover["result"]["contents"]["value"].as_str().unwrap();
    assert!(text.contains("**\"start\"**") && text.contains("Sent from Stage"), "{text}");

    let definition = client.request("textDocument/definition", json!({ "textDocument": { "uri": uri(&player) }, "position": { "line": 10, "character": 5 } }));
    assert_eq!(definition["result"]["range"]["start"]["line"], 5);

    let references = client.request(
        "textDocument/references",
        json!({ "textDocument": { "uri": uri(&player) }, "position": { "line": 1, "character": 12 }, "context": { "includeDeclaration": true } }),
    );
    let files: Vec<&str> = references["result"].as_array().unwrap().iter().map(|l| l["uri"].as_str().unwrap()).collect();
    assert!(files.iter().any(|f| f.ends_with("Stage.sdsl")) && files.iter().any(|f| f.ends_with("Player.sdsl")), "{files:?}");

    let completion = client.request("textDocument/completion", json!({ "textDocument": { "uri": uri(&player) }, "position": { "line": 1, "character": 12 } }));
    let labels: Vec<&str> = completion["result"]["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap()).collect();
    assert!(labels.contains(&"score]"), "{labels:?}");

    client.notify(
        "textDocument/didChange",
        json!({ "textDocument": { "uri": uri(&player), "version": 2 }, "contentChanges": [{ "range": { "start": { "line": 11, "character": 15 }, "end": { "line": 11, "character": 21 } }, "text": "start" }] }),
    );
    let updated = client.wait_for(|m| m["method"] == "textDocument/publishDiagnostics" && m["params"]["version"] == 2);
    assert!(!updated["params"]["diagnostics"].as_array().unwrap().iter().any(|d| d["message"].as_str().unwrap().contains("nobody")));

    let rename = client
        .request("textDocument/rename", json!({ "textDocument": { "uri": uri(&player) }, "position": { "line": 1, "character": 12 }, "newName": "points" }));
    let changes = rename["result"]["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 2);

    let tokens = client.request("textDocument/semanticTokens/full", json!({ "textDocument": { "uri": uri(&player) } }));
    assert!(tokens["result"]["data"].as_array().unwrap().len() > 20);
}

fn shutdown(client: &mut Client) {
    let reply = client.request("shutdown", Value::Null);
    assert!(reply["error"].is_null());
    client.notify("exit", Value::Null);
}

#[test]
fn stdio_round_trip() {
    let dir = project();
    let (mut child, mut client) = stdio_server(&["--project", dir.to_str().unwrap()]);
    let init = client.initialize(Value::Null);
    assert_eq!(init["result"]["serverInfo"]["name"], "scratchpilerd");
    exercise(&mut client, &dir);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}

#[test]
fn project_comes_from_initialization_options() {
    let dir = project();
    let (mut child, mut client) = stdio_server(&[]);
    client.initialize(json!({ "project": dir }));
    exercise(&mut client, &dir);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}

#[test]
fn refuses_to_guess_the_project() {
    let (mut child, mut client) = stdio_server(&[]);
    let reply = client.request("initialize", json!({ "processId": null, "rootUri": "file:///tmp", "capabilities": {} }));
    assert!(reply["error"]["message"].as_str().unwrap().contains("--project"));
    drop(client);
    let _ = child.wait();
}

#[cfg(unix)]
#[test]
fn node_ipc_channel() {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    let dir = project();
    let (ours, theirs) = UnixStream::pair().unwrap();
    let fd = theirs.as_raw_fd();
    let mut command = binary();
    command.args(["--node-ipc", "--project", dir.to_str().unwrap()]).env("NODE_CHANNEL_FD", fd.to_string()).stdin(Stdio::null()).stdout(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(theirs);
    let mut client = Client::new(ours.try_clone().unwrap(), ours, true);
    client.initialize(Value::Null);
    exercise(&mut client, &dir);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn pipe_transport() {
    use std::os::unix::net::UnixListener;
    let dir = project();
    let socket = dir.join("client.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = binary()
        .arg(format!("--pipe={}", socket.display()))
        .args(["--project", dir.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().unwrap();
    let mut client = Client::new(stream.try_clone().unwrap(), stream, false);
    client.initialize(Value::Null);
    exercise(&mut client, &dir);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}

#[test]
fn socket_transport() {
    let dir = project();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut child =
        binary().arg(format!("--socket={port}")).args(["--project", dir.to_str().unwrap()]).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap();
    let (stream, _) = listener.accept().unwrap();
    let mut client = Client::new(stream.try_clone().unwrap(), stream, false);
    client.initialize(Value::Null);
    exercise(&mut client, &dir);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn daemon_serves_several_editors() {
    use std::os::unix::net::UnixStream;
    let dir = project();
    let socket = dir.join("daemon.sock");
    let mut child = binary()
        .args(["daemon", &format!("--listen={}", socket.display()), "--project", dir.to_str().unwrap(), "--idle-exit", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let connect = || loop {
        if let Ok(stream) = UnixStream::connect(&socket) {
            return stream;
        }
        assert!(Instant::now() < deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(20));
    };
    let first = connect();
    let mut a = Client::new(first.try_clone().unwrap(), first, false);
    a.initialize(Value::Null);
    exercise(&mut a, &dir);
    let second = connect();
    let mut b = Client::new(second.try_clone().unwrap(), second, false);
    b.initialize(Value::Null);
    let status = b.request("scratchpiler/status", Value::Null);
    assert_eq!(status["result"]["ready"], true);
    shutdown(&mut a);
    shutdown(&mut b);
    drop(a);
    drop(b);
    let exited = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        child.try_wait().unwrap().is_some()
    });
    assert!(exited, "daemon should exit after --idle-exit with no editors");
}

#[test]
fn check_mode_reports_problems() {
    let dir = project();
    let output = binary().args(["check", "--project", dir.to_str().unwrap()]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Player.sdsl:12:15: warning: Nothing receives \"nobody\""), "{stdout}");
    assert!(output.status.success());
}

#[test]
fn warm_cache_skips_analysis() {
    let dir = project();
    let cache = dir.join("cache");
    let run = || {
        let out = Command::new(env!("CARGO_BIN_EXE_scratchpilerd"))
            .args(["warm", "--project", dir.to_str().unwrap(), "--cache-dir", cache.to_str().unwrap()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(run().contains("(0 from cache)"));
    assert!(run().contains("(2 from cache)"));
    std::fs::write(dir.join("Player.sdsl"), format!("{PLAYER}\non click {{\n}}\n")).unwrap();
    assert!(run().contains("(1 from cache)"));
}

#[cfg(unix)]
#[test]
fn symlinked_project_paths_resolve_to_the_same_files() {
    let dir = project();
    let link = dir.with_extension("link");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&dir, &link).unwrap();
    let (mut child, mut client) = stdio_server(&["--project", link.to_str().unwrap()]);
    client.initialize(Value::Null);
    exercise(&mut client, &link);
    shutdown(&mut client);
    assert!(child.wait().unwrap().success());
}
