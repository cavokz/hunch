use crate::config::StoreRegistry;
use crate::migrate::{self, MigrateResult};
use crate::store::Store;
use crate::types::{
    ItemType, JournalFile, JournalItem, Pagination, item_id, validate_name, validate_title,
};
use chrono::Utc;
use clap::{Parser, Subcommand};
use clap_complete::Shell;
#[cfg(feature = "dynamic-completion")]
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use std::collections::HashMap;
#[cfg(feature = "dynamic-completion")]
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "foray", version, about = "Persistent investigation journals")]
pub(crate) struct Cli {
    /// Override journal name (skips env + .forayrc resolution)
    #[arg(long, global = true)]
    #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_journal_names)))]
    pub(crate) journal: Option<String>,

    /// Override store name (skips env + .forayrc resolution)
    #[arg(long, global = true)]
    #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_store_names)))]
    pub(crate) store: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Start MCP stdio server
    Serve,
    /// Show a journal with all items
    Show {
        /// Journal name (optional if resolvable)
        #[arg()]
        #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_journal_names)))]
        name: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Follow: watch for new items in real time
        #[arg(short, long)]
        follow: bool,
        /// Show an archived journal
        #[arg(long)]
        archived: bool,
    },
    /// Add an item to the current journal
    Add {
        /// Item content
        content: String,
        /// Item type: finding, decision, snippet, note
        #[arg(long, name = "type", default_value = "note")]
        item_type: String,
        /// External reference (URL, file path, ticket/PR link, etc.)
        #[arg(long, name = "ref")]
        item_ref: Option<String>,
        /// Comma-separated tags
        #[arg(long)]
        tags: Option<String>,
        /// Metadata key=value pairs
        #[arg(long = "meta", value_name = "KEY=VALUE")]
        meta: Vec<String>,
    },
    /// Create a new journal
    Create {
        /// Journal name
        name: String,
        /// Title (required)
        #[arg(long)]
        title: String,
        /// Metadata key=value pairs
        #[arg(long = "meta", value_name = "KEY=VALUE")]
        meta: Vec<String>,
    },
    /// List journals
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show archived journals
        #[arg(long)]
        archived: bool,
        /// Output bare journal names for shell completion (one per line)
        #[arg(long, conflicts_with = "json")]
        completion: bool,
    },
    /// Delete a journal permanently
    Delete {
        /// Journal name
        #[arg()]
        #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_journal_names)))]
        name: String,
        /// Delete an archived journal
        #[arg(long)]
        archived: bool,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
    /// Archive a journal
    Archive {
        /// Journal name
        #[arg()]
        #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_journal_names)))]
        name: String,
    },
    /// Unarchive a journal
    Unarchive {
        /// Journal name
        #[arg()]
        #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_archived_journal_names)))]
        name: String,
    },
    /// Export journal JSON to stdout or file
    Export {
        /// Journal name
        #[arg()]
        #[cfg_attr(feature = "dynamic-completion", arg(add = ArgValueCompleter::new(complete_journal_names)))]
        name: String,
        /// Output file (default: stdout)
        #[arg(long)]
        file: Option<PathBuf>,
        /// Export an archived journal
        #[arg(long)]
        archived: bool,
    },
    /// Import journal JSON from stdin or file
    Import {
        /// Destination journal name
        name: String,
        /// Input file (default: stdin)
        #[arg(long)]
        file: Option<PathBuf>,
        /// Merge items into an existing journal (skips items whose ID already exists)
        #[arg(long)]
        merge: bool,
        /// Create the imported journal as archived
        #[arg(long, conflicts_with = "merge")]
        archived: bool,
    },
    /// Generate shell completion script
    #[cfg_attr(
        not(feature = "dynamic-completion"),
        command(after_help = "\
ACTIVATION:
  bash:       eval \"$(COMPLETE=bash foray)\"
              # or persist: COMPLETE=bash foray >> ~/.bash_completion

  zsh:        Add to ~/.zshrc, AFTER compinit:
                eval \"$(COMPLETE=zsh foray)\"
              # Example ~/.zshrc order:
              #   autoload -Uz compinit && compinit
              #   eval \"$(COMPLETE=zsh foray)\"

  fish:       COMPLETE=fish foray | source
              # or persist: COMPLETE=fish foray > ~/.config/fish/completions/foray.fish

  powershell: & { $env:COMPLETE='powershell'; foray } | Invoke-Expression
              # or append the output to $PROFILE

  elvish:     eval (COMPLETE=elvish foray | slurp)
              # or persist: COMPLETE=elvish foray > ~/.config/elvish/lib/foray-complete.elv
              #   then add to rc.elv: use foray-complete

NOTE: this binary completes subcommands and flags only.
For store and journal name completion, rebuild with:
  cargo build --features dynamic-completion
")
    )]
    #[cfg_attr(
        feature = "dynamic-completion",
        command(after_help = "\
ACTIVATION (completes subcommands, flags, store names and journal names):
  bash:       eval \"$(COMPLETE=bash foray)\"
              # or persist: COMPLETE=bash foray >> ~/.bash_completion

  zsh:        Add to ~/.zshrc, AFTER compinit:
                eval \"$(COMPLETE=zsh foray)\"
              # Example ~/.zshrc order:
              #   autoload -Uz compinit && compinit
              #   eval \"$(COMPLETE=zsh foray)\"

  fish:       COMPLETE=fish foray | source
              # or persist: COMPLETE=fish foray > ~/.config/fish/completions/foray.fish

  powershell: & { $env:COMPLETE='powershell'; foray } | Invoke-Expression
              # or append the output to $PROFILE

  elvish:     eval (COMPLETE=elvish foray | slurp)
              # or persist: COMPLETE=elvish foray > ~/.config/elvish/lib/foray-complete.elv
              #   then add to rc.elv: use foray-complete
")
    )]
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

