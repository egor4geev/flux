// The ten language servers Flux had built in before stage 8.3, as test fixtures: since then servers
// come from language plugins (`[[language-servers]]` of their manifests). Included (`include!`) by
// the unit tests (`crate::fixtures`) and by the integration tests, so the module that includes it
// brings `Install` and `ServerConfig` into scope.

/// rust-analyzer, gopls, pyright and ruff (Python: types, then formatting and linting),
/// typescript-language-server, taplo, bash-language-server, yaml-language-server,
/// vscode-json-language-server, marksman.
#[allow(dead_code)]
pub fn default_servers() -> Vec<ServerConfig> {
    vec![
        with_install(
            server("rust-analyzer", "rust-analyzer", &[], &["rs"], &[]),
            Install::Rustup {
                component: "rust-analyzer".into(),
                fallback: Box::new(github(
                    "rust-lang/rust-analyzer",
                    "rust-analyzer-{arch}-apple-darwin.gz",
                    "rust-analyzer",
                )),
            },
        ),
        with_install(
            server("gopls", "gopls", &[], &["go"], &[]),
            Install::GoInstall {
                package: "golang.org/x/tools/gopls".into(),
                bin: "gopls".into(),
            },
        ),
        with_install(
            server(
                "pyright",
                "pyright-langserver",
                &["--stdio"],
                &["py", "pyi", "pyw"],
                &[],
            ),
            npm(&["pyright"], "pyright-langserver"),
        ),
        // After pyright: it has no formatter; ruff formats and lints.
        with_install(
            server("ruff", "ruff", &["server"], &["py", "pyi"], &[]),
            github("astral-sh/ruff", "ruff-{arch}-apple-darwin.tar.gz", "ruff"),
        ),
        with_install(
            server(
                "typescript-language-server",
                "typescript-language-server",
                &["--stdio"],
                &["ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx"],
                &[],
            ),
            npm(
                &["typescript-language-server", "typescript@6"],
                "typescript-language-server",
            ),
        ),
        with_install(
            server(
                "taplo",
                "taplo",
                &["lsp", "stdio"],
                &["toml"],
                &["Cargo.lock", "Pipfile", "poetry.lock", "uv.lock"],
            ),
            github("tamasfe/taplo", "taplo-darwin-{arch}.gz", "taplo"),
        ),
        with_install(
            server(
                "bash-language-server",
                "bash-language-server",
                &["start"],
                &["sh", "bash"],
                &[
                    ".bashrc",
                    ".bash_profile",
                    ".bash_aliases",
                    ".bash_logout",
                    ".profile",
                    "PKGBUILD",
                ],
            ),
            npm(&["bash-language-server"], "bash-language-server"),
        ),
        with_install(
            server(
                "yaml-language-server",
                "yaml-language-server",
                &["--stdio"],
                &["yaml", "yml"],
                &[".clang-format", ".clang-tidy"],
            ),
            npm(&["yaml-language-server"], "yaml-language-server"),
        ),
        with_install(
            ServerConfig {
                // Formatting is off unless asked for.
                initialization_options: Some(serde_json::json!({ "provideFormatter": true })),
                ..server(
                    "vscode-json-language-server",
                    "vscode-json-language-server",
                    &["--stdio"],
                    &["json", "jsonc"],
                    &["flake.lock"],
                )
            },
            npm(
                &["vscode-langservers-extracted"],
                "vscode-json-language-server",
            ),
        ),
        with_install(
            server("marksman", "marksman", &["server"], &["md", "markdown"], &[]),
            github("artempyanykh/marksman", "marksman-macos", "marksman"),
        ),
    ]
}

#[allow(dead_code)]
fn npm(packages: &[&str], bin: &str) -> Install {
    Install::Npm {
        packages: packages.iter().map(|p| p.to_string()).collect(),
        bin: bin.to_string(),
    }
}

#[allow(dead_code)]
fn github(repo: &str, asset: &str, bin: &str) -> Install {
    Install::GitHubRelease {
        repo: repo.to_string(),
        asset: asset.to_string(),
        bin: bin.to_string(),
    }
}

/// A config without an install recipe.
#[allow(dead_code)]
pub fn server(
    name: &str,
    command: &str,
    args: &[&str],
    extensions: &[&str],
    file_names: &[&str],
) -> ServerConfig {
    let strings = |items: &[&str]| items.iter().map(|s| s.to_string()).collect();
    ServerConfig {
        name: name.to_string(),
        command: command.to_string(),
        args: strings(args),
        extensions: strings(extensions),
        file_names: strings(file_names),
        initialization_options: None,
        settings: None,
        install: None,
    }
}

#[allow(dead_code)]
fn with_install(config: ServerConfig, install: Install) -> ServerConfig {
    ServerConfig {
        install: Some(install),
        ..config
    }
}
