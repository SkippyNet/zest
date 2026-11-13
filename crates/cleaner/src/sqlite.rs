//! Careful, minimal edits of browser SQLite databases.
//!
//! Every write happens inside a single transaction and only touches tables
//! that exist, so an unexpected schema leaves the database untouched rather
//! than half-cleaned.

use crate::cookies::KeepList;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::time::Duration;
use zest_core::{Error, Result};

fn db_err(context: &str, path: &Path, e: rusqlite::Error) -> Error {
    Error::Other(format!("{context} {}: {e}", path.display()))
}

fn ident(name: &str) -> Result<&str> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name)
    } else {
        Err(Error::Invalid(format!("unsafe SQL identifier: {name}")))
    }
}

fn open_read(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err("opening", path, e))?;
    let _ = conn.busy_timeout(Duration::from_millis(60));
    Ok(conn)
}

fn open_write(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| db_err("opening", path, e))?;
    let _ = conn.busy_timeout(Duration::from_secs(2));
    Ok(conn)
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(table) = ident(table) else {
        return false;
    };
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    rows.flatten().any(|name| name.eq_ignore_ascii_case(column))
}

/// Number of rows in `table`, or zero when the table does not exist.
pub fn count_rows(db: &Path, table: &str) -> Result<u64> {
    let table = ident(table)?;
    let conn = open_read(db)?;
    if !table_exists(&conn, table) {
        return Ok(0);
    }
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|n| n.max(0) as u64)
    .map_err(|e| db_err("reading", db, e))
}

/// Cookie counts per host.
pub fn cookie_hosts(db: &Path, table: &str, column: &str) -> Result<Vec<(String, u64)>> {
    let (table, column) = (ident(table)?, ident(column)?);
    let conn = open_read(db)?;
    if !table_exists(&conn, table) {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {column}, COUNT(*) FROM {table} GROUP BY {column}"
        ))
        .map_err(|e| db_err("reading", db, e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| db_err("reading", db, e))?;
    let mut out = Vec::new();
    for row in rows {
        let (host, count) = row.map_err(|e| db_err("reading", db, e))?;
        out.push((host, count.max(0) as u64));
    }
    Ok(out)
}

/// Counts cookies that are *not* protected by the keep list: `(removable, kept)`.
pub fn count_cookies(db: &Path, table: &str, column: &str, keep: &KeepList) -> Result<(u64, u64)> {
    let mut removable = 0;
    let mut kept = 0;
    for (host, count) in cookie_hosts(db, table, column)? {
        if keep.keeps(&host) {
            kept += count;
        } else {
            removable += count;
        }
    }
    Ok((removable, kept))
}

/// Deletes all cookies whose host is not on the keep list. Returns how many
/// cookies were removed.
pub fn delete_cookies(db: &Path, table: &str, column: &str, keep: &KeepList) -> Result<u64> {
    let (table, column) = (ident(table)?, ident(column)?);
    let hosts = cookie_hosts(db, table, column)?;
    let mut conn = open_write(db)?;
    let tx = conn.transaction().map_err(|e| db_err("updating", db, e))?;
    let mut removed = 0u64;
    {
        let mut stmt = tx
            .prepare(&format!("DELETE FROM {table} WHERE {column} = ?1"))
            .map_err(|e| db_err("updating", db, e))?;
        for (host, _) in hosts.iter().filter(|(host, _)| !keep.keeps(host)) {
            removed += stmt
                .execute([host])
                .map_err(|e| db_err("updating", db, e))? as u64;
        }
    }
    tx.commit().map_err(|e| db_err("updating", db, e))?;
    Ok(removed)
}

/// Deletes every row of the given tables (those that exist). Returns the
/// total number of rows removed.
pub fn clear_tables(db: &Path, tables: &[&str]) -> Result<u64> {
    let mut conn = open_write(db)?;
    let tx = conn.transaction().map_err(|e| db_err("updating", db, e))?;
    let mut removed = 0u64;
    for table in tables {
        let table = ident(table)?;
        if table_exists(&tx, table) {
            removed += tx
                .execute(&format!("DELETE FROM {table}"), [])
                .map_err(|e| db_err("updating", db, e))? as u64;
        }
    }
    tx.commit().map_err(|e| db_err("updating", db, e))?;
    Ok(removed)
}

/// Number of history visits recorded in a Firefox `places.sqlite`.
pub fn firefox_history_count(db: &Path) -> Result<u64> {
    count_rows(db, "moz_historyvisits")
}

