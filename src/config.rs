use std::io::Write;
use std::{collections::BTreeMap, env, ffi::OsString, io, path::PathBuf, time::Duration};

pub const VERSION: &str = match option_env!("MCP_LAZY_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub struct Config {
    pub command: Vec<OsString>,
    pub cache_dir: PathBuf,
    pub ttl: Duration,
    pub idle: Duration,
    pub shutdown: Duration,
    pub response_timeout: Duration,
}

impl Config {
    pub fn parse() -> io::Result<Option<Self>> {
        let args: Vec<_> = env::args_os().skip(1).collect();
        if args.len() == 1 && args[0] == "--version" {
            let _ = writeln!(io::stdout(), "mcp-lazy-proxy {VERSION}");
            return Ok(None);
        }
        if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
            let _ = writeln!(
                io::stdout(),
                "mcp-lazy-proxy [--cache-dir DIR] [--cache-ttl-ms N] [--idle-ms N]\n\
                [--shutdown-ms N] [--response-timeout-ms N] -- COMMAND [ARGS...]\n\
                Defaults: cache OS_TEMP/mcp-lazy-proxy, TTL 600000, idle 1800000, shutdown 2000, response timeout 3600000.\n\
                Zero cache TTL disables caching; zero idle disables automatic idle shutdown. Logs use stderr."
            );
            return Ok(None);
        }
        let mut config = Self {
            command: Vec::new(),
            cache_dir: env::temp_dir().join("mcp-lazy-proxy"),
            ttl: Duration::from_secs(600),
            idle: Duration::from_secs(1800),
            shutdown: Duration::from_secs(2),
            response_timeout: Duration::from_secs(3600),
        };
        let mut i = 0;
        while i < args.len() {
            if args[i] == "--" {
                config.command = args[i + 1..].to_vec();
                break;
            }
            let option = args[i].to_str().unwrap_or("");
            let value = args
                .get(i + 1)
                .ok_or_else(|| invalid("missing option value"))?;
            if option == "--cache-dir" {
                config.cache_dir = value.into();
            } else {
                let ms = value
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|ms| *ms <= 86_400_000)
                    .ok_or_else(|| invalid("duration must be milliseconds in 0..86400000"))?;
                if ms == 0 && !matches!(option, "--cache-ttl-ms" | "--idle-ms") {
                    return Err(invalid("only cache TTL and idle may be zero"));
                }
                let duration = Duration::from_millis(ms);
                match option {
                    "--cache-ttl-ms" => config.ttl = duration,
                    "--idle-ms" => config.idle = duration,
                    // Accepted for older configurations; discovery no longer changes idle lifetime.
                    "--discovery-idle-ms" => (),
                    "--shutdown-ms" => config.shutdown = duration,
                    "--response-timeout-ms" => config.response_timeout = duration,
                    _ => return Err(invalid("unknown option; use --help")),
                }
            }
            i += 2;
        }
        if config.command.is_empty() {
            return Err(invalid("expected -- COMMAND [ARGS...]"));
        }
        Ok(Some(config))
    }
}

pub fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn included_env(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name != "PATH"
        && name != "CODEX_TOOL_CALL_ID"
        && !name.starts_with("INIT_")
        && !name.starts_with("NPM_")
}

pub fn environment() -> BTreeMap<String, String> {
    env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.to_string_lossy().into_owned();
            if !included_env(&key) {
                return None;
            }
            #[cfg(windows)]
            let key = key.to_ascii_uppercase();
            Some((key, value.to_string_lossy().into_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_only_agreed_session_noise() {
        for name in ["Path", "INIT_CWD", "npm_config_cache", "CODEX_TOOL_CALL_ID"] {
            assert!(!included_env(name));
        }
        assert!(included_env("API_KEY"));
        assert!(included_env("TEMP"));
    }
}
