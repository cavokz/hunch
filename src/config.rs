use crate::migrate;
use crate::paths::{expand_tilde, resolve_foray_home};
use crate::store::{Store, StoreError};
use crate::store_elasticsearch::ElasticsearchStore;
use crate::store_json::JsonFileStore;
use crate::store_stdio::StdioStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

// ── Config file types ───────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    stores: BTreeMap<String, RawStoreConfig>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, tag = "type")]
enum RawStoreConfig {
    #[serde(rename = "json_file")]
    JsonFile {
        /// Absolute path to the journals directory (must be valid UTF-8; TOML guarantees this).
        path: String,
        /// Human-readable description — required; helps the model suggest the right store.
        description: String,
    },
    #[serde(rename = "foray_stdio")]
    ForayStdio {
        /// Command to spawn the remote foray server (must be a `foray` binary or a
        /// transport that ends up invoking one).
        command: String,
        /// Arguments passed to `command` **before** the implicit `serve` that
        /// `StdioStore` always appends.
        ///
        /// Local:  `command = "foray"`, `args = []`  →  spawns `foray serve`
        /// SSH:    `command = "ssh"`, `args = ["user@host", "--", "foray"]`
        ///         →  spawns `ssh user@host -- foray serve`
        #[serde(default)]
        args: Vec<String>,
        /// Human-readable description — required; helps the model suggest the right store.
        description: String,
        /// Preferred store name on the remote server (first from hello if absent).
        #[serde(default)]
        store: Option<String>,
    },
    #[serde(rename = "elasticsearch")]
    Elasticsearch {
        /// Elasticsearch URL with the index as the last path segment,
        /// e.g. `https://es.example.com/foray-team`.
        url: String,
        /// Human-readable description — required; helps the model suggest the right store.
        description: String,
        /// API key for authentication (mutually exclusive with username/password).
        #[serde(default)]
        api_key: Option<String>,
        /// Username for HTTP Basic authentication (mutually exclusive with api_key).
        #[serde(default)]
        username: Option<String>,
        /// Password for HTTP Basic authentication (mutually exclusive with api_key).
        #[serde(default)]
        password: Option<String>,
    },
}

impl fmt::Debug for RawStoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RawStoreConfig::JsonFile { path, description } => f
                .debug_struct("JsonFile")
                .field("path", path)
                .field("description", description)
                .finish(),
            RawStoreConfig::ForayStdio {
                command,
                args,
                description,
                store,
            } => f
                .debug_struct("ForayStdio")
                .field("command", command)
                .field("args", args)
                .field("description", description)
                .field("store", store)
                .finish(),
            RawStoreConfig::Elasticsearch {
                url,
                description,
                api_key,
                username,
                password,
            } => f
                .debug_struct("Elasticsearch")
                .field("url", url)
                .field("description", description)
                .field("api_key", &api_key.as_ref().map(|_| "<redacted>"))
                .field("username", username)
                .field("password", &password.as_ref().map(|_| "<redacted>"))
                .finish(),
        }
    }
}

impl RawStoreConfig {
    fn description(&self) -> &str {
        match self {
            RawStoreConfig::JsonFile { description, .. } => description,
            RawStoreConfig::ForayStdio { description, .. } => description,
            RawStoreConfig::Elasticsearch { description, .. } => description,
        }
    }
}

// ── StoreRegistry ───────────────────────────────────────────────────

/// A named collection of stores, ordered by store name (BTreeMap key order) for a stable, deterministic nuance.
#[derive(Clone)]
pub(crate) struct StoreRegistry {
    stores: Vec<StoreEntry>,
    /// Stable fingerprint derived from the config (sorted store names+paths).
    pub(crate) nuance: String,
}

#[derive(Clone)]
pub(crate) struct StoreEntry {
    pub(crate) name: String,
    pub(crate) description: String,
    store: Arc<dyn Store>,
}

impl StoreRegistry {
    /// Build a registry from `~/.foray/config.toml`.
    /// Falls back to a single implicit `local` store if the file is absent or has no stores.
    pub(crate) fn load() -> Result<Self, StoreError> {
        Self::load_from(&config_path()?)
    }