/// Resolve journal name from CLI flag, env, or .forayrc walk-up.
fn resolve_journal(cli_flag: Option<&str>, explicit_name: Option<&str>) -> anyhow::Result<String> {
    let name = if let Some(name) = explicit_name {
        name.to_string()
    } else if let Some(name) = cli_flag {
        name.to_string()
    } else if let Ok(name) = std::env::var("FORAY_JOURNAL")
        && !name.is_empty()
    {
        name
    } else if let Some(name) = find_forayrc(&std::env::current_dir()?) {
        name
    } else {
        anyhow::bail!(
            "no journal specified. Use --journal <name>, set FORAY_JOURNAL, \
             or add `current-journal = <name>` to a .forayrc file"
        )
    };
    validate_name(&name).map_err(|e| anyhow::anyhow!(e))?;
    Ok(name)
}

/// Walk up from `start_dir` looking for `.forayrc` with `current-journal`.
fn find_forayrc(start_dir: &std::path::Path) -> Option<String> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let rc_path = dir.join(".forayrc");
        if rc_path.is_file()
            && let Ok(contents) = std::fs::read_to_string(&rc_path)
            && let Ok(table) = contents.parse::<toml::Table>()
        {
            if let Some(name) = table.get("current-journal").and_then(|v| v.as_str()) {
                return Some(name.to_string());
            }
            if table.get("root").and_then(|v| v.as_bool()) == Some(true) {
                return None;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Resolve which store to use: CLI flag > FORAY_STORE env > .forayrc current-store >
/// implicit default (only when exactly one store is configured) > error.
pub(crate) fn resolve_store<'a>(
    registry: &'a StoreRegistry,
    cli_flag: Option<&str>,
) -> anyhow::Result<&'a dyn Store> {
    let name: Option<String> = if let Some(n) = cli_flag {
        Some(n.to_string())
    } else if let Ok(n) = std::env::var("FORAY_STORE")
        && !n.is_empty()
    {
        Some(n)
    } else {
        find_store_in_forayrc(&std::env::current_dir()?)
    };

    match name {
        None => {
            if registry.entries().len() == 1 {
                Ok(registry.default_store().as_ref())
            } else {
                Err(anyhow::anyhow!(
                    "no store specified. Use --store <name>, set FORAY_STORE, or add current-store to .forayrc (available: {})",
                    registry.names_hint()
                ))
            }
        }
        Some(n) => registry.get(&n).map(|s| s.as_ref()).ok_or_else(|| {
            anyhow::anyhow!(
                "store '{n}' not found. Available: {}",
                registry.names_hint()
            )
        }),
    }
}

