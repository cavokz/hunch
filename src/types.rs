use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The schema version produced by this build.
pub const CURRENT_SCHEMA: u32 = 1;

/// Allowed item types in a journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Finding,
    Decision,
    Snippet,
    Note,
}

/// A single entry inside a journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalItem {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: ItemType,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    pub added_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<HashMap<String, serde_json::Value>>,
}

/// The top-level journal file stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalFile {
    /// Schema version. Always call [`crate::migrate::migrate`] before deserializing —
    /// migration guarantees this field is present and at the current version.
    pub schema: u32,
    pub name: String,
    pub title: String,
    pub items: Vec<JournalItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<HashMap<String, serde_json::Value>>,
}

impl JournalFile {
    pub fn new(
        name: &str,
        title: String,
        meta: Option<HashMap<String, serde_json::Value>>,
    ) -> Self {
        Self {
            schema: CURRENT_SCHEMA,
            name: name.to_string(),
            title,
            items: Vec::new(),
            meta,
        }
    }
}

/// Summary returned by `list_journals`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalSummary {
    pub name: String,
    pub title: String,
    pub item_count: usize,
    pub archived: bool,
    /// Average serialized byte size of items in this journal.
    /// `None` if the journal is empty or the server does not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_item_size: Option<usize>,
    /// Standard deviation of serialized item sizes.
    /// `None` for empty journals, old servers (protocol 0), or unreadable journals (`error` set).
    /// `Some(0)` for single-item journals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub std_item_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<&JournalFile> for JournalSummary {
    fn from(j: &JournalFile) -> Self {
        let n = j.items.len();
        let (avg_item_size, std_item_size) = if n == 0 {
            (None, None)
        } else {
            // Welford's online algorithm for mean and population variance.
            // ByteCounter avoids allocating a buffer per item.
            struct ByteCounter(usize);
            impl std::io::Write for ByteCounter {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0 += buf.len();
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut count = 0usize;
            let mut mean = 0.0f64;
            let mut m2 = 0.0f64;
            for item in &j.items {
                let mut counter = ByteCounter(0);
                serde_json::to_writer(&mut counter, item)
                    .expect("serialization to ByteCounter cannot fail");
                let x = counter.0 as f64;
                count += 1;
                let delta = x - mean;
                mean += delta / count as f64;
                m2 += delta * (x - mean);
            }
            let avg = mean.ceil() as usize;
            // For n==1 the population std dev is 0 by definition (no variance in a single sample).
            let std = if n < 2 {
                Some(0)
            } else {
                Some((m2 / n as f64).sqrt().ceil() as usize)
            };
            (Some(avg), std)
        };
        Self {
            name: j.name.clone(),
            title: j.title.clone(),
            item_count: n,
            archived: false, // populated by the store after reading location
            avg_item_size,
            std_item_size,
            schema: Some(j.schema),
            meta: j.meta.clone(),
            error: None,
        }
    }
}

/// Pagination parameters for `sync_journal` (load) operations.
#[derive(Debug, Clone)]
pub struct Pagination {
    pub from: usize,
    pub size: usize,
}

impl Pagination {
    /// Returns a `Pagination` that spans all items: starts at offset 0 with no
    /// size limit (`size = usize::MAX`).
    pub fn all() -> Self {
        Self {
            from: 0,
            size: usize::MAX,
        }
    }

    /// Apply pagination to a slice, returning the page and total count.
    pub fn apply<T: Clone>(&self, items: &[T]) -> (Vec<T>, usize) {
        let total = items.len();
        let offset = self.from.min(total);
        let remaining = &items[offset..];
        let page = &remaining[..self.size.min(remaining.len())];
        (page.to_vec(), total)
    }
}

const CONSONANTS: &[u8] = b"bcdfghjklmnpqrstvwxyz";

fn random_consonants(n: usize) -> String {
    let mut rng = rand::rng();
    (0..n)
        .map(|_| CONSONANTS[rng.random_range(0..CONSONANTS.len())] as char)
        .collect()
}

/// Generate an item ID in `xxxx-xxxx-xxxx-xxxx` format (16 consonants, 4 groups of 4).
pub fn item_id() -> String {
    let c = random_consonants(16);
    format!("{}-{}-{}-{}", &c[..4], &c[4..8], &c[8..12], &c[12..16])
}

const MAX_TITLE: usize = 512;