    fn load_from(config_path: &std::path::Path) -> Result<Self, StoreError> {
        let raw = if config_path.exists() {
            let text = std::fs::read_to_string(config_path).map_err(StoreError::Io)?;
            toml::from_str::<RawConfig>(&text).map_err(|e| {
                StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("config parse error: {e}"),
                ))
            })?
        } else {
            RawConfig::default()
        };

        if raw.stores.is_empty() {
            return Self::implicit_local();
        }

        let mut stores = Vec::with_capacity(raw.stores.len());
        // BTreeMap iteration is sorted by key → deterministic nuance
        for (name, entry) in &raw.stores {
            let store: Arc<dyn Store> = match entry {
                RawStoreConfig::JsonFile { path, .. } => {
                    let path_buf = expand_tilde(path)?;
                    if !path_buf.is_absolute() {
                        return Err(StoreError::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("store '{name}': path must be absolute, got '{path}'"),
                        )));
                    }
                    Arc::new(JsonFileStore::new(path_buf))
                }
                RawStoreConfig::ForayStdio {
                    command,
                    args,
                    store,
                    ..
                } => Arc::new(StdioStore::new(
                    command.clone(),
                    args.clone(),
                    vec![],
                    store.clone(),
                )),
                RawStoreConfig::Elasticsearch {
                    url,
                    api_key,
                    username,
                    password,
                    ..
                } => {
                    let es_store = ElasticsearchStore::new(
                        url.clone(),
                        api_key.clone(),
                        username.clone(),
                        password.clone(),
                    )?;
                    Arc::new(es_store) as Arc<dyn Store>
                }
            };
            stores.push(StoreEntry {
                name: name.clone(),
                description: entry.description().to_string(),
                store,
            });
        }

        let nuance = Self::compute_nuance_from_config(
            &raw,
            migrate::CURRENT_SCHEMA,
            migrate::CURRENT_PROTOCOL,
        );
        Ok(Self { stores, nuance })
    }

    fn compute_nuance_from_config(raw: &RawConfig, schema: u32, protocol: u32) -> String {
        let config_json =
            serde_json::to_string(raw).expect("RawConfig serialization is infallible");
        compute_nuance(&[
            config_json,
            format!("schema={schema}"),
            format!("protocol={protocol}"),
        ])
    }

    /// Single implicit `local` `JsonFileStore` at `~/.foray/journals/`.
    pub(crate) fn implicit_local() -> Result<Self, StoreError> {
        let base_dir = JsonFileStore::default_dir()?;
        let store: Arc<dyn Store> = Arc::new(JsonFileStore::new(base_dir.clone()));
        let path_str = base_dir.to_str().ok_or_else(|| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "default journal path is not valid UTF-8: {}",
                    base_dir.display()
                ),
            ))
        })?;
        let raw = RawConfig {
            stores: [(
                "local".to_string(),
                RawStoreConfig::JsonFile {
                    path: path_str.to_string(),
                    description: "Default local journal store".to_string(),
                },
            )]
            .into(),
        };
        let nuance = Self::compute_nuance_from_config(
            &raw,
            migrate::CURRENT_SCHEMA,
            migrate::CURRENT_PROTOCOL,
        );
        let stores = raw
            .stores
            .iter()
            .map(|(name, entry)| StoreEntry {
                name: name.clone(),
                description: entry.description().to_string(),
                store: store.clone(),
            })
            .collect();
        Ok(Self { stores, nuance })
    }

    /// Construct a single-store registry backed by `base_dir`.
    /// For use in tests — avoids depending on the user's home directory.
    #[cfg(test)]
    pub(crate) fn for_test(base_dir: std::path::PathBuf) -> Self {
        let store: Arc<dyn Store> = Arc::new(JsonFileStore::new(base_dir.clone()));
        let raw = RawConfig {
            stores: [(
                "local".to_string(),
                RawStoreConfig::JsonFile {
                    path: base_dir
                        .to_str()
                        .expect("test dir is valid UTF-8")
                        .to_string(),
                    description: "Test store".to_string(),
                },
            )]
            .into(),
        };
        let nuance = Self::compute_nuance_from_config(
            &raw,
            migrate::CURRENT_SCHEMA,
            migrate::CURRENT_PROTOCOL,
        );
        let stores = raw
            .stores
            .iter()
            .map(|(name, entry)| StoreEntry {
                name: name.clone(),
                description: entry.description().to_string(),
                store: store.clone(),
            })
            .collect();
        Self { stores, nuance }
    }

    /// Construct a two-store registry. For use in tests only.
    #[cfg(test)]
    pub(crate) fn for_test_two(
        base_dir1: std::path::PathBuf,
        base_dir2: std::path::PathBuf,
    ) -> Self {
        let store1: Arc<dyn Store> = Arc::new(JsonFileStore::new(base_dir1.clone()));
        let store2: Arc<dyn Store> = Arc::new(JsonFileStore::new(base_dir2.clone()));
        let raw = RawConfig {
            stores: [
                (
                    "store1".to_string(),
                    RawStoreConfig::JsonFile {
                        path: base_dir1
                            .to_str()
                            .expect("test dir is valid UTF-8")
                            .to_string(),
                        description: "Test store 1".to_string(),
                    },
                ),
                (
                    "store2".to_string(),
                    RawStoreConfig::JsonFile {
                        path: base_dir2
                            .to_str()
                            .expect("test dir is valid UTF-8")
                            .to_string(),
                        description: "Test store 2".to_string(),
                    },
                ),
            ]
            .into(),
        };
        let nuance = Self::compute_nuance_from_config(
            &raw,
            migrate::CURRENT_SCHEMA,
            migrate::CURRENT_PROTOCOL,
        );
        let arc_stores = [store1, store2];
        let stores = raw
            .stores
            .iter()
            .zip(arc_stores)
            .map(|((name, entry), store)| StoreEntry {
                name: name.clone(),
                description: entry.description().to_string(),
                store,
            })
            .collect();
        Self { stores, nuance }
    }

    /// Look up a store by name. Returns `None` if not found.
    pub(crate) fn get(&self, name: &str) -> Option<&Arc<dyn Store>> {
        self.stores
            .iter()
            .find(|e| e.name == name)
            .map(|e| &e.store)
    }

    /// The default store (first in registry).
    pub(crate) fn default_store(&self) -> &Arc<dyn Store> {
        &self.stores[0].store
    }

    /// All store entries (name + description), for listing.
    pub(crate) fn entries(&self) -> &[StoreEntry] {
        &self.stores
    }

    /// All store names joined, for hint messages.
    pub(crate) fn names_hint(&self) -> String {
        self.stores
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn config_path() -> Result<PathBuf, StoreError> {
    Ok(resolve_foray_home()?.join("config.toml"))
}

fn compute_nuance(fingerprints: &[String]) -> String {
    // Using a simple FNV-1a 64-bit hash — no crypto needed, just stability.
    let mut sorted = fingerprints.to_vec();
    sorted.sort_unstable();
    let mut hash: u64 = 0xcbf29ce484222325;
    for s in &sorted {
        for byte in s.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        // separator between entries
        hash ^= b'|' as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config(path: &str) -> RawConfig {
        RawConfig {
            stores: [(
                "local".to_string(),
                RawStoreConfig::JsonFile {
                    path: path.to_string(),
                    description: "Local".to_string(),
                },
            )]
            .into(),
        }
    }

    #[test]
    fn nuance_is_stable() {
        let raw = make_config("/home/user/.foray/journals");
        let a = StoreRegistry::compute_nuance_from_config(&raw, 1, 1);
        let b = StoreRegistry::compute_nuance_from_config(&raw, 1, 1);
        assert_eq!(a, b);
    }

    #[test]
    fn nuance_differs_on_config_change() {
        // Two distinct configs produce distinct nuances — proves the serialized
        // config feeds into the hash rather than being discarded.
        let a =
            StoreRegistry::compute_nuance_from_config(&make_config("/home/user/.foray/a"), 1, 1);
        let b =
            StoreRegistry::compute_nuance_from_config(&make_config("/home/user/.foray/b"), 1, 1);
        assert_ne!(a, b);
    }

    #[test]
    fn nuance_differs_on_schema_change() {
        // Schema version participates directly in compute_nuance_from_config.
        let raw = make_config("/home/user/.foray/journals");
        let a = StoreRegistry::compute_nuance_from_config(&raw, 1, 1);
        let b = StoreRegistry::compute_nuance_from_config(&raw, 2, 1);
        assert_ne!(a, b);
    }

    #[test]
    fn nuance_differs_on_protocol_change() {
        // Protocol version participates directly in compute_nuance_from_config.
        let raw = make_config("/home/user/.foray/journals");
        let a = StoreRegistry::compute_nuance_from_config(&raw, 1, 1);
        let b = StoreRegistry::compute_nuance_from_config(&raw, 1, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn implicit_local_builds() {
        // Just verify it doesn't panic; actual path depends on env.
        let _ = StoreRegistry::implicit_local();
    }

    #[test]
    fn load_from_rejects_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[stores.work]\ntype = \"json_file\"\npath = \"relative/path\"\ndescription = \"Work\"\n",
        )
        .unwrap();
        let err = StoreRegistry::load_from(&config_path).err().unwrap();
        let msg = err.to_string();
        assert!(msg.contains("path must be absolute"), "error was: {msg}");
    }

    #[test]
    fn load_from_rejects_missing_description() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let path_str = dir.path().join("journals").display().to_string();
        std::fs::write(
            &config_path,
            format!("[stores.work]\ntype = \"json_file\"\npath = '{path_str}'\n"),
        )
        .unwrap();
        let err = StoreRegistry::load_from(&config_path).err().unwrap();
        assert!(err.to_string().contains("description"), "error was: {err}");
    }

    #[tokio::test]
    async fn load_from_expands_tilde_in_store_path() {
        let fake_home = tempfile::tempdir().unwrap();
        let journals_dir = fake_home.path().join("journals");
        std::fs::create_dir_all(&journals_dir).unwrap();
        let _home = crate::paths::TestHomeGuard::set(fake_home.path());

        assert_eq!(expand_tilde("~/journals").unwrap(), journals_dir.as_path());

        let probe = serde_json::json!({
            "schema": crate::migrate::CURRENT_SCHEMA,
            "name": "probe",
            "title": "Probe",
            "items": []
        });
        std::fs::write(
            journals_dir.join("probe.json"),
            serde_json::to_string(&probe).unwrap(),
        )
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[stores.work]\ntype = \"json_file\"\npath = '~/journals'\ndescription = \"Work journals\"\n",
        )
        .unwrap();

        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.entries()[0].name, "work");

        let store = registry.get("work").expect("work store");
        let (summaries, total) = store.list().await.expect("list journals");
        assert_eq!(total, 1, "store must read from expanded ~/journals path");
        assert_eq!(summaries[0].name, "probe");
    }

    #[test]
    fn load_from_accepts_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let journals_dir = dir.path().join("journals");
        let config_path = dir.path().join("config.toml");
        // Use TOML literal strings (single quotes) so backslashes in Windows
        // paths are not interpreted as TOML escape sequences.
        let path_str = journals_dir.display().to_string();
        std::fs::write(
            &config_path,
            format!("[stores.work]\ntype = \"json_file\"\npath = '{path_str}'\ndescription = \"Work journals\"\n"),
        )
        .unwrap();
        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.entries()[0].name, "work");
        assert_eq!(registry.entries()[0].description, "Work journals");
    }

    #[test]
    fn load_from_foray_stdio_store() {
        // Verifies that a foray_stdio entry is parsed correctly and that
        // the store name and description are captured in the registry.
        // (args and store hint are internal to StdioStore — not exposed by the registry.)
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            "[stores.remote]\ntype = \"foray_stdio\"\ncommand = \"ssh\"\nargs = [\"user@host\", \"--\", \"foray\"]\ndescription = \"Remote store\"\nstore = \"work\"\n",
        )
        .unwrap();
        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.entries()[0].name, "remote");
        assert_eq!(registry.entries()[0].description, "Remote store");
    }

    #[test]
    fn load_from_absent_config_falls_back_to_implicit_local() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml"); // does not exist
        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.entries()[0].name, "local");
    }

    #[test]
    fn load_from_empty_stores_falls_back_to_implicit_local() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "# no stores section\n").unwrap();
        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 1);
        assert_eq!(registry.entries()[0].name, "local");
    }

    #[test]
    fn load_from_multi_store_config() {
        let dir = tempfile::tempdir().unwrap();
        let a_path = dir.path().join("a").display().to_string();
        let b_path = dir.path().join("b").display().to_string();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[stores.alpha]\ntype = \"json_file\"\npath = '{a_path}'\ndescription = \"Alpha\"\n\
                 [stores.beta]\ntype = \"json_file\"\npath = '{b_path}'\ndescription = \"Beta\"\n"
            ),
        )
        .unwrap();
        let registry = StoreRegistry::load_from(&config_path).unwrap();
        assert_eq!(registry.entries().len(), 2);
        // BTreeMap order: alpha < beta
        assert_eq!(registry.entries()[0].name, "alpha");
        assert_eq!(registry.entries()[1].name, "beta");
    }

    #[test]
    fn get_returns_none_for_unknown_name() {
        let registry = StoreRegistry::implicit_local().unwrap();
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn get_returns_entry_for_known_name() {
        let registry = StoreRegistry::implicit_local().unwrap();
        assert!(registry.get("local").is_some());
    }
}
