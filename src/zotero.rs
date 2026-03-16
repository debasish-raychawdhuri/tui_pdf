use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

#[derive(Debug, Clone)]
pub struct ZoteroEntry {
    pub item_id: i64,
    pub title: String,
    pub authors: String,
    pub year: String,
    pub pdf_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ZoteroCollection {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}

pub struct ZoteroLibrary {
    pub entries: Vec<ZoteroEntry>,
    pub collections: Vec<ZoteroCollection>,
    /// item_id -> list of collection IDs it belongs to
    pub item_collections: HashMap<i64, Vec<i64>>,
}

impl ZoteroLibrary {
    /// Get entries belonging to a collection (direct members only).
    pub fn entries_in_collection(&self, collection_id: i64) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.item_collections
                    .get(&e.item_id)
                    .map_or(false, |cols| cols.contains(&collection_id))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Get child collections of a parent (None = top-level).
    pub fn child_collections(&self, parent_id: Option<i64>) -> Vec<&ZoteroCollection> {
        self.collections
            .iter()
            .filter(|c| c.parent_id == parent_id)
            .collect()
    }

    /// Get entries not in any collection.
    pub fn unfiled_entries(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.item_collections
                    .get(&e.item_id)
                    .map_or(true, |cols| cols.is_empty())
            })
            .map(|(i, _)| i)
            .collect()
    }
}