/// Walk up from `start_dir` looking for `.forayrc` with `current-store`.
fn find_store_in_forayrc(start_dir: &std::path::Path) -> Option<String> {
    let mut dir = start_dir.to_path_buf();
    loop {
        let rc_path = dir.join(".forayrc");
        if rc_path.is_file()
            && let Ok(contents) = std::fs::read_to_string(&rc_path)
            && let Ok(table) = contents.parse::<toml::Table>()
        {
            if let Some(name) = table.get("current-store").and_then(|v| v.as_str()) {
                return Some(name.to_string());
            }
            if table.get("root").and_then(|v| v.as_bool()) == Some(true) {
                return None;
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Write or update `.forayrc` in the current directory.
#[cfg(test)]
fn write_forayrc(name: &str, store: Option<&str>) -> anyhow::Result<()> {
    let rc_path = std::env::current_dir()?.join(".forayrc");
    let mut table = if rc_path.is_file() {
        let contents = std::fs::read_to_string(&rc_path)?;
        contents.parse::<toml::Table>().unwrap_or_default()
    } else {
        toml::Table::new()
    };
    table.insert("current-journal".into(), toml::Value::String(name.into()));
    if let Some(s) = store {
        table.insert("current-store".into(), toml::Value::String(s.into()));
    }
    std::fs::write(&rc_path, toml::to_string_pretty(&table)?)?;
    Ok(())
}

/// Parse `--meta KEY=VALUE` pairs into a HashMap.
fn parse_meta(pairs: &[String]) -> Option<HashMap<String, serde_json::Value>> {
    if pairs.is_empty() {
        return None;
    }
    let map: HashMap<String, serde_json::Value> = pairs
        .iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.to_string(), serde_json::Value::String(v.to_string())))
        })
        .collect();
    if map.is_empty() { None } else { Some(map) }
}

fn parse_item_type(s: &str) -> anyhow::Result<ItemType> {
    match s {
        "finding" => Ok(ItemType::Finding),
        "decision" => Ok(ItemType::Decision),
        "snippet" => Ok(ItemType::Snippet),
        "note" => Ok(ItemType::Note),
        other => {
            anyhow::bail!("unknown item type: {other}. Valid: finding, decision, snippet, note")
        }
    }
}

// ── Shell completion candidates ──────────────────────────────────────

#[cfg(feature = "dynamic-completion")]
fn complete_store_names(_current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(registry) = StoreRegistry::load() else {
        return vec![];
    };
    registry
        .entries()
        .iter()
        .map(|e| CompletionCandidate::new(&e.name))
        .collect()
}

#[cfg(feature = "dynamic-completion")]
fn complete_journal_names(_current: &OsStr) -> Vec<CompletionCandidate> {
    journal_names_as_candidates(false)
}

#[cfg(feature = "dynamic-completion")]
fn complete_archived_journal_names(_current: &OsStr) -> Vec<CompletionCandidate> {
    journal_names_as_candidates(true)
}

#[cfg(feature = "dynamic-completion")]
fn journal_names_as_candidates(archived: bool) -> Vec<CompletionCandidate> {
    let Ok(exe) = std::env::current_exe() else {
        return vec![];
    };
    let store_name = std::env::var("FORAY_STORE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| find_store_in_forayrc(&d))
        });
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("list").arg("--completion");
    if archived {
        cmd.arg("--archived");
    }
    if let Some(store) = &store_name {
        cmd.arg("--store").arg(store);
    }
    // Unset COMPLETE so the subprocess runs normally rather than completing.
    cmd.env_remove("COMPLETE");
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let Ok(mut child) = cmd.spawn() else {
        return vec![];
    };
    let stdout = child.stdout.take();

    // Read stdout in a thread so we can enforce a timeout on slow/remote stores.
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            use std::io::Read;
            let _ = out.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });

    let buf = match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(buf) => buf,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return vec![];
        }
    };
    let _ = child.wait();

    let Ok(stdout) = String::from_utf8(buf) else {
        return vec![];
    };
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(CompletionCandidate::new)
        .collect()
}

fn print_item(item: &JournalItem) {
    let type_str = format!("{:?}", item.item_type).to_lowercase();
    println!(
        "[{}] ({}) {}",
        item.added_at.format("%Y-%m-%d %H:%M"),
        type_str,
        item.content
    );
    if let Some(r) = item
        .meta
        .as_ref()
        .and_then(|m| m.get("ref"))
        .and_then(|v| v.as_str())
    {
        println!("  ref: {r}");
    }
    if let Some(tags) = &item.tags {
        println!("  tags: {}", tags.join(", "));
    }
}

