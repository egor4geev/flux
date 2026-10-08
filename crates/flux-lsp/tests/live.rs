//! The client against real servers, if installed (on the machine or by Flux into its servers
//! directory — `tests/install.rs`): `cargo test -p flux-lsp --test live -- --ignored --nocapture`.
//! Each test makes a tiny project in a temporary directory, opens a file with a problem, and waits
//! for diagnostics, then hover, definition, completion, formatting — what the server does. The
//! first run of rust-analyzer indexes the standard library: a minute or so.

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use std::path::PathBuf;

use flux_lsp::lsp_types::notification::DidOpenTextDocument;
use flux_lsp::lsp_types::request::{Completion, Formatting, GotoDefinition, HoverRequest};
use flux_lsp::lsp_types::{
    CompletionParams, CompletionResponse, DidOpenTextDocumentParams, DocumentFormattingParams,
    FormattingOptions, GotoDefinitionParams, GotoDefinitionResponse, HoverParams, Location,
    Position, TextDocumentIdentifier, TextDocumentItem, TextDocumentPositionParams, Uri,
};
use flux_lsp::position::{path_from_uri, uri_from_path};
use flux_lsp::{LanguageServer, RequestError, ServerConfig, ServerEvent, default_servers, install};
use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};

const TIMEOUT: Duration = Duration::from_secs(240);

fn servers_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLUX_SERVERS_DIR") {
        return dir.into();
    }
    let home = std::env::var_os("HOME").expect("HOME");
    PathBuf::from(home).join("Library/Application Support/flux/servers")
}

fn config(name: &str) -> Option<ServerConfig> {
    install::set_dir(servers_dir());
    let config = default_servers().into_iter().find(|c| c.name == name)?;
    if config.resolve_command().is_none() {
        eprintln!("{name} is not installed: skipped");
        return None;
    }
    Some(config)
}

fn block_on<T: Send + 'static>(future: impl Future<Output = T> + Send + 'static) -> T {
    let (sender, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(futures::executor::block_on(future));
    });
    receiver
        .recv_timeout(TIMEOUT)
        .expect("the request didn't resolve")
}

/// Calls `f` until it gives something; servers answer `null` or "content modified" while they
/// are still loading the project.
fn retry<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = f() {
            return value;
        }
        assert!(Instant::now() < deadline, "no {what}");
        thread::sleep(Duration::from_millis(300));
    }
}

fn answer<T>(result: Result<Option<T>, RequestError>) -> Option<T> {
    match result {
        Ok(value) => value,
        Err(error) if error.is_outdated() => None,
        Err(error) => panic!("{error:?}"),
    }
}

fn wait_event(
    events: &mut UnboundedReceiver<ServerEvent>,
    mut f: impl FnMut(&ServerEvent) -> bool,
) -> ServerEvent {
    retry("event", || {
        loop {
            match events.try_recv() {
                Ok(event) if f(&event) => return Some(event),
                Ok(ServerEvent::Exited { reason }) => panic!("the server exited: {reason}"),
                Ok(_) => continue,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Closed) => panic!("the event stream ended"),
            }
        }
    })
}

fn at(uri: &Uri, line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: uri.clone() },
        position: Position::new(line, character),
    }
}