/// Removes browsing history from a Firefox `places.sqlite` while keeping
/// bookmarks (and the pages they point to) intact. Returns the number of
/// visits removed.
pub fn firefox_clear_history(db: &Path) -> Result<u64> {
    let mut conn = open_write(db)?;
    if !table_exists(&conn, "moz_places") || !table_exists(&conn, "moz_historyvisits") {
        return Err(Error::Invalid(format!(
            "{} is not a places database",
            db.display()
        )));
    }
    let has_bookmarks = table_exists(&conn, "moz_bookmarks");
    let has_foreign_count = column_exists(&conn, "moz_places", "foreign_count");
    let tx = conn.transaction().map_err(|e| db_err("updating", db, e))?;
    let run = |sql: &str| -> Result<u64> {
        tx.execute(sql, [])
            .map(|n| n as u64)
            .map_err(|e| db_err("updating", db, e))
    };

    let visits = run("DELETE FROM moz_historyvisits")?;
    if table_exists(&tx, "moz_inputhistory") {
        run("DELETE FROM moz_inputhistory")?;
    }

    // Remove pages nothing refers to any more. A page is kept when a
    // bookmark, keyword or tag points at it.
    let mut keep_clauses: Vec<&str> = Vec::new();
    if has_bookmarks {
        keep_clauses.push("id NOT IN (SELECT fk FROM moz_bookmarks WHERE fk IS NOT NULL)");
    }
    if has_foreign_count {
        keep_clauses.push("foreign_count = 0");
    }
    if table_exists(&tx, "moz_keywords") {
        keep_clauses
            .push("id NOT IN (SELECT place_id FROM moz_keywords WHERE place_id IS NOT NULL)");
    }
    if keep_clauses.is_empty() {
        // Without any way to recognise bookmarked pages, leave pages alone.
        log::warn!(
            "{}: unknown places schema, keeping page records",
            db.display()
        );
    } else {
        run(&format!(
            "DELETE FROM moz_places WHERE {}",
            keep_clauses.join(" AND ")
        ))?;
    }

    // Reset visit statistics of the pages that remain (bookmarked ones).
    let mut resets = vec!["visit_count = 0", "last_visit_date = NULL"];
    if column_exists(&tx, "moz_places", "recalc_frecency") {
        resets.push("recalc_frecency = 1");
    }
    if column_exists(&tx, "moz_places", "typed") {
        resets.push("typed = 0");
    }
    run(&format!("UPDATE moz_places SET {}", resets.join(", ")))?;

    // Dependent records of pages that are gone.
    for (table, column) in [
        ("moz_annos", "place_id"),
        ("moz_places_metadata", "place_id"),
        ("moz_previews_tombstones", ""),
        ("moz_places_metadata_search_queries", ""),
    ] {
        if !table_exists(&tx, table) {
            continue;
        }
        if column.is_empty() {
            run(&format!("DELETE FROM {table}"))?;
        } else if column_exists(&tx, table, column) {
            run(&format!(
                "DELETE FROM {table} WHERE {column} NOT IN (SELECT id FROM moz_places)"
            ))?;
        }
    }
    if table_exists(&tx, "moz_origins") && column_exists(&tx, "moz_places", "origin_id") {
        run(
            "DELETE FROM moz_origins WHERE id NOT IN (SELECT origin_id FROM moz_places WHERE origin_id IS NOT NULL)",
        )?;
    }
    if table_exists(&tx, "moz_meta") {
        // Cached frecency statistics; the browser recomputes them.
        run("DELETE FROM moz_meta WHERE key LIKE 'origin_frecency_%'")?;
    }

    tx.commit().map_err(|e| db_err("updating", db, e))?;
    Ok(visits)
}

/// Bytes a `VACUUM` would return to the file system (free pages in the file).
pub fn reclaimable_bytes(db: &Path) -> Result<u64> {
    let conn = open_read(db)?;
    let pragma = |name: &str| -> Result<u64> {
        conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0))
            .map(|n| n.max(0) as u64)
            .map_err(|e| db_err("reading", db, e))
    };
    Ok(pragma("freelist_count")? * pragma("page_size")?)
}

/// Compacts a database in place. Returns the number of bytes saved.
pub fn vacuum(db: &Path) -> Result<u64> {
    let before = std::fs::metadata(db).map(|m| m.len()).unwrap_or(0);
    let conn = open_write(db)?;
    conn.execute_batch("VACUUM")
        .map_err(|e| db_err("compacting", db, e))?;
    drop(conn);
    let after = std::fs::metadata(db).map(|m| m.len()).unwrap_or(before);
    Ok(before.saturating_sub(after))
}

#[cfg(test)]
pub(crate) mod testdb {
    //! Builders for miniature browser databases used across the test suite.
    use rusqlite::Connection;
    use std::path::Path;

