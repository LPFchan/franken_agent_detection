//! Small synchronous helpers shared by the SQLite-backed connectors.

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
