# Manifold-SQL Follow-ups Design

## Overview

Three follow-up work items for the manifold-sql crate after the initial implementation:

1. **Fix 8 ignored test edge cases** — bugs and missing features discovered during test suite development
2. **Performance benchmarks** — empirical runtime analysis across all SQL statement types
3. **Subquery support** — subqueries in WHERE, FROM, and SELECT clauses

## 1. Bug Fixes (8 Ignored Tests)

### 1.1 Mixed SMALLINT/INTEGER row storage corruption

**Problem:** When a table has columns of different fixed widths (e.g., SMALLINT=2 bytes, INTEGER=8 bytes), the row format decode logic miscalculates offsets because it relies on column types to compute positions, but there's a mismatch between how the encoder and decoder handle mixed widths.

**Fix:** Audit `storage/row_format.rs` — ensure `decode_column()` correctly sums the fixed widths of all preceding columns when computing the offset into the fixed-width section. The issue is likely that the decoder doesn't properly handle the varying widths within the fixed-width section.

### 1.2 SMALLINT overflow enforcement

**Problem:** Inserting a value larger than i16::MAX into a SMALLINT column silently truncates instead of erroring.

**Fix:** In the constraint checking or value conversion logic, add a range check: if the value is `Value::Integer(v)` and the target type is `SqlType::SmallInt`, verify `v >= i16::MIN as i64 && v <= i16::MAX as i64`. Error with `SqlError::Execute` if out of range.

### 1.3 PK auto-increment (INSERT without PK column)

**Problem:** `INSERT INTO t (v) VALUES ('hello')` on a table with `id INTEGER PRIMARY KEY` fails because `id` is NOT NULL but not provided.

**Fix:** When an INTEGER PRIMARY KEY column is omitted from INSERT, auto-generate the next rowid from the sequence. The executor's INSERT path should detect missing PK columns and fill them with the next auto-increment value.

### 1.4 UNIQUE allows multiple NULLs

**Problem:** UNIQUE index treats NULLs as equal, rejecting a second NULL. SQL standard says NULLs are not equal — multiple NULLs should be allowed in UNIQUE columns.

**Fix:** In the unique index enforcement logic, skip the uniqueness check when the value being inserted is NULL.

### 1.5 Column aliases in ResultSet

**Problem:** `SELECT name AS n FROM t` returns the correct values but `result.columns()` shows `"name"` instead of `"n"`.

**Fix:** The projection's aliases need to propagate through to the ResultSet column names. The `PlanSchema` from the `Project` node should carry the alias names, and the executor should pass them to `ResultSet::new()`.

### 1.6 3-way joins

**Problem:** `SELECT ... FROM a JOIN b ON ... JOIN c ON ...` returns 0 rows.

**Fix:** The join executor likely doesn't handle the column offset correctly when there are more than 2 tables. When the second join runs, column references in its condition need to account for columns from both previous tables. Debug the column offset calculation in the join builder.

### 1.7 Self-joins

**Problem:** `SELECT ... FROM t AS a JOIN t AS b ON a.id = b.id` returns 0 rows.

**Fix:** The scope/binding logic may not properly handle the same table added twice with different aliases. The binder's `Scope::add_table()` may overwrite the first entry. Fix by tracking tables by alias (not table name) when aliases are present.

### 1.8 NULLS FIRST/LAST in ORDER BY

**Problem:** `ORDER BY col NULLS FIRST` syntax may not be supported.

**Fix:** The sort operator already has `nulls_first: Option<bool>` in `OrderByExpr`. The issue may be in the binder (not passing through the sqlparser `nulls_first` field) or in the sort comparator (not handling the `nulls_first` option). Fix both if needed.

## 2. Performance Benchmarks

### Goal

Empirical runtime analysis across all SQL statement types, measuring latency and throughput at various table sizes.

### Benchmark framework

Use Criterion.rs for statistical rigor. Create `crates/manifold-sql/benches/sql_benchmarks.rs`.

### Benchmark matrix

**Statement types to benchmark:**
- Point lookup: `SELECT * FROM t WHERE id = $1` (indexed PK)
- Range scan: `SELECT * FROM t WHERE id BETWEEN $1 AND $2`
- Full table scan: `SELECT * FROM t`
- Single insert: `INSERT INTO t (...) VALUES (...)`
- Bulk insert: 1000 rows in one transaction
- Update: `UPDATE t SET col = $1 WHERE id = $2`
- Delete: `DELETE FROM t WHERE id = $1`
- Join (2 tables): `SELECT ... FROM a JOIN b ON a.id = b.a_id`
- Aggregate: `SELECT COUNT(*), SUM(val) FROM t`
- Aggregate with GROUP BY: `SELECT category, COUNT(*) FROM t GROUP BY category`

**Table sizes:**
- 100 rows
- 1,000 rows
- 10,000 rows
- 100,000 rows

**Metrics per benchmark:**
- Latency (median, p95, p99)
- Throughput (ops/sec)

### Benchmark setup

Each benchmark group creates a database, populates it to the target size, then measures the operation. Population happens outside the measured section.

### Output

Criterion generates HTML reports with statistical analysis. Results should be comparable across runs.

## 3. Subquery Support

### Scope

Support subqueries in three positions:
- `WHERE col IN (SELECT ...)` — uncorrelated subquery producing a set
- `WHERE EXISTS (SELECT ...)` — existence check
- `FROM (SELECT ...) AS alias` — derived table
- `SELECT (SELECT ...) AS col` — scalar subquery

### Implementation

**Binder:** Already has `BoundExpr::Subquery`, `BoundExpr::Exists`, `BoundExpr::ScalarSubquery` variants (defined but not populated). Add handling in `bind_expr()` for `ast::Expr::Subquery` and `ast::Expr::Exists`.

**Planner:** Add `ScalarExpr::Subquery` and `ScalarExpr::Exists` variants. Lower subquery BoundSelect into a nested LogicalPlan. For derived tables, lower the subquery in `bind_table_factor` when the table factor is a `Derived` variant.

**Executor:** For uncorrelated subqueries, evaluate once and cache the result. For `IN (subquery)`, materialize the subquery result into a HashSet and check membership. For `EXISTS`, execute until one row is found. For scalar subqueries, execute and take the first row's first column. For derived tables, execute the subquery as a sub-executor and wrap it.

### Testing

- `WHERE id IN (SELECT id FROM other_table)`
- `WHERE EXISTS (SELECT 1 FROM other_table WHERE ...)`
- `SELECT * FROM (SELECT id, name FROM t) AS sub`
- `SELECT (SELECT COUNT(*) FROM orders WHERE orders.user_id = users.id) FROM users` (correlated — stretch goal)