    /// A Chromium-style cookie database with one cookie per `(host, name)`.
    pub fn chromium_cookies(path: &Path, cookies: &[(&str, &str)]) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL, \
             name TEXT NOT NULL, value TEXT NOT NULL, path TEXT NOT NULL);",
        )
        .unwrap();
        for (i, (host, name)) in cookies.iter().enumerate() {
            conn.execute(
                "INSERT INTO cookies VALUES (?1, ?2, ?3, 'v', '/')",
                rusqlite::params![i as i64, host, name],
            )
            .unwrap();
        }
    }

    /// A Firefox-style cookie database.
    pub fn firefox_cookies(path: &Path, cookies: &[(&str, &str)]) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, name TEXT, value TEXT, host TEXT, path TEXT);",
        )
        .unwrap();
        for (host, name) in cookies {
            conn.execute(
                "INSERT INTO moz_cookies (name, value, host, path) VALUES (?1, 'v', ?2, '/')",
                rusqlite::params![name, host],
            )
            .unwrap();
        }
    }

    /// A Chromium-style `History` database with some URLs and downloads.
    pub fn chromium_history(path: &Path, downloads: usize) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE urls (id INTEGER PRIMARY KEY, url TEXT);\
             CREATE TABLE downloads (id INTEGER PRIMARY KEY, target_path TEXT);\
             CREATE TABLE downloads_url_chains (id INTEGER, chain_index INTEGER, url TEXT);\
             INSERT INTO urls (url) VALUES ('https://example.com/');",
        )
        .unwrap();
        for i in 0..downloads {
            conn.execute(
                "INSERT INTO downloads (target_path) VALUES (?1)",
                [format!("C:\\Downloads\\file{i}.zip")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO downloads_url_chains VALUES (?1, 0, 'https://example.com/f')",
                [i as i64],
            )
            .unwrap();
        }
    }

    /// A Firefox-style `places.sqlite` with one bookmarked page and several
    /// visited-only pages.
    pub fn firefox_places(path: &Path, visited_only: usize) {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_origins (id INTEGER PRIMARY KEY, prefix TEXT, host TEXT, frecency INTEGER);\
             CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT, visit_count INTEGER DEFAULT 0, \
                typed INTEGER DEFAULT 0, last_visit_date INTEGER, foreign_count INTEGER DEFAULT 0 NOT NULL, \
                recalc_frecency INTEGER DEFAULT 0 NOT NULL, origin_id INTEGER);\
             CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, place_id INTEGER, visit_date INTEGER, visit_type INTEGER);\
             CREATE TABLE moz_inputhistory (place_id INTEGER, input TEXT, use_count INTEGER);\
             CREATE TABLE moz_bookmarks (id INTEGER PRIMARY KEY, type INTEGER, fk INTEGER, title TEXT);\
             CREATE TABLE moz_keywords (id INTEGER PRIMARY KEY, keyword TEXT, place_id INTEGER);\
             CREATE TABLE moz_annos (id INTEGER PRIMARY KEY, place_id INTEGER, content TEXT);\
             CREATE TABLE moz_places_metadata (id INTEGER PRIMARY KEY, place_id INTEGER);\
             CREATE TABLE moz_meta (key TEXT PRIMARY KEY, value);\
             INSERT INTO moz_origins VALUES (1, 'https://', 'bookmarked.example', 100);\
             INSERT INTO moz_places (id, url, title, visit_count, typed, last_visit_date, foreign_count, origin_id) \
                VALUES (1, 'https://bookmarked.example/', 'Bookmarked', 5, 1, 1700000000, 1, 1);\
             INSERT INTO moz_bookmarks VALUES (1, 2, NULL, 'folder');\
             INSERT INTO moz_bookmarks VALUES (2, 1, 1, 'Bookmarked');\
             INSERT INTO moz_historyvisits (place_id, visit_date, visit_type) VALUES (1, 1700000000, 1);\
             INSERT INTO moz_meta VALUES ('origin_frecency_count', 2);\
             INSERT INTO moz_meta VALUES ('other_setting', 7);",
        )
        .unwrap();
        for i in 0..visited_only {
            let id = 100 + i as i64;
            conn.execute(
                "INSERT INTO moz_origins VALUES (?1, 'https://', ?2, 10)",
                rusqlite::params![id, format!("visited{i}.example")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places (id, url, title, visit_count, last_visit_date, origin_id) \
                 VALUES (?1, ?2, 'Visited', 2, 1700000001, ?1)",
                rusqlite::params![id, format!("https://visited{i}.example/")],
            )
            .unwrap();
            for _ in 0..2 {
                conn.execute(
                    "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type) VALUES (?1, 1700000001, 1)",
                    [id],
                )
                .unwrap();
            }
            conn.execute("INSERT INTO moz_inputhistory VALUES (?1, 'vis', 1)", [id])
                .unwrap();
            conn.execute(
                "INSERT INTO moz_annos (place_id, content) VALUES (?1, 'x')",
                [id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places_metadata (place_id) VALUES (?1)",
                [id],
            )
            .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn keep(domains: &[&str]) -> KeepList {
        KeepList::new(&domains.iter().map(|d| (*d).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn cookie_counting_and_selective_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("Cookies");
        testdb::chromium_cookies(
            &db,
            &[
                (".example.com", "sid"),
                ("www.example.com", "pref"),
                (".tracker.net", "id"),
                ("ads.tracker.net", "uid"),
                ("shop.org", "cart"),
            ],
        );
        let keep_list = keep(&["example.com"]);
        assert_eq!(
            count_cookies(&db, "cookies", "host_key", &keep_list).unwrap(),
            (3, 2)
        );
        assert_eq!(count_rows(&db, "cookies").unwrap(), 5);
        assert_eq!(count_rows(&db, "no_such_table").unwrap(), 0);

        assert_eq!(
            delete_cookies(&db, "cookies", "host_key", &keep_list).unwrap(),
            3
        );
        let mut left: Vec<String> = cookie_hosts(&db, "cookies", "host_key")
            .unwrap()
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        left.sort();
        assert_eq!(left, vec![".example.com", "www.example.com"]);
        // Running again removes nothing.
        assert_eq!(
            delete_cookies(&db, "cookies", "host_key", &keep_list).unwrap(),
            0
        );
    }

    #[test]
    fn firefox_cookies_use_their_own_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("cookies.sqlite");
        testdb::firefox_cookies(&db, &[("example.com", "a"), (".other.io", "b")]);
        assert_eq!(
            delete_cookies(&db, "moz_cookies", "host", &keep(&["other.io"])).unwrap(),
            1
        );
        assert_eq!(count_rows(&db, "moz_cookies").unwrap(), 1);
    }

    #[test]
    fn clearing_tables_skips_missing_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("History");
        testdb::chromium_history(&db, 4);
        let removed = clear_tables(
            &db,
            &["downloads", "downloads_url_chains", "downloads_slices"],
        )
        .unwrap();
        assert_eq!(removed, 8);
        assert_eq!(count_rows(&db, "downloads").unwrap(), 0);
        // Browsing history itself is untouched.
        assert_eq!(count_rows(&db, "urls").unwrap(), 1);
    }

    #[test]
    fn firefox_history_is_cleared_but_bookmarks_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("places.sqlite");
        testdb::firefox_places(&db, 3);
        assert_eq!(firefox_history_count(&db).unwrap(), 7);

        assert_eq!(firefox_clear_history(&db).unwrap(), 7);

        let conn = Connection::open(&db).unwrap();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(count("SELECT COUNT(*) FROM moz_historyvisits"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM moz_inputhistory"), 0);
        // Only the bookmarked page remains, with its statistics reset.
        assert_eq!(count("SELECT COUNT(*) FROM moz_places"), 1);
        assert_eq!(count("SELECT visit_count FROM moz_places WHERE id = 1"), 0);
        assert_eq!(
            count("SELECT recalc_frecency FROM moz_places WHERE id = 1"),
            1
        );
        assert_eq!(count("SELECT COUNT(*) FROM moz_bookmarks"), 2);
        assert_eq!(count("SELECT COUNT(*) FROM moz_origins"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM moz_annos"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM moz_places_metadata"), 0);
        assert_eq!(count("SELECT COUNT(*) FROM moz_meta"), 1);
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    #[test]
    fn refuses_databases_that_are_not_places() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("Cookies");
        testdb::chromium_cookies(&db, &[("a.com", "x")]);
        assert!(firefox_clear_history(&db).is_err());
        assert_eq!(count_rows(&db, "cookies").unwrap(), 1);
    }

    #[test]
    fn vacuum_reclaims_free_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("big.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE t (data BLOB);").unwrap();
            let blob = vec![7u8; 64 * 1024];
            for _ in 0..40 {
                conn.execute("INSERT INTO t VALUES (?1)", [&blob]).unwrap();
            }
            conn.execute("DELETE FROM t", []).unwrap();
        }
        let estimate = reclaimable_bytes(&db).unwrap();
        assert!(estimate > 1024 * 1024, "estimate was {estimate}");
        let saved = vacuum(&db).unwrap();
        assert!(saved > 1024 * 1024, "saved {saved}");
        assert_eq!(reclaimable_bytes(&db).unwrap(), 0);
    }

    #[test]
    fn identifiers_are_validated() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("Cookies");
        testdb::chromium_cookies(&db, &[("a.com", "x")]);
        assert!(count_rows(&db, "cookies; DROP TABLE cookies").is_err());
        assert!(cookie_hosts(&db, "cookies", "host_key --").is_err());
        assert_eq!(count_rows(&db, "cookies").unwrap(), 1);
    }
}