/// Validate and normalise a journal title: trim whitespace, non-empty, max 512 Unicode chars.
/// Returns the trimmed title on success.
pub fn validate_title(title: &str) -> Result<String, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("title must not be empty".into());
    }
    let char_count = trimmed.chars().count();
    if char_count > MAX_TITLE {
        return Err(format!(
            "title exceeds {MAX_TITLE} char limit ({char_count} chars)",
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a journal name: `[a-z0-9_-]`, non-empty, max 256 chars.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("journal name cannot be empty".into());
    }
    if name.len() > 256 {
        return Err("journal name cannot exceed 256 characters".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err("journal name may only contain [a-z0-9_-]".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_titles() {
        let t = validate_title("My Journal").unwrap();
        assert_eq!(t, "My Journal");
    }

    #[test]
    fn title_trimmed() {
        let t = validate_title("  spaces  ").unwrap();
        assert_eq!(t, "spaces");
    }

    #[test]
    fn title_rejects_empty() {
        let e = validate_title("").unwrap_err();
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn title_rejects_whitespace_only() {
        let e = validate_title("   ").unwrap_err();
        assert!(e.contains("empty"), "{e}");
    }

    #[test]
    fn title_rejects_too_long() {
        let long = "a".repeat(MAX_TITLE + 1);
        let e = validate_title(&long).unwrap_err();
        assert!(e.contains("exceeds"), "{e}");
    }

    #[test]
    fn title_accepts_max_len() {
        let exact = "a".repeat(MAX_TITLE);
        assert!(validate_title(&exact).is_ok());
    }

    #[test]
    fn title_counts_unicode_chars_not_bytes() {
        // "é" is 2 bytes but 1 char — 512 of them must be accepted, 513 rejected.
        let ok = "é".repeat(MAX_TITLE);
        assert!(
            validate_title(&ok).is_ok(),
            "512 multibyte chars should be accepted"
        );
        let too_long = "é".repeat(MAX_TITLE + 1);
        let e = validate_title(&too_long).unwrap_err();
        assert!(e.contains("exceeds"), "{e}");
    }

    #[test]
    fn valid_names() {
        assert!(validate_name("auth-triage").is_ok());
        assert!(validate_name("perf_deep_dive").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("abc-123_def").is_ok());
    }

    #[test]
    fn invalid_names() {
        assert!(validate_name("").is_err());
        assert!(validate_name("Auth").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("has.dot").is_err());
        assert!(validate_name(&"a".repeat(256)).is_ok());
        assert!(validate_name(&"a".repeat(257)).is_err());
    }

    #[test]
    fn pagination_apply() {
        let items: Vec<i32> = (0..10).collect();

        let p = Pagination { from: 2, size: 3 };
        let (page, total) = p.apply(&items);
        assert_eq!(total, 10);
        assert_eq!(page, vec![2, 3, 4]);

        let p = Pagination::all();
        let (page, total) = p.apply(&items);
        assert_eq!(total, 10);
        assert_eq!(page.len(), 10);

        let p = Pagination { from: 8, size: 5 };
        let (page, total) = p.apply(&items);
        assert_eq!(total, 10);
        assert_eq!(page, vec![8, 9]);
    }

    #[test]
    fn journal_file_new() {
        let j = JournalFile::new("test-journal", "Test Title".into(), None);
        assert_eq!(j.name, "test-journal");
        assert_eq!(j.title, "Test Title");
        assert!(j.items.is_empty());
        assert_eq!(j.schema, CURRENT_SCHEMA);
    }

    #[test]
    fn journal_summary_avg_std_item_size() {
        // 0 items — both None
        let j = JournalFile::new("empty", "Empty".into(), None);
        let s = JournalSummary::from(&j);
        assert_eq!(s.avg_item_size, None);
        assert_eq!(s.std_item_size, None);

        // 1 item — avg is Some, std is Some(0) (population std dev of one element is 0)
        let mut j = JournalFile::new("one", "One".into(), None);
        j.items.push(JournalItem {
            id: item_id(),
            item_type: ItemType::Note,
            content: "hello".into(),
            tags: None,
            added_at: Utc::now(),
            meta: None,
        });
        let s = JournalSummary::from(&j);
        assert!(s.avg_item_size.is_some());
        assert_eq!(s.std_item_size, Some(0));

        // 2+ items — both Some; avg matches manual calculation
        let mut j = JournalFile::new("two", "Two".into(), None);
        for content in &["short", "a longer piece of content"] {
            j.items.push(JournalItem {
                id: item_id(),
                item_type: ItemType::Note,
                content: content.to_string(),
                tags: None,
                added_at: Utc::now(),
                meta: None,
            });
        }
        let sizes: Vec<usize> = j
            .items
            .iter()
            .map(|item| serde_json::to_vec(item).unwrap().len())
            .collect();
        let sum = sizes.iter().sum::<usize>();
        let n = sizes.len();
        let expected_avg = (sum as f64 / n as f64).ceil() as usize;
        let s = JournalSummary::from(&j);
        assert_eq!(s.avg_item_size, Some(expected_avg));
        assert!(s.std_item_size.is_some());
    }

    #[test]
    fn item_id_format() {
        let id = item_id();
        assert_eq!(id.len(), 19);
        assert_eq!(&id[4..5], "-");
        assert_eq!(&id[9..10], "-");
        assert_eq!(&id[14..15], "-");
        assert!(
            id.chars()
                .all(|c| c == '-' || CONSONANTS.contains(&(c as u8)))
        );
    }
}
