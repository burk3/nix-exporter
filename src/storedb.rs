//! Read-only queries against Nix's SQLite store database.

use anyhow::Result;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;
use std::time::Duration;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(1))?;
    Ok(conn)
}

#[derive(Debug, PartialEq)]
pub struct Stats {
    pub paths: f64,
    pub nar_bytes: f64,
    pub built: f64,
    pub signed: f64,
    pub last_registration: f64,
}

pub fn stats(conn: &Connection) -> Result<Stats> {
    Ok(conn.query_row(
        "select count(*), coalesce(sum(narSize), 0), coalesce(sum(ultimate = 1), 0), \
         coalesce(sum(coalesce(sigs, '') != ''), 0), coalesce(max(registrationTime), 0) \
         from ValidPaths",
        [],
        |r| {
            Ok(Stats {
                paths: r.get::<_, i64>(0)? as f64,
                nar_bytes: r.get::<_, i64>(1)? as f64,
                built: r.get::<_, i64>(2)? as f64,
                signed: r.get::<_, i64>(3)? as f64,
                last_registration: r.get::<_, i64>(4)? as f64,
            })
        },
    )?)
}

/// Total NAR size of `path`'s closure, or `None` when `path` is not valid.
pub fn closure_bytes(conn: &Connection, path: &str) -> Result<Option<f64>> {
    let id: Option<i64> = conn
        .query_row("select id from ValidPaths where path = ?1", [path], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let sum: i64 = conn.query_row(
        "with recursive closure(id) as (select ?1 \
         union select reference from Refs join closure on Refs.referrer = closure.id) \
         select coalesce(sum(narSize), 0) from ValidPaths where id in (select id from closure)",
        [id],
        |r| r.get(0),
    )?;
    Ok(Some(sum as f64))
}

#[cfg(test)]
pub mod fixture {
    //! Nix's schema, and helpers to fill it.
    use rusqlite::{params, Connection};

    pub const SCHEMA: &str = "
        create table ValidPaths (id integer primary key autoincrement not null, path text unique not null,
            hash text not null, registrationTime integer not null, deriver text, narSize integer,
            ultimate integer, sigs text, ca text);
        create table Refs (referrer integer not null, reference integer not null,
            primary key (referrer, reference));
        create table DerivationOutputs (drv integer not null, id text not null, path text not null,
            primary key (drv, id));";

    /// A writable DB in WAL mode, like Nix's.
    pub fn create(path: &std::path::Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn
    }

    /// Registration time is 100 + the number of paths already present.
    pub fn add_path(
        conn: &Connection,
        path: &str,
        nar: i64,
        ultimate: bool,
        signed: bool,
        deriver: Option<&str>,
    ) -> i64 {
        conn.execute(
            "insert into ValidPaths (path, hash, registrationTime, deriver, narSize, ultimate, sigs) \
             values (?1, 'sha256:0', (select count(*) from ValidPaths) + 100, ?2, ?3, ?4, ?5)",
            params![path, deriver, nar, ultimate.then_some(1), signed.then_some("cache:sig")],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    pub fn add_ref(conn: &Connection, from: i64, to: i64) {
        conn.execute("insert into Refs values (?1, ?2)", params![from, to])
            .unwrap();
    }

    pub fn add_output(conn: &Connection, drv: i64, name: &str, path: &str) {
        conn.execute(
            "insert into DerivationOutputs values (?1, ?2, ?3)",
            params![drv, name, path],
        )
        .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn stats_and_closure() {
        let d = tempfile::tempdir().unwrap();
        let db = d.path().join("db/db.sqlite");
        let w = create(&db);
        let a = add_path(&w, "/nix/store/a-app", 100, true, false, None);
        let b = add_path(&w, "/nix/store/b-lib", 50, false, true, None);
        let c = add_path(&w, "/nix/store/c-libc", 25, false, true, None);
        add_path(&w, "/nix/store/d-other", 7, false, false, None);
        add_ref(&w, a, b);
        add_ref(&w, a, a);
        add_ref(&w, b, c);
        // the writer stays open, as the daemon's does
        let r = open(&db).unwrap();
        assert_eq!(
            stats(&r).unwrap(),
            Stats {
                paths: 4.0,
                nar_bytes: 182.0,
                built: 1.0,
                signed: 2.0,
                last_registration: 103.0
            }
        );
        assert_eq!(closure_bytes(&r, "/nix/store/a-app").unwrap(), Some(175.0));
        assert_eq!(closure_bytes(&r, "/nix/store/c-libc").unwrap(), Some(25.0));
        assert_eq!(closure_bytes(&r, "/nix/store/missing").unwrap(), None);
    }
}