/// Execute a CLI command against the store.
pub(crate) async fn run(cli: &Cli, store: &dyn Store) -> anyhow::Result<()> {
    match &cli.command {
        Commands::Serve => {
            unreachable!("serve is handled in main")
        }
        Commands::Completions { .. } => {
            unreachable!("completions is handled in main")
        }
        Commands::Show {
            name,
            json,
            follow,
            archived,
        } => {
            let journal_name = resolve_journal(cli.journal.as_deref(), name.as_deref())?;
            let (journal, total) = store
                .load(&journal_name, &Pagination::all(), *archived)
                .await?;
            if *json {
                for item in &journal.items {
                    println!("{}", serde_json::to_string(item)?);
                }
            } else {
                println!("Journal: {}", journal.name);
                println!("Title:   {}", journal.title);
                println!("Items:   {} / {total}", journal.items.len());
                println!();
                for item in &journal.items {
                    print_item(item);
                }
            }
            if *follow {
                let mut seen = total;
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let all = Pagination {
                        from: seen,
                        size: usize::MAX,
                    };
                    let (journal, new_total) = store.load(&journal_name, &all, *archived).await?;
                    if new_total > seen {
                        for item in &journal.items {
                            if *json {
                                println!("{}", serde_json::to_string(item).unwrap());
                            } else {
                                print_item(item);
                            }
                        }
                        seen = new_total;
                    }
                }
            }
        }
        Commands::Add {
            content,
            item_type,
            item_ref,
            tags,
            meta,
        } => {
            let journal_name = resolve_journal(cli.journal.as_deref(), None)?;
            let it = parse_item_type(item_type)?;
            let parsed_tags = tags.as_ref().map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<Vec<_>>()
            });
            let mut parsed_meta = parse_meta(meta);
            if let Some(r) = item_ref {
                parsed_meta
                    .get_or_insert_with(HashMap::new)
                    .entry("ref".to_string())
                    .or_insert_with(|| serde_json::Value::String(r.clone()));
            }
            let item = JournalItem {
                id: item_id(),
                item_type: it,
                content: content.clone(),
                tags: parsed_tags,
                added_at: Utc::now(),
                meta: parsed_meta,
            };
            let failed = store.add_items(&journal_name, vec![item], false).await?;
            if !failed.is_empty() {
                return Err(anyhow::anyhow!(
                    "failed to add item to {journal_name}: ID collision after store rejected it"
                ));
            }
            println!("Added to {journal_name}");
        }
        Commands::Create { name, title, meta } => {
            validate_name(name).map_err(|e| anyhow::anyhow!(e))?;
            let meta = parse_meta(meta);
            let title = validate_title(title).map_err(|e| anyhow::anyhow!(e))?;
            store.create(name, title, meta).await?;
            println!("Created journal: {name}");
        }
        Commands::List {
            json,
            archived,
            completion,
        } => {
            let (all_summaries, _) = store.list().await?;
            let summaries: Vec<_> = all_summaries
                .into_iter()
                .filter(|s| s.archived == *archived)
                .collect();
            let total = summaries.len();

            if *completion {
                for s in &summaries {
                    if s.error.is_none() {
                        println!("{}", s.name);
                    }
                }
                return Ok(());
            }

            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"total": total, "journals": &summaries})
                    )?
                );
            } else {
                let label = if *archived { "archived" } else { "active" };
                println!("{} journal(s) ({label}):", total);
                for s in &summaries {
                    if let Some(err) = &s.error {
                        println!("  {} (ERROR: {})", s.name, err);
                    } else {
                        let title = &s.title;
                        println!("  {} ({} items) {}", s.name, s.item_count, title);
                    }
                }
            }
        }
        Commands::Delete {
            name,
            archived,
            force,
        } => {
            validate_name(name).map_err(|e| anyhow::anyhow!(e))?;
            let (summaries, _) = store.list().await?;
            let exists_here = summaries
                .iter()
                .any(|s| s.name == *name && s.archived == *archived);
            if !exists_here {
                let exists_elsewhere = summaries
                    .iter()
                    .any(|s| s.name == *name && s.archived != *archived);
                return Err(if exists_elsewhere && *archived {
                    anyhow::anyhow!(
                        "journal '{name}' not found in archived location; it may be active (omit --archived)"
                    )
                } else if exists_elsewhere {
                    anyhow::anyhow!(
                        "journal '{name}' not found; it may be archived (use --archived)"
                    )
                } else {
                    anyhow::anyhow!("journal '{name}' not found")
                });
            }
            if !*force {
                eprint!("Delete journal '{name}'? [y/N] ");
                std::io::stderr().flush()?;
                let mut input = String::new();
                std::io::stdin().read_line(&mut input)?;
                if input.trim().to_lowercase() != "y" {
                    println!("Aborted.");
                    return Ok(());
                }
            }
            store.delete(name, *archived).await?;
            println!("Deleted: {name}");
        }
        Commands::Archive { name } => {
            store.archive(name).await?;
            println!("Archived: {name}");
        }
        Commands::Unarchive { name } => {
            store.unarchive(name).await?;
            println!("Unarchived: {name}");
        }
        Commands::Export {
            name,
            file,
            archived,
        } => {
            validate_name(name).map_err(|e| anyhow::anyhow!(e))?;
            let (journal, _) = store
                .load(name, &Pagination::all(), *archived)
                .await
                .map_err(|e| match e {
                    crate::store::StoreError::NotFound(_) if *archived => {
                        anyhow::anyhow!("journal '{name}' not found in archived location; it may be active (omit --archived)")
                    }
                    crate::store::StoreError::NotFound(_) => {
                        anyhow::anyhow!("journal '{name}' not found; it may be archived (use --archived)")
                    }
                    e => e.into(),
                })?;
            let data = serde_json::to_string_pretty(&journal)?;
            match file {
                Some(path) => std::fs::write(path, format!("{data}\n"))?,
                None => println!("{data}"),
            }
        }
        Commands::Import {
            name,
            file,
            merge,
            archived,
        } => {
            validate_name(name).map_err(|e| anyhow::anyhow!(e))?;
            let data = match file {
                Some(path) => std::fs::read_to_string(path)?,
                None => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let raw: serde_json::Value = serde_json::from_str(&data)?;
            let value = match migrate::migrate(raw) {
                MigrateResult::Current(v) | MigrateResult::Migrated(v) => v,
                MigrateResult::TooNew { found, max } => {
                    return Err(anyhow::anyhow!(
                        "journal schema {found} is too new (max supported: {max})"
                    ));
                }
                MigrateResult::Invalid => {
                    return Err(anyhow::anyhow!("journal file is not a JSON object"));
                }
            };
            let journal: JournalFile = serde_json::from_value(value)?;
            if !*merge {
                validate_title(&journal.title).map_err(|e| anyhow::anyhow!(e))?;
            }
            let (added, skipped) = store.import(name, journal, *merge, *archived).await?;
            if skipped > 0 {
                eprintln!("warning: skipped {skipped} item(s) already present in {name}");
            }
            if *merge {
                println!("Merged {added} item(s) into {name}");
            } else if *archived {
                println!("Imported {added} item(s) as archived: {name}");
            } else {
                println!("Imported {added} item(s) as {name}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    // Serialize tests that mutate process-global state (env vars, cwd) to avoid races.
    static SERIAL_LOCK: Mutex<()> = Mutex::const_new(());

    #[test]
    fn test_find_forayrc() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc_path = dir.path().join(".forayrc");
        std::fs::write(&rc_path, "current-journal = \"test-journal\"\n").unwrap();
        assert_eq!(find_forayrc(dir.path()), Some("test-journal".into()));
    }

    #[test]
    fn test_find_forayrc_root_stops_walk() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc_path = dir.path().join(".forayrc");
        std::fs::write(&rc_path, "root = true\n").unwrap();
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).unwrap();
        assert_eq!(find_forayrc(&child), None);
    }

    #[test]
    fn write_forayrc_persists_store_when_given() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let dir = tempfile::TempDir::new().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        write_forayrc("my-journal", Some("remote")).unwrap();
        let contents = std::fs::read_to_string(dir.path().join(".forayrc")).unwrap();
        let table: toml::Table = contents.parse().unwrap();
        assert_eq!(table["current-journal"].as_str(), Some("my-journal"));
        assert_eq!(table["current-store"].as_str(), Some("remote"));
    }

    #[test]
    fn write_forayrc_omits_store_when_none() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let dir = tempfile::TempDir::new().unwrap();
        let _cwd = CwdGuard::set(dir.path());
        write_forayrc("my-journal", None).unwrap();
        let contents = std::fs::read_to_string(dir.path().join(".forayrc")).unwrap();
        let table: toml::Table = contents.parse().unwrap();
        assert_eq!(table["current-journal"].as_str(), Some("my-journal"));
        assert!(!table.contains_key("current-store"));
    }

    #[test]
    fn test_parse_meta() {
        let pairs = vec!["key1=value1".into(), "key2=value2".into()];
        let meta = parse_meta(&pairs).unwrap();
        assert_eq!(meta.get("key1").unwrap(), "value1");
        assert_eq!(meta.get("key2").unwrap(), "value2");
        assert!(parse_meta(&[]).is_none());
    }

    #[test]
    fn test_parse_item_type() {
        assert_eq!(parse_item_type("finding").unwrap(), ItemType::Finding);
        assert_eq!(parse_item_type("decision").unwrap(), ItemType::Decision);
        assert_eq!(parse_item_type("snippet").unwrap(), ItemType::Snippet);
        assert_eq!(parse_item_type("note").unwrap(), ItemType::Note);
        assert!(parse_item_type("invalid").is_err());
    }

    fn make_registry() -> (StoreRegistry, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let registry = StoreRegistry::for_test(dir.path().to_path_buf());
        (registry, dir)
    }

    #[test]
    fn resolve_store_uses_cli_flag() {
        let (registry, _dir) = make_registry();
        // "local" is the default name in for_test
        assert!(resolve_store(&registry, Some("local")).is_ok());
    }

    #[test]
    fn resolve_store_cli_flag_beats_env_var() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let (registry, _dir) = make_registry();
        unsafe {
            std::env::set_var("FORAY_STORE", "some-other-store");
        }
        let result = resolve_store(&registry, Some("local"));
        unsafe {
            std::env::remove_var("FORAY_STORE");
        }
        // CLI flag wins even though FORAY_STORE is set to an unknown store
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_store_cli_flag_unknown_errors() {
        let (registry, _dir) = make_registry();
        let result = resolve_store(&registry, Some("no-such-store"));
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("not found"));
        assert!(msg.contains("no-such-store"));
    }

    #[test]
    fn resolve_store_env_var() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let (registry, _dir) = make_registry();
        // env var wins when no CLI flag
        unsafe {
            std::env::set_var("FORAY_STORE", "local");
        }
        let result = resolve_store(&registry, None);
        unsafe {
            std::env::remove_var("FORAY_STORE");
        }
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_store_env_var_unknown_errors() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let (registry, _dir) = make_registry();
        unsafe {
            std::env::set_var("FORAY_STORE", "nope");
        }
        let result = resolve_store(&registry, None);
        unsafe {
            std::env::remove_var("FORAY_STORE");
        }
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("not found"));
    }

    #[test]
    fn resolve_store_forayrc() {
        let (_registry, rc_dir) = make_registry();
        let rc_path = rc_dir.path().join(".forayrc");
        std::fs::write(&rc_path, "current-store = \"local\"\n").unwrap();
        let found = find_store_in_forayrc(rc_dir.path());
        assert_eq!(found, Some("local".to_string()));
    }

    #[test]
    fn resolve_store_falls_back_to_default() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let (registry, dir) = make_registry();
        // Stop find_store_in_forayrc() from walking up into parent dirs.
        std::fs::write(dir.path().join(".forayrc"), "root = true\n").unwrap();
        let _cwd = CwdGuard::set(dir.path());
        let prior_foray_store = std::env::var("FORAY_STORE").ok();
        unsafe {
            std::env::remove_var("FORAY_STORE");
        }
        // Single-store registry: implicit default is returned.
        let result = resolve_store(&registry, None);
        unsafe {
            match prior_foray_store {
                Some(v) => std::env::set_var("FORAY_STORE", v),
                None => std::env::remove_var("FORAY_STORE"),
            }
        }
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_store_errors_without_spec_when_multiple_stores() {
        let _guard = SERIAL_LOCK.blocking_lock();
        let dir1 = tempfile::TempDir::new().unwrap();
        let dir2 = tempfile::TempDir::new().unwrap();
        let registry =
            StoreRegistry::for_test_two(dir1.path().to_path_buf(), dir2.path().to_path_buf());
        // Stop find_store_in_forayrc() from walking up into parent dirs.
        std::fs::write(dir1.path().join(".forayrc"), "root = true\n").unwrap();
        let _cwd = CwdGuard::set(dir1.path());
        let prior_foray_store = std::env::var("FORAY_STORE").ok();
        unsafe {
            std::env::remove_var("FORAY_STORE");
        }
        let result = resolve_store(&registry, None);
        unsafe {
            match prior_foray_store {
                Some(v) => std::env::set_var("FORAY_STORE", v),
                None => std::env::remove_var("FORAY_STORE"),
            }
        }
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("no store specified"));
        assert!(msg.contains("available:"));
    }

    #[test]
    fn find_store_in_forayrc_root_stops_walk() {
        let dir = tempfile::TempDir::new().unwrap();
        let rc_path = dir.path().join(".forayrc");
        std::fs::write(&rc_path, "root = true\n").unwrap();
        let child = dir.path().join("sub");
        std::fs::create_dir(&child).unwrap();
        assert_eq!(find_store_in_forayrc(&child), None);
    }

    fn make_cli(command: Commands) -> Cli {
        Cli {
            journal: None,
            store: None,
            command,
        }
    }

    // RAII guard that restores the process cwd on drop, even on panic.
    struct CwdGuard(std::path::PathBuf);
    impl CwdGuard {
        fn set(dir: &std::path::Path) -> Self {
            let orig = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self(orig)
        }
    }
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    #[tokio::test]
    async fn create_rejects_empty_title() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let cli = make_cli(Commands::Create {
            name: "my-journal".to_string(),
            title: "".to_string(),
            meta: vec![],
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[tokio::test]
    async fn create_rejects_whitespace_only_title() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let cli = make_cli(Commands::Create {
            name: "my-journal".to_string(),
            title: "   ".to_string(),
            meta: vec![],
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[tokio::test]
    async fn create_rejects_duplicate_journal() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let make = || {
            make_cli(Commands::Create {
                name: "my-journal".to_string(),
                title: "My Journal".to_string(),
                meta: vec![],
            })
        };
        run(&make(), &store)
            .await
            .expect("first create should succeed");
        let err = run(&make(), &store).await.unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("already exists"),
            "expected 'already exists' in error, got: {err}"
        );
    }

    fn make_export_json(
        name: &str,
        title: &str,
        items: &[(&str, &str)],
    ) -> tempfile::NamedTempFile {
        use crate::types::{ItemType, JournalFile, JournalItem};
        let file_items: Vec<JournalItem> = items
            .iter()
            .map(|(id, content)| JournalItem {
                id: id.to_string(),
                item_type: ItemType::Note,
                content: content.to_string(),
                tags: None,
                added_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                meta: None,
            })
            .collect();
        let journal = JournalFile {
            schema: crate::migrate::CURRENT_SCHEMA,
            name: name.to_string(),
            title: title.to_string(),
            items: file_items,
            meta: None,
        };
        let f = tempfile::NamedTempFile::new().unwrap();
        serde_json::to_writer_pretty(&f, &journal).unwrap();
        f
    }

    #[tokio::test]
    async fn import_creates_new_journal() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let f = make_export_json("my-journal", "My Title", &[("aaa-bbb", "item one")]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: false,
            archived: false,
        });
        run(&cli, &store).await.unwrap();
        let (journal, total) = store
            .load("my-journal", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(journal.title, "My Title");
        assert_eq!(journal.items[0].content, "item one");
    }

    #[tokio::test]
    async fn import_preserves_added_at() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let f = make_export_json("ts-journal", "TS Test", &[("ts-id-1", "timestamped")]);
        let cli = make_cli(Commands::Import {
            name: "ts-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: false,
            archived: false,
        });
        run(&cli, &store).await.unwrap();
        let (journal, _) = store
            .load("ts-journal", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(
            journal.items[0].added_at.to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
    }

    #[tokio::test]
    async fn import_fails_if_journal_already_exists() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "Existing".to_string(), None)
            .await
            .unwrap();
        let f = make_export_json("irrelevant", "My Title", &[]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: false,
            archived: false,
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[tokio::test]
    async fn import_merge_appends_new_items() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "Existing".to_string(), None)
            .await
            .unwrap();
        store
            .add_items(
                "my-journal",
                vec![JournalItem {
                    id: "existing-id".to_string(),
                    item_type: ItemType::Note,
                    content: "already here".to_string(),
                    tags: None,
                    added_at: Utc::now(),
                    meta: None,
                }],
                false,
            )
            .await
            .unwrap();
        let f = make_export_json("my-journal", "Title", &[("new-id", "new item")]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: true,
            archived: false,
        });
        run(&cli, &store).await.unwrap();
        let (journal, total) = store
            .load("my-journal", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(total, 2);
        assert!(journal.items.iter().any(|i| i.content == "already here"));
        assert!(journal.items.iter().any(|i| i.content == "new item"));
    }

    #[tokio::test]
    async fn import_merge_skips_duplicate_ids() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "Existing".to_string(), None)
            .await
            .unwrap();
        store
            .add_items(
                "my-journal",
                vec![JournalItem {
                    id: "dup-id".to_string(),
                    item_type: ItemType::Note,
                    content: "original".to_string(),
                    tags: None,
                    added_at: Utc::now(),
                    meta: None,
                }],
                false,
            )
            .await
            .unwrap();
        let f = make_export_json("my-journal", "Title", &[("dup-id", "duplicate")]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: true,
            archived: false,
        });
        run(&cli, &store).await.unwrap();
        let (journal, total) = store
            .load("my-journal", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(journal.items[0].content, "original");
    }

    #[tokio::test]
    async fn import_merge_fails_if_journal_missing() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let f = make_export_json("my-journal", "Title", &[("some-id", "item")]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: true,
            archived: false,
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn import_archived_creates_archived_journal() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let f = make_export_json("my-journal", "My Title", &[("id-1", "item one")]);
        let cli = make_cli(Commands::Import {
            name: "my-journal".to_string(),
            file: Some(f.path().to_path_buf()),
            merge: false,
            archived: true,
        });
        run(&cli, &store).await.unwrap();
        let (all, _) = store.list().await.unwrap();
        assert!(!all.iter().any(|s| !s.archived && s.name == "my-journal"));
        assert!(all.iter().any(|s| s.archived && s.name == "my-journal"));
    }

    #[tokio::test]
    async fn export_active_without_flag_works() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "My Title".to_string(), None)
            .await
            .unwrap();
        let out = tempfile::NamedTempFile::new().unwrap();
        let cli = make_cli(Commands::Export {
            name: "my-journal".to_string(),
            file: Some(out.path().to_path_buf()),
            archived: false,
        });
        run(&cli, &store).await.unwrap();
    }

    #[tokio::test]
    async fn export_active_with_archived_flag_errors() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "My Title".to_string(), None)
            .await
            .unwrap();
        let cli = make_cli(Commands::Export {
            name: "my-journal".to_string(),
            file: None,
            archived: true,
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(
            err.to_string().contains("not found") && err.to_string().contains("omit --archived"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn export_archived_without_flag_errors() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "My Title".to_string(), None)
            .await
            .unwrap();
        store.archive("my-journal").await.unwrap();
        let cli = make_cli(Commands::Export {
            name: "my-journal".to_string(),
            file: None,
            archived: false,
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(
            err.to_string().contains("not found") && err.to_string().contains("use --archived"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn export_archived_with_flag_works() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        store
            .create("my-journal", "My Title".to_string(), None)
            .await
            .unwrap();
        store.archive("my-journal").await.unwrap();
        let out = tempfile::NamedTempFile::new().unwrap();
        let cli = make_cli(Commands::Export {
            name: "my-journal".to_string(),
            file: Some(out.path().to_path_buf()),
            archived: true,
        });
        run(&cli, &store).await.unwrap();
    }

    #[tokio::test]
    async fn export_nonexistent_errors() {
        let _guard = SERIAL_LOCK.lock().await;
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::store_json::JsonFileStore::new(dir.path().to_path_buf());
        let _cwd = CwdGuard::set(dir.path());
        let cli = make_cli(Commands::Export {
            name: "no-such-journal".to_string(),
            file: None,
            archived: false,
        });
        let err = run(&cli, &store).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }
}
