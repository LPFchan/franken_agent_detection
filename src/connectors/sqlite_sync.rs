//! Small synchronous helpers shared by the SQLite-backed connectors.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use rusqlite::{Connection as RusqliteConnection, OpenFlags, Params, Row};

/// A SQLite connection with the connector-facing operations kept in one place.
pub struct Connection {
    inner: RusqliteConnection,
}

impl Connection {
    /// Open (or create) a database at `path`.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        Ok(Self {
            inner: RusqliteConnection::open(path)?,
        })
    }

    /// Execute a single SQL statement, returning the affected row count.
    pub fn execute(&self, sql: &str) -> rusqlite::Result<usize> {
        match self.inner.execute(sql, []) {
            Ok(changed) => Ok(changed),
            Err(rusqlite::Error::ExecuteReturnedResults) => {
                self.inner.execute_batch(sql).map(|()| 0)
            }
            Err(error) => Err(error),
        }
    }

    /// Execute a string of semicolon-separated SQL statements.
    pub fn execute_batch(&self, sql: &str) -> rusqlite::Result<()> {
        self.inner.execute_batch(sql)
    }

    pub fn read_transaction<T, E>(&self, f: impl FnOnce(&Self) -> Result<T, E>) -> Result<T, E>
    where
        E: From<rusqlite::Error>,
    {
        self.execute("BEGIN DEFERRED;").map_err(E::from)?;
        // The callback is never resumed after an unwind. Keep its existing
        // unconstrained signature while releasing this transaction before the
        // caller can catch the panic and reuse the connection.
        let result = match catch_unwind(AssertUnwindSafe(|| f(self))) {
            Ok(result) => result,
            Err(payload) => {
                // A cleanup panic must not replace the callback's payload.
                let _ = catch_unwind(AssertUnwindSafe(|| self.execute("ROLLBACK;")));
                resume_unwind(payload);
            }
        };
        match result {
            Ok(value) => {
                if let Err(err) = self.execute("COMMIT;") {
                    let _ = self.execute("ROLLBACK;");
                    return Err(E::from(err));
                }
                Ok(value)
            }
            Err(err) => {
                let _ = self.execute("ROLLBACK;");
                Err(err)
            }
        }
    }
}

/// Open a database with the requested SQLite access flags.
pub fn open_with_flags(path: &str, flags: OpenFlags) -> rusqlite::Result<Connection> {
    Ok(Connection {
        inner: RusqliteConnection::open_with_flags(path, flags)?,
    })
}

/// Connector query helpers with synchronous, collected results.
pub trait ConnectionExt {
    /// Execute a query that returns exactly one row, mapping it with `f`.
    fn query_row_map<T, P, F>(&self, sql: &str, params: P, f: F) -> rusqlite::Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>;

    /// Execute a query and collect all rows into a `Vec<T>` via a mapping closure.
    fn query_map_collect<T, P, F>(&self, sql: &str, params: P, f: F) -> rusqlite::Result<Vec<T>>
    where
        P: Params,
        F: FnMut(&Row<'_>) -> rusqlite::Result<T>;

    /// Execute a SQL statement with bound parameters.
    fn execute_compat<P: Params>(&self, sql: &str, params: P) -> rusqlite::Result<usize>;
}

impl ConnectionExt for Connection {
    fn query_row_map<T, P, F>(&self, sql: &str, params: P, f: F) -> rusqlite::Result<T>
    where
        P: Params,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    {
        self.inner.query_row(sql, params, f)
    }

    fn query_map_collect<T, P, F>(&self, sql: &str, params: P, f: F) -> rusqlite::Result<Vec<T>>
    where
        P: Params,
        F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
    {
        let mut statement = self.inner.prepare(sql)?;
        statement.query_map(params, f)?.collect()
    }

    fn execute_compat<P: Params>(&self, sql: &str, params: P) -> rusqlite::Result<usize> {
        self.inner.execute(sql, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_and_panicking_transactions_release_connection() {
        let conn = Connection::open(":memory:").unwrap();
        conn.execute("CREATE TABLE sample (value INTEGER)").unwrap();
        let failed: rusqlite::Result<()> = conn.read_transaction(|db| {
            db.execute("INSERT INTO sample VALUES (1)")?;
            Err(rusqlite::Error::InvalidQuery)
        });
        assert!(failed.is_err());
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            let _: rusqlite::Result<()> = conn.read_transaction(|db| {
                db.execute("INSERT INTO sample VALUES (2)")?;
                panic!("callback failed");
            });
        }));
        assert!(panicked.is_err());
        let count: i64 = conn
            .read_transaction(|db| {
                db.query_row_map("SELECT count(*) FROM sample", [], |row| row.get(0))
            })
            .unwrap();
        assert_eq!(count, 0);
    }
}