/// Starts the server on `root`, opens `file`, and checks what the server says about it; returns
/// the server for more checks ([`finish`] stops it).
fn exercise(
    config: &ServerConfig,
    root: &Path,
    file: &Path,
    language: &str,
    checks: Checks,
) -> Running {
    let started = Instant::now();
    let (server, mut events) = LanguageServer::start(config, root).unwrap();
    wait_event(&mut events, |e| matches!(e, ServerEvent::Initialized));
    eprintln!("{}: initialized in {:?}", config.name, started.elapsed());

    let uri = uri_from_path(file);
    let text = std::fs::read_to_string(file).unwrap();
    server.notify::<DidOpenTextDocument>(DidOpenTextDocumentParams {
        text_document: TextDocumentItem::new(uri.clone(), language.into(), 1, text),
    });

    let ServerEvent::Diagnostics(diagnostics) = wait_event(&mut events, |e| {
        matches!(e, ServerEvent::Diagnostics(d)
            if path_from_uri(&d.uri).as_deref() == Some(file)
                && d.diagnostics.iter().any(|d| d.range.start.line == checks.error_line))
    }) else {
        unreachable!()
    };
    let error = diagnostics
        .diagnostics
        .iter()
        .find(|d| d.range.start.line == checks.error_line)
        .unwrap();
    eprintln!(
        "{}: diagnostic in {:?}: {}",
        config.name,
        started.elapsed(),
        error.message
    );
    for line in checks.clean_lines {
        let found: Vec<_> = diagnostics
            .diagnostics
            .iter()
            .filter(|d| d.range.start.line == *line)
            .map(|d| &d.message)
            .collect();
        assert!(found.is_empty(), "line {line}: {found:?}");
    }

    if let Some((line, character, expected)) = checks.hover {
        let hover = retry("hover", || {
            answer(block_on(server.request::<HoverRequest>(HoverParams {
                text_document_position_params: at(&uri, line, character),
                work_done_progress_params: Default::default(),
            })))
        });
        let hover_text = format!("{:?}", hover.contents);
        assert!(hover_text.contains(expected), "{hover_text}");
    }

    if let Some((line, character, expected_line)) = checks.definition {
        let target = definition(&server, &uri, line, character);
        assert_eq!(target.range.start.line, expected_line, "{target:?}");
    }

    if let Some((line, character, expected)) = checks.completion {
        let items = retry("completion", || {
            let response = answer(block_on(server.request::<Completion>(CompletionParams {
                text_document_position: at(&uri, line, character),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            })))?;
            let items = match response {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => list.items,
            };
            (!items.is_empty()).then_some(items)
        });
        assert!(
            items.iter().any(|item| item.label.starts_with(expected)),
            "{:?}",
            items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    if checks.formatting {
        let edits = retry("formatting", || {
            answer(block_on(server.request::<Formatting>(
                DocumentFormattingParams {
                    text_document: TextDocumentIdentifier { uri: uri.clone() },
                    options: FormattingOptions {
                        tab_size: 4,
                        insert_spaces: true,
                        ..Default::default()
                    },
                    work_done_progress_params: Default::default(),
                },
            )))
            .filter(|edits| !edits.is_empty())
        });
        eprintln!("{}: {} formatting edits", config.name, edits.len());
    }
    eprintln!("{}: all answered in {:?}", config.name, started.elapsed());
    Running {
        server,
        events,
        uri,
    }
}

struct Running {
    server: LanguageServer,
    events: UnboundedReceiver<ServerEvent>,
    uri: Uri,
}

/// Where the definition of the symbol at `line`:`character` is.
fn definition(server: &LanguageServer, uri: &Uri, line: u32, character: u32) -> Location {
    let response = retry("definition", || {
        answer(block_on(server.request::<GotoDefinition>(
            GotoDefinitionParams {
                text_document_position_params: at(uri, line, character),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )))
    });
    match response {
        GotoDefinitionResponse::Scalar(location) => location,
        GotoDefinitionResponse::Array(locations) => locations[0].clone(),
        GotoDefinitionResponse::Link(links) => Location {
            uri: links[0].target_uri.clone(),
            range: links[0].target_selection_range,
        },
    }
}

fn finish(running: Running) {
    let Running {
        server, mut events, ..
    } = running;
    block_on(server.shutdown());
    wait_event(&mut events, |e| matches!(e, ServerEvent::Exited { .. }));
}

#[derive(Default)]
struct Checks {
    /// A diagnostic must come for this line.
    error_line: u32,
    /// …and none for these.
    clean_lines: &'static [u32],
    /// Line, character, and what the hover text contains.
    hover: Option<(u32, u32, &'static str)>,
    /// Line, character, and the line of the definition (in the same file).
    definition: Option<(u32, u32, u32)>,
    /// Line, character, and the start of a label among the items.
    completion: Option<(u32, u32, &'static str)>,
    /// The server formats the file (some edits).
    formatting: bool,
}

/// A temporary project directory (canonical: macOS's `/var` is a link to `/private/var`, and
/// servers report real paths).
fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (path, contents) in files {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    (dir, root)
}

#[test]
#[ignore]
fn rust_analyzer() {
    let Some(config) = config("rust-analyzer") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"live\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    let file = root.join("src/main.rs");
    std::fs::write(
        &file,
        "fn helper() -> i32 {\n    1\n}\n\nfn main() {\n    let x: i32 = \"text\";\n    \
         let s = String::new();\n    let n = s.len();\n    helper();\n    println!(\"{x}{n}\");\n}\n",
    )
    .unwrap();
    finish(exercise(
        &config,
        &root,
        &file,
        "rust",
        Checks {
            error_line: 5,
            hover: Some((6, 12, "String")),
            definition: Some((8, 4, 0)),
            completion: Some((7, 14, "len")),
            ..Default::default()
        },
    ));
}

#[test]
#[ignore]
fn gopls() {
    let Some(config) = config("gopls") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    std::fs::write(root.join("go.mod"), "module live\n\ngo 1.21\n").unwrap();
    let file = root.join("main.go");
    std::fs::write(
        &file,
        "package main\n\nimport \"fmt\"\n\nfunc helper() int { return 1 }\n\nfunc main() {\n\t\
         var x int = \"text\"\n\tfmt.Println(x, helper())\n}\n",
    )
    .unwrap();
    finish(exercise(
        &config,
        &root,
        &file,
        "go",
        Checks {
            error_line: 7,
            hover: Some((8, 6, "Println")),
            definition: Some((8, 16, 4)),
            completion: Some((8, 5, "Println")),
            ..Default::default()
        },
    ));
}

/// pyright with the project's virtual environment: a package installed there resolves (no
/// "import could not be resolved"), and its definition is in the environment.
#[test]
#[ignore]
fn pyright_with_a_virtual_environment() {
    let Some(config) = config("pyright") else {
        return;
    };
    let (_dir, root) = project(&[(
        "main.py",
        "import idna\n\n\ndef helper() -> int:\n    return 1\n\n\nx: int = \"text\"\nencoded = idna.encode(\"example.com\")\nhelper()\ns = \"abc\"\ns.upper()\n",
    )]);
    let venv = root.join(".venv");
    let status = std::process::Command::new("python3")
        .args(["-m", "venv"])
        .arg(&venv)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new(venv.join("bin/pip"))
        .args(["install", "--quiet", "idna"])
        .status()
        .unwrap();
    assert!(status.success());
    let running = exercise(
        &config,
        &root,
        &root.join("main.py"),
        "python",
        Checks {
            error_line: 7,
            clean_lines: &[0],
            hover: Some((11, 3, "upper")),
            definition: Some((9, 0, 3)),
            completion: Some((11, 2, "upper")),
            ..Default::default()
        },
    );
    let target = definition(&running.server, &running.uri, 8, 16);
    let path = path_from_uri(&target.uri).unwrap();
    assert!(path.starts_with(&venv), "{path:?} not in {venv:?}");
    finish(running);
}

/// ruff: lint diagnostics and the formatting pyright doesn't do.
#[test]
#[ignore]
fn ruff() {
    let Some(config) = config("ruff") else {
        return;
    };
    let (_dir, root) = project(&[(
        "lint.py",
        "import os\n\n\ndef f():\n    x=1\n    return x\n",
    )]);
    finish(exercise(
        &config,
        &root,
        &root.join("lint.py"),
        "python",
        Checks {
            error_line: 0,
            formatting: true,
            ..Default::default()
        },
    ));
}

/// typescript-language-server in a project without `node_modules`: it uses the TypeScript
/// installed next to it (the fallback path).
#[test]
#[ignore]
fn typescript_language_server() {
    let Some(config) = config("typescript-language-server") else {
        return;
    };
    let (_dir, root) = project(&[(
        "app.ts",
        "function helper(): number {\n    return 1;\n}\n\nlet x: number = \"text\";\nconst s = \"abc\";\ns.toUpperCase();\nhelper();\nconsole.log(  x  );\n",
    )]);
    finish(exercise(
        &config,
        &root,
        &root.join("app.ts"),
        "typescript",
        Checks {
            error_line: 4,
            hover: Some((6, 3, "toUpperCase")),
            definition: Some((7, 1, 0)),
            completion: Some((6, 2, "toUpperCase")),
            formatting: true,
            ..Default::default()
        },
    ));
}

#[test]
#[ignore]
fn taplo() {
    let Some(config) = config("taplo") else {
        return;
    };
    let (_dir, root) = project(&[(
        "config.toml",
        "[package]\nname = \"x\"\nversion = = \"1\"\n",
    )]);
    finish(exercise(
        &config,
        &root,
        &root.join("config.toml"),
        "toml",
        Checks {
            error_line: 2,
            ..Default::default()
        },
    ));
}

#[test]
#[ignore]
fn yaml_language_server() {
    let Some(config) = config("yaml-language-server") else {
        return;
    };
    let (_dir, root) = project(&[(
        "bad.yaml",
        "key: value\nlist:\n  - a\n  - b\nbroken: [unclosed\n",
    )]);
    finish(exercise(
        &config,
        &root,
        &root.join("bad.yaml"),
        "yaml",
        // The unclosed bracket is reported where the file ends.
        Checks {
            error_line: 5,
            ..Default::default()
        },
    ));
}

#[test]
#[ignore]
fn vscode_json_language_server() {
    let Some(config) = config("vscode-json-language-server") else {
        return;
    };
    let (_dir, root) = project(&[("bad.json", "{\n  \"a\": 1,\n  \"b\": ,\n  \"c\":[1,2]\n}\n")]);
    finish(exercise(
        &config,
        &root,
        &root.join("bad.json"),
        "json",
        Checks {
            error_line: 2,
            formatting: true,
            ..Default::default()
        },
    ));
}

/// bash-language-server with shellcheck (installed by Flux next to it if the machine has none).
#[test]
#[ignore]
fn bash_language_server() {
    let Some(config) = config("bash-language-server") else {
        return;
    };
    let (_dir, root) = project(&[("script.sh", "#!/bin/bash\nname=$1\necho $name\n")]);
    finish(exercise(
        &config,
        &root,
        &root.join("script.sh"),
        "shellscript",
        Checks {
            error_line: 2,
            hover: Some((2, 1, "echo")),
            ..Default::default()
        },
    ));
}

#[test]
#[ignore]
fn marksman() {
    let Some(config) = config("marksman") else {
        return;
    };
    let (_dir, root) = project(&[
        (
            "README.md",
            "# Title\n\nSee [other](other.md).\n\nAnd [[missing]] too.\n",
        ),
        ("other.md", "# Other\n\nText.\n"),
        // marksman checks links between documents only in a workspace: a git repository or a
        // folder with this file.
        (".marksman.toml", ""),
    ]);
    let running = exercise(
        &config,
        &root,
        &root.join("README.md"),
        "markdown",
        Checks {
            error_line: 4,
            ..Default::default()
        },
    );
    let target = definition(&running.server, &running.uri, 2, 6);
    assert_eq!(path_from_uri(&target.uri), Some(root.join("other.md")));
    finish(running);
}
