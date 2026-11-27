use crate::config::{Config, environment};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub fn hash(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value"))
    )
}
pub fn cacheable(method: &str) -> bool {
    matches!(method, "initialize" | "server/discover" | "tools/list")
}
pub fn discovery(method: &str) -> bool {
    cacheable(method)
        || matches!(
            method,
            "prompts/list" | "resources/list" | "resources/templates/list"
        )
}
pub struct Cache {
    root: PathBuf,
    ttl: Duration,
}
impl Cache {
    pub fn new(config: &Config) -> Self {
        let command: Vec<_> = config.command.iter().map(|s| s.to_string_lossy()).collect();
        // Never persist raw environment variables or command arguments.
        let identity = json!({"format":1,"command":command,"env":environment(),"cwd":std::env::current_dir().ok()});
        Self {
            root: config.cache_dir.join(hash(&identity)),
            ttl: config.ttl,
        }
    }
    pub fn key(method: &str, params: &Value, initialize: &Value) -> String {
        format!(
            "{}-{}",
            method.replace('/', "_"),
            hash(&json!([params, initialize]))
        )
    }
    pub fn get(&self, key: &str) -> Option<Value> {
        if self.ttl.is_zero() {
            return None;
        }
        let data = fs::read(self.root.join(format!("{key}.json"))).ok()?;
        let entry: Value = serde_json::from_slice(&data).ok()?;
        let age = now().checked_sub(entry["writtenMs"].as_u64()?)?;
        if age >= self.ttl.as_millis() as u64 {
            return None;
        }
        entry.get("result").cloned()
    }
    pub fn put(&self, key: &str, result: &Value) {
        if self.ttl.is_zero() {
            return;
        }
        let save = || -> std::io::Result<()> {
            fs::create_dir_all(&self.root)?;
            let file = self.root.join(format!("{key}.json"));
            let temporary = self.root.join(format!("{key}.{}.part", std::process::id()));
            fs::write(
                &temporary,
                serde_json::to_vec(&json!({"writtenMs":now(),"result":result}))?,
            )?;
            let result = fs::rename(&temporary, file);
            if result.is_err() {
                let _ = fs::remove_file(temporary);
            }
            result
        };
        if save().is_err() {
            eprintln!("mcp-lazy: cache write failed (continuing without cache)");
        }
    }
    pub fn invalidate_tools(&self) {
        if let Ok(files) = fs::read_dir(&self.root) {
            for file in files.flatten() {
                if file
                    .file_name()
                    .to_string_lossy()
                    .starts_with("tools_list-")
                {
                    let _ = fs::remove_file(file.path());
                }
            }
        }
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_distinguish_pages_and_negotiation_but_not_object_order() {
        assert_eq!(hash(&json!({"a":1,"b":2})), hash(&json!({"b":2,"a":1})));
        assert_ne!(
            Cache::key("tools/list", &json!({}), &json!({})),
            Cache::key("tools/list", &json!({"cursor":"next"}), &json!({}))
        );
        assert_ne!(
            Cache::key("initialize", &json!({}), &json!({"capabilities":{}})),
            Cache::key(
                "initialize",
                &json!({}),
                &json!({"capabilities":{"roots":{}}})
            )
        );
    }
}
