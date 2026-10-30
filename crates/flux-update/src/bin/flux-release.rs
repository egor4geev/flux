//! CI's half of the updates (part 10.8, `.github/workflows/release.yml`):
//!
//! ```text
//! flux-release keygen
//!     prints a new key pair: `private: <base64 of the 32-byte seed>`, `public: <base64>`
//! flux-release sign <file>
//!     the Ed25519 signature (base64) of the file's bytes; the private key from the environment
//!     variable FLUX_UPDATE_PRIVATE_KEY (base64 of the seed)
//! flux-release verify <file> <signature> [--public-key <base64>]
//!     checks a signature (by default with the key built into Flux)
//! flux-release manifest --version <X.Y.Z[-beta.N]> --date <YYYY-MM-DD> --notes <file.md>
//!         --base-url <https://github.com/…/releases/download/vX.Y.Z>
//!         --platform <key>=<archive.zip>[,<installer>] [--platform …]
//!     prints `latest.json` (the channel follows the version; archives are signed with
//!     FLUX_UPDATE_PRIVATE_KEY, their size and SHA-256 computed)
//! ```

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use flux_update::download::sha256_hex;
use flux_update::manifest::{FORMAT, Manifest, Platform};
use flux_update::{Channel, Version, signing};

const USAGE: &str = "usage:
  flux-release keygen
  flux-release sign <file>                       (FLUX_UPDATE_PRIVATE_KEY: base64 seed)
  flux-release verify <file> <signature> [--public-key <base64>]
  flux-release manifest --version <X.Y.Z[-beta.N]> --date <YYYY-MM-DD> --notes <file.md>
      --base-url <url> --platform <key>=<archive.zip>[,<installer>] [--platform …]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("flux-release: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<String, String> {
    let (command, rest) = args.split_first().ok_or(USAGE)?;
    match command.as_str() {
        "keygen" => {
            let (private, public) = signing::generate()?;
            Ok(format!("private: {private}\npublic: {public}"))
        }
        "sign" => {
            let [file] = rest else {
                return Err(USAGE.into());
            };
            signing::sign(&read(file)?, &private_key()?)
        }
        "verify" => {
            let (positional, options) = split(rest)?;
            let [file, signature] = positional.as_slice() else {
                return Err(USAGE.into());
            };
            let public = options
                .get("public-key")
                .and_then(|values| values.last())
                .map(String::as_str)
                .unwrap_or(flux_update::PUBLIC_KEY);
            signing::verify_signature(&read(file)?, signature, public)?;
            Ok("OK: the signature matches".into())
        }
        "manifest" => manifest(rest),
        "help" | "--help" | "-h" => Ok(USAGE.into()),
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn manifest(args: &[String]) -> Result<String, String> {
    let (positional, options) = split(args)?;
    if !positional.is_empty() {
        return Err(format!("unexpected {}\n{USAGE}", positional[0]));
    }
    let one = |name: &str| -> Result<&str, String> {
        match options.get(name).map(Vec::as_slice) {
            Some([value]) => Ok(value.as_str()),
            Some(_) => Err(format!("--{name} given more than once")),
            None => Err(format!("--{name} is missing\n{USAGE}")),
        }
    };
    let version = Version::parse(one("version")?.trim_start_matches('v'))
        .map_err(|err| format!("--version: {err}"))?;
    let date = one("date")?.to_string();
    if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return Err(format!("--date {date}: expected YYYY-MM-DD"));
    }
    let notes = String::from_utf8(read(one("notes")?)?).map_err(|_| "--notes: not UTF-8")?;
    let base_url = one("base-url")?.trim_end_matches('/').to_string();
    let private = private_key()?;
    let mut platforms = BTreeMap::new();
    for spec in options.get("platform").ok_or("--platform is missing")? {
        let (key, files) = spec
            .split_once('=')
            .ok_or_else(|| format!("--platform {spec}: expected <key>=<archive>[,<installer>]"))?;
        let (archive, installer) = match files.split_once(',') {
            Some((archive, installer)) => (archive, Some(installer)),
            None => (files, None),
        };
        let bytes = read(archive)?;
        let url = |file: &str| format!("{base_url}/{}", file_name(file));
        let platform = Platform {
            url: url(archive),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
            signature: signing::sign(&bytes, &private)?,
            installer: installer.map(url),
        };
        if platforms.insert(key.to_string(), platform).is_some() {
            return Err(format!("--platform {key} given twice"));
        }
    }
    let manifest = Manifest {
        format: FORMAT,
        channel: Channel::of(&version),
        notes_url: Some(flux_update::release_page(&version)),
        version,
        date,
        notes,
        platforms,
    };
    serde_json::to_string_pretty(&manifest).map_err(|err| err.to_string())
}

/// `--name value` options by name (an option may repeat).
type Options = BTreeMap<String, Vec<String>>;

/// Positional arguments and options.
fn split(args: &[String]) -> Result<(Vec<String>, Options), String> {
    let mut positional = Vec::new();
    let mut options = Options::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let (name, value) = match name.split_once('=') {
                Some((name, value)) => (name, value.to_string()),
                None => (
                    name,
                    args.next()
                        .ok_or_else(|| format!("--{name} needs a value"))?
                        .clone(),
                ),
            };
            options.entry(name.to_string()).or_default().push(value);
        } else {
            positional.push(arg.clone());
        }
    }
    Ok((positional, options))
}

fn private_key() -> Result<String, String> {
    std::env::var("FLUX_UPDATE_PRIVATE_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| "FLUX_UPDATE_PRIVATE_KEY isn't set".into())
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("{path}: {err}"))
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}
