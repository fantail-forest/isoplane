# SQLite

## One connection, WAL, synchronous FULL, busy timeout, behind spawn_blocking

Give the store one connection per database behind a mutex, opened in WAL mode with `synchronous=FULL` and a busy timeout, and run every operation on the blocking pool. This is the configuration that survives crashes and concurrent readers.

**Not this:** A connection per request, or async wrappers that hold the connection across an await.

**Exception:** Read-only reporting paths may open a second read connection.

## Keep migrations as an ordered list, applied once, in a transaction, each with a test

Keep migrations as `(name, sql)` pairs in one list, recorded in a migrations table, applied in a single transaction on open. Give every new migration a test that applies all prior migrations, seeds adversarial pre-migration data, applies the new one, and asserts the outcome.

**Not this:** Schema changes made by editing `CREATE TABLE` statements in place.

**Exception:** None.

## Store as a row any state that must survive a restart

Persist component state that must survive a restart in database rows. Treat an in-memory table as a cache rebuilt from rows, rather than the source of truth. Keep one transactional owner for that state.

**Not this:** Treating process-local memory as restart-persistent storage.

**Exception:** None.

## Make read-modify-write operations atomic

Place a read-modify-write operation in one `BEGIN IMMEDIATE` transaction so it does not race a separate check and write.

**Not this:** Reading a value, ending the transaction, then writing a result based on that earlier value.

**Exception:** None.

## Use the real store in tests of query behaviour

Run tests of what SQL produces, including ordering and pagination, against a real SQLite database in a temp directory with an injected clock. Tests of what the application does with a query result may take the result as input.

**Not this:** An in-memory fake used as proof of SQL query correctness.

**Exception:** None.
