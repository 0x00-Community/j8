use std::sync::Mutex;

use j8::frame::{Scope, Error, ErrorKind};
use rusqlite::types::{Value as Sql, ValueRef};
use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::{Batch, Connection};
use serde_json::{Value as Json, json};

#[derive(j8::Trace)]
pub struct SqlStorage {
    #[collect(require_static)]
    db: Mutex<Connection>,
}

struct Param(Sql);

impl From<Json> for Param {
    fn from(v: Json) -> Self {
        Self(match v {
            Json::Null => Sql::Null,
            Json::Bool(b) => Sql::Integer(i64::from(b)),
            Json::Number(n) => n.as_i64().map_or_else(|| Sql::Real(n.as_f64().unwrap_or(f64::NAN)), Sql::Integer),
            Json::String(s) => Sql::Text(s),
            other => Sql::Text(other.to_string()),
        })
    }
}

struct Column(Json);

impl From<ValueRef<'_>> for Column {
    fn from(v: ValueRef<'_>) -> Self {
        Self(match v {
            ValueRef::Null => Json::Null,
            ValueRef::Integer(i) => json!(i),
            ValueRef::Real(f) => json!(f),
            ValueRef::Text(t) => Json::String(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => json!(b),
        })
    }
}

#[j8::class]
impl SqlStorage {
    #[js(constructor)]
    fn open(a: &mut Scope<'_, '_>, path: &str) -> Result<Self, Error> {
        let db = Self::configure(path).map_err(|e| a.throw(ErrorKind::Error, &e))?;
        Ok(Self { db: Mutex::new(db) })
    }

    async fn prepare(path: String) -> Option<String> {
        let (tx, rx) = j8::lockfree::Oneshot::new();
        std::thread::spawn(move || tx.send(Self::configure(&path).err()));
        rx.await
    }

    fn exec(&self, a: &mut Scope<'_, '_>, query: &str, params: &str) -> Result<String, Error> {
        let params: Vec<Json> = serde_json::from_str(params).map_err(|e| a.throw(ErrorKind::Type, &format!("SQL bindings: {e}")))?;
        let db = self.db.lock().map_err(|_| a.throw(ErrorKind::Error, "SQLite: the database is poisoned"))?;
        Self::run(&db, query, params).map_err(|e| a.throw(ErrorKind::Error, &format!("SQLite: {e}")))
    }

    #[js(getter)]
    #[expect(clippy::cast_precision_loss, reason = "a JS number; sizes stay far below 2^53")]
    fn database_size(&self) -> f64 {
        let Ok(db) = self.db.lock() else { return 0.0 };
        db.query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get::<_, i64>(0)).map_or(0.0, |n| n as f64)
    }
}

impl SqlStorage {
    fn configure(path: &str) -> Result<Connection, String> {
        let db = if path.is_empty() { Connection::open_in_memory() } else { Connection::open(path) };
        let db = db.map_err(|e| format!("SQLite: open {path}: {e}"))?;
        db.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").map_err(|e| format!("SQLite: {e}"))?;
        Ok(db)
    }

    fn run(db: &Connection, query: &str, params: Vec<Json>) -> rusqlite::Result<String> {
        let mut statements = Vec::new();
        let mut batch = Batch::new(db, query);
        while let Some(stmt) = batch.next()? {
            statements.push(stmt);
        }
        let Some(mut last) = statements.pop() else {
            return Ok(json!({ "columns": [], "rows": [], "rowsRead": 0, "rowsWritten": 0 }).to_string());
        };
        for mut stmt in statements {
            stmt.raw_execute()?;
        }
        let params: Vec<Sql> = params.into_iter().map(|p| Param::from(p).0).collect();
        let columns: Vec<String> = last.column_names().into_iter().map(str::to_owned).collect();
        let readonly = last.readonly();
        let mut rows = Vec::new();
        let mut cursor = last.query(rusqlite::params_from_iter(params))?;
        while let Some(row) = cursor.next()? {
            let values: rusqlite::Result<Vec<Json>> = (0..columns.len()).map(|i| row.get_ref(i).map(|v| Column::from(v).0)).collect();
            rows.push(Json::Array(values?));
        }
        drop(cursor);
        let written = if readonly { 0 } else { db.changes() };
        Ok(json!({ "columns": columns, "rowsRead": rows.len(), "rowsWritten": written, "rows": rows }).to_string())
    }
}