pub fn load_library(zotero_dir: &Path) -> Result<ZoteroLibrary, Box<dyn std::error::Error>> {
    let db_path = zotero_dir.join("zotero.sqlite");
    let db_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &db_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;

    // Load collections
    let mut coll_stmt = conn.prepare(
        "SELECT collectionID, collectionName, parentCollectionID FROM collections"
    )?;
    let collections: Vec<ZoteroCollection> = coll_stmt
        .query_map([], |row| {
            Ok(ZoteroCollection {
                id: row.get(0)?,
                name: row.get(1)?,
                parent_id: row.get(2)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    // Load collection-item mappings
    let mut ci_stmt = conn.prepare(
        "SELECT collectionID, itemID FROM collectionItems"
    )?;
    let mut item_collections: HashMap<i64, Vec<i64>> = HashMap::new();
    let ci_rows = ci_stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in ci_rows {
        let (coll_id, item_id) = row?;
        item_collections.entry(item_id).or_default().push(coll_id);
    }

    // Find all stored PDF attachments with their parent item
    let mut att_stmt = conn.prepare(
        "SELECT ia.parentItemID, ia.path, items.key
         FROM itemAttachments ia
         JOIN items ON items.itemID = ia.itemID
         WHERE ia.linkMode = 1
           AND ia.contentType = 'application/pdf'
           AND ia.parentItemID IS NOT NULL"
    )?;

    struct AttInfo {
        path: String,
        key: String,
    }

    let mut parent_attachments: HashMap<i64, Vec<AttInfo>> = HashMap::new();
    let rows = att_stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    for row in rows {
        let (parent_id, path, key) = row?;
        parent_attachments.entry(parent_id).or_default().push(AttInfo { path, key });
    }

    if parent_attachments.is_empty() {
        return Ok(ZoteroLibrary {
            entries: Vec::new(),
            collections,
            item_collections,
        });
    }

    let mut title_stmt = conn.prepare(
        "SELECT idv.value
         FROM itemData id
         JOIN itemDataValues idv ON idv.valueID = id.valueID
         JOIN fields f ON f.fieldID = id.fieldID
         WHERE id.itemID = ? AND f.fieldName = 'title'"
    )?;

    let mut author_stmt = conn.prepare(
        "SELECT c.firstName, c.lastName
         FROM itemCreators ic
         JOIN creators c ON c.creatorID = ic.creatorID
         WHERE ic.itemID = ?
         ORDER BY ic.orderIndex"
    )?;

    let mut date_stmt = conn.prepare(
        "SELECT idv.value
         FROM itemData id
         JOIN itemDataValues idv ON idv.valueID = id.valueID
         JOIN fields f ON f.fieldID = id.fieldID
         WHERE id.itemID = ? AND f.fieldName = 'date'"
    )?;

    let storage_dir = zotero_dir.join("storage");
    let mut entries = Vec::new();

    for (parent_id, attachments) in &parent_attachments {
        let mut pdf_path = None;
        for att in attachments {
            let resolved = if let Some(filename) = att.path.strip_prefix("storage:") {
                storage_dir.join(&att.key).join(filename)
            } else {
                PathBuf::from(&att.path)
            };
            if resolved.exists() {
                pdf_path = Some(resolved);
                break;
            }
        }
        let pdf_path = match pdf_path {
            Some(p) => p,
            None => continue,
        };

        let title: String = title_stmt
            .query_row([parent_id], |row| row.get(0))
            .unwrap_or_default();

        let authors: Vec<String> = author_stmt
            .query_map([parent_id], |row| {
                let first: String = row.get(0)?;
                let last: String = row.get(1)?;
                if first.is_empty() {
                    Ok(last)
                } else {
                    Ok(format!("{first} {last}"))
                }
            })?
            .filter_map(|r| r.ok())
            .collect();
        let authors = authors.join(", ");

        let year: String = date_stmt
            .query_row([parent_id], |row| row.get::<_, String>(0))
            .unwrap_or_default();
        let year = year.chars().take(4).collect::<String>();

        if title.is_empty() {
            continue;
        }

        entries.push(ZoteroEntry {
            item_id: *parent_id,
            title,
            authors,
            year,
            pdf_path,
        });
    }

    entries.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(ZoteroLibrary {
        entries,
        collections,
        item_collections,
    })
}

/// Look up Zotero metadata for a PDF by its file path.
pub fn lookup_by_path(zotero_dir: &Path, pdf_path: &Path) -> Option<ZoteroEntry> {
    let canonical = pdf_path.canonicalize().ok()?;
    let db_path = zotero_dir.join("zotero.sqlite");
    let db_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &db_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ).ok()?;

    let storage_dir = zotero_dir.join("storage");

    let mut att_stmt = conn.prepare(
        "SELECT ia.parentItemID, ia.path, items.key
         FROM itemAttachments ia
         JOIN items ON items.itemID = ia.itemID
         WHERE ia.linkMode = 1
           AND ia.contentType = 'application/pdf'
           AND ia.parentItemID IS NOT NULL"
    ).ok()?;

    let rows = att_stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }).ok()?;

    let mut parent_id = None;
    for row in rows {
        let (pid, path, key) = row.ok()?;
        let resolved = if let Some(filename) = path.strip_prefix("storage:") {
            storage_dir.join(&key).join(filename)
        } else {
            PathBuf::from(&path)
        };
        if let Ok(c) = resolved.canonicalize() {
            if c == canonical {
                parent_id = Some(pid);
                break;
            }
        }
    }

    let parent_id = parent_id?;

    let title: String = conn.query_row(
        "SELECT idv.value FROM itemData id
         JOIN itemDataValues idv ON idv.valueID = id.valueID
         JOIN fields f ON f.fieldID = id.fieldID
         WHERE id.itemID = ? AND f.fieldName = 'title'",
        [parent_id], |row| row.get(0),
    ).unwrap_or_default();

    let mut author_stmt = conn.prepare(
        "SELECT c.firstName, c.lastName
         FROM itemCreators ic
         JOIN creators c ON c.creatorID = ic.creatorID
         WHERE ic.itemID = ?
         ORDER BY ic.orderIndex"
    ).ok()?;
    let authors: Vec<String> = author_stmt
        .query_map([parent_id], |row| {
            let first: String = row.get(0)?;
            let last: String = row.get(1)?;
            if first.is_empty() { Ok(last) } else { Ok(format!("{first} {last}")) }
        }).ok()?
        .filter_map(|r| r.ok())
        .collect();
    let authors = authors.join(", ");

    let year: String = conn.query_row(
        "SELECT idv.value FROM itemData id
         JOIN itemDataValues idv ON idv.valueID = id.valueID
         JOIN fields f ON f.fieldID = id.fieldID
         WHERE id.itemID = ? AND f.fieldName = 'date'",
        [parent_id], |row| row.get::<_, String>(0),
    ).unwrap_or_default();
    let year = year.chars().take(4).collect::<String>();

    Some(ZoteroEntry {
        item_id: parent_id,
        title,
        authors,
        year,
        pdf_path: pdf_path.to_path_buf(),
    })
}

/// Return the PDF path of the most recently added Zotero item.
pub fn latest_pdf(zotero_dir: &Path) -> Option<PathBuf> {
    let db_path = zotero_dir.join("zotero.sqlite");
    let db_uri = format!("file:{}?immutable=1", db_path.display());
    let conn = Connection::open_with_flags(
        &db_uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    ).ok()?;

    let storage_dir = zotero_dir.join("storage");

    // Find the most recently added parent item that has a stored PDF attachment
    let mut stmt = conn.prepare(
        "SELECT ia.path, att.key
         FROM itemAttachments ia
         JOIN items att ON att.itemID = ia.itemID
         JOIN items parent ON parent.itemID = ia.parentItemID
         WHERE ia.linkMode = 1
           AND ia.contentType = 'application/pdf'
           AND ia.parentItemID IS NOT NULL
         ORDER BY parent.dateAdded DESC"
    ).ok()?;

    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).ok()?;

    for row in rows {
        let (path, key) = row.ok()?;
        let resolved = if let Some(filename) = path.strip_prefix("storage:") {
            storage_dir.join(&key).join(filename)
        } else {
            PathBuf::from(&path)
        };
        if resolved.exists() {
            return Some(resolved);
        }
    }
    None
}
