# manifold-sql Performance Optimization Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining 3-30x performance gap between manifold-sql and SQLite on aggregate, join, update, delete, and bulk insert benchmarks.

**Architecture:** Two categories of fix: (1) push-down optimizations that avoid materializing rows when only aggregates are needed, and (2) write-path improvements that reduce per-statement overhead for UPDATE/DELETE by using RowidLookup instead of full scans. The key insight is that the current executor materializes ALL rows into `Vec<Vec<Value>>` before processing, which dominates read-side cost. On the write side, UPDATE/DELETE always scan the full table even when the predicate targets a single PK row.

**Tech Stack:** Rust, manifold-sql executor/optimizer, Criterion benchmarks.

**Benchmark baseline (current):**

| Benchmark | Manifold | SQLite | Ratio |
|---|---|---|---|
| aggregate/1K | 655 µs | 49 µs | 13.5x |
| aggregate/10K | 5.80 ms | 480 µs | 12.1x |
| group_by/10K | 6.85 ms | 2.48 ms | 2.7x |
| inner_join/1K | 529 µs | 42 µs | 12.5x |
| update_by_pk | 72 µs | 2.38 µs | 30x |
| delete_by_pk | 147 µs | 15.5 µs | 9.5x |
| single_insert | 70 µs | 8.1 µs | 8.7x |
| bulk_insert/1K | 9.28 ms | 721 µs | 12.9x |

---

## File Structure

| File | Change | Responsibility |
|------|--------|---------------|
| `src/executor/scan.rs` | Modify | Add `CountScan` for O(1) `COUNT(*)` via `table.len()` |
| `src/executor/aggregate.rs` | Modify | Detect scalar aggregate over scan → use `CountScan` or push-down path |
| `src/executor/join.rs` | Modify | Hash join for equi-joins (replace nested loop) |
| `src/executor/mod.rs` | Modify | Wire RowidLookup into UPDATE/DELETE row-finding; add hash join builder |
| `src/optimizer/rules/index_selection.rs` | Modify | Recognize PK predicates in UPDATE/DELETE plans |
| `src/planner/plan.rs` | Modify | Add `RowidUpdate`/`RowidDelete` plan nodes |
| `src/executor/explain.rs` | Modify | EXPLAIN output for new plan nodes |
| `benches/sqlite_comparison.rs` | Modify | Verify improvements |

---

### Task 1: O(1) COUNT(*) via table.len()

`SELECT COUNT(*) FROM t` currently scans every row. The manifold B-tree stores row count in metadata — `table.len()` is O(1).

**Files:**
- Modify: `src/executor/scan.rs`
- Modify: `src/executor/mod.rs`

- [ ] **Step 1: Add CountScan to scan.rs**

Add after `RowidRangeScan`:

```rust
/// O(1) row count: reads table.len() from B-tree metadata.
pub struct CountScan {
    count: Option<u64>,
}

impl CountScan {
    pub fn from_read_txn(
        txn: &manifold::ReadTransaction,
        table_name: &str,
    ) -> Result<Self> {
        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);

        let count = match txn.open_table(def) {
            Ok(table) => table.len().map_err(SqlError::Storage)?,
            Err(manifold::TableError::TableDoesNotExist(_)) => 0,
            Err(e) => return Err(SqlError::TableError(e)),
        };

        Ok(Self { count: Some(count) })
    }

    pub fn from_write_txn(
        txn: &manifold::WriteTransaction,
        table_name: &str,
    ) -> Result<Self> {
        let data_name: &'static str = Box::leak(format!("data_{table_name}").into_boxed_str());
        let def = TableDefinition::<u64, &[u8]>::new(data_name);
        let table = txn.open_table(def)?;
        let count = table.len().map_err(SqlError::Storage)?;
        Ok(Self { count: Some(count) })
    }
}

impl super::Executor for CountScan {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        Ok(self.count.take().map(|c| vec![Value::Integer(c as i64)]))
    }
}
```

- [ ] **Step 2: Detect `COUNT(*)` over `Scan` in the aggregate builder**

In `mod.rs`, in `build_read_query_executor`, before the general `LogicalPlan::Aggregate` match arm, add a fast-path check:

```rust
LogicalPlan::Aggregate {
    group_by,
    aggregates,
    input,
    schema,
} if group_by.is_empty()
    && aggregates.len() == 1
    && aggregates[0].func == AggregateFunc::Count
    && aggregates[0].arg.is_none()
    && matches!(input.as_ref(), LogicalPlan::Scan { .. }) =>
{
    // COUNT(*) with no GROUP BY over a plain scan → O(1) table.len()
    if let LogicalPlan::Scan { table_name, .. } = input.as_ref() {
        let count_scan = scan::CountScan::from_read_txn(txn, table_name)?;
        return Ok(Box::new(count_scan));
    }
    unreachable!()
}
```

Add the same in `build_write_query_executor` using `from_write_txn`.

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p manifold-sql`

- [ ] **Step 4: Test**

Run: `cargo test -p manifold-sql`
Expected: All 230 tests pass (COUNT(*) queries should now be O(1))

- [ ] **Step 5: Commit**

```bash
git add crates/manifold-sql/src/executor/scan.rs crates/manifold-sql/src/executor/mod.rs
git commit -m "perf(manifold-sql): O(1) COUNT(*) via table.len() metadata"
```

---

### Task 2: Direct rowid UPDATE (skip full scan)

`UPDATE t SET value = $1 WHERE id = $2` currently scans the entire table to find the row. With PK = rowid, this should be a single `table.get(rowid)` followed by a re-insert.

**Files:**
- Modify: `src/planner/plan.rs`
- Modify: `src/optimizer/rules/index_selection.rs`
- Modify: `src/executor/mod.rs`
- Modify: `src/executor/explain.rs`

- [ ] **Step 1: Add RowidUpdate plan node**

In `plan.rs`, add after `RowidRangeScan`:

```rust
/// Direct rowid UPDATE — finds the row by PK, applies SET expressions, re-inserts.
RowidUpdate {
    table_name: String,
    rowid_expr: ScalarExpr,
    assignments: Vec<(usize, ScalarExpr)>,
},
```

In the `schema()` method, add `LogicalPlan::RowidUpdate { .. } => None,` to the match.

- [ ] **Step 2: Optimizer: convert Filter+Scan UPDATE to RowidUpdate**

In `index_selection.rs`, the `select_plan` function already handles `Filter { Scan }` for SELECT. But UPDATE plans are `LogicalPlan::Update { table_name, assignments, input }` where `input` is `Filter { Scan }`. Add handling:

```rust
LogicalPlan::Update {
    table_name,
    assignments,
    input,
} => {
    let input = select_plan(*input, catalog)?;
    // Check if the resolved input is a RowidLookup (PK = value)
    if let LogicalPlan::RowidLookup {
        rowid_expr,
        ..
    } = &input
    {
        return Ok(LogicalPlan::RowidUpdate {
            table_name,
            rowid_expr: rowid_expr.clone(),
            assignments,
        });
    }
    Ok(LogicalPlan::Update {
        table_name,
        assignments,
        input: Box::new(input),
    })
}
```

- [ ] **Step 3: Implement RowidUpdate executor**

In `mod.rs`, add `execute_rowid_update`:

```rust
fn execute_rowid_update(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    rowid_expr: &ScalarExpr,
    assignments: &[(usize, ScalarExpr)],
    params: &[Value],
) -> Result<u64> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();
    let col_types = schema.column_types();

    let rowid = match evaluate(rowid_expr, &[], params)? {
        Value::Integer(i) if i > 0 => i as u64,
        _ => return Ok(0),
    };

    let data_name = data_table_name(table_name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    let mut data_table = txn.open_table(def)?;

    let existing = data_table.get(rowid).map_err(SqlError::Storage)?;
    let Some(guard) = existing else {
        return Ok(0);
    };

    let mut row = vec![Value::Integer(rowid as i64)];
    row.extend(decode_row(&col_types, guard.value())?);
    drop(guard);

    // Apply assignments (shift column index by 1 for rowid prefix)
    let mut new_row = row.clone();
    for &(col_idx, ref expr) in assignments {
        new_row[col_idx + 1] = evaluate(expr, &row, params)?;
    }

    // Re-encode and insert (same rowid)
    let encoded = encode_row(&col_types, &new_row[1..])?;
    data_table.insert(rowid, encoded.as_slice())?;

    // Update indexes
    let indexes: Vec<_> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();
    update_indexes_delete(txn, table_name, &indexes, &row[1..], rowid)?;
    update_indexes_insert(txn, table_name, &indexes, &new_row[1..], rowid)?;

    Ok(1)
}
```

Wire it into `execute_plan_mut`:

```rust
LogicalPlan::RowidUpdate {
    table_name,
    rowid_expr,
    assignments,
} => execute_rowid_update(txn, catalog, table_name, rowid_expr, assignments, params),
```

And into `execute_plan_mut_buffered` with the same pattern.

- [ ] **Step 4: Add EXPLAIN support**

In `explain.rs`:
```rust
LogicalPlan::RowidUpdate {
    table_name,
    rowid_expr,
    ..
} => {
    out.push_str(&format!(
        "{pfx}RowidUpdate {table_name} [{}]\n",
        format_expr_brief(rowid_expr)
    ));
}
```

- [ ] **Step 5: Verify compilation and tests**

Run: `cargo check -p manifold-sql`
Run: `cargo test -p manifold-sql`
Expected: All pass

- [ ] **Step 6: Commit**

```bash
git add crates/manifold-sql/src/planner/plan.rs crates/manifold-sql/src/optimizer/rules/index_selection.rs crates/manifold-sql/src/executor/mod.rs crates/manifold-sql/src/executor/explain.rs
git commit -m "perf(manifold-sql): direct rowid UPDATE skips full table scan"
```

---

### Task 3: Direct rowid DELETE (skip full scan)

Same pattern as Task 2 but for DELETE.

**Files:**
- Modify: `src/planner/plan.rs`
- Modify: `src/optimizer/rules/index_selection.rs`
- Modify: `src/executor/mod.rs`
- Modify: `src/executor/explain.rs`

- [ ] **Step 1: Add RowidDelete plan node**

In `plan.rs`:

```rust
/// Direct rowid DELETE — finds the row by PK and removes it.
RowidDelete {
    table_name: String,
    rowid_expr: ScalarExpr,
},
```

Add `LogicalPlan::RowidDelete { .. } => None,` to `schema()`.

- [ ] **Step 2: Optimizer rule**

In `index_selection.rs`, add in `select_plan`:

```rust
LogicalPlan::Delete {
    table_name,
    input,
} => {
    let input = select_plan(*input, catalog)?;
    if let LogicalPlan::RowidLookup {
        rowid_expr,
        ..
    } = &input
    {
        return Ok(LogicalPlan::RowidDelete {
            table_name,
            rowid_expr: rowid_expr.clone(),
        });
    }
    Ok(LogicalPlan::Delete {
        table_name,
        input: Box::new(input),
    })
}
```

- [ ] **Step 3: Implement RowidDelete executor**

In `mod.rs`, add `execute_rowid_delete`:

```rust
fn execute_rowid_delete(
    txn: &manifold::WriteTransaction,
    catalog: &mut Catalog,
    table_name: &str,
    rowid_expr: &ScalarExpr,
    params: &[Value],
) -> Result<u64> {
    let schema = catalog
        .get_table(table_name)
        .ok_or_else(|| SqlError::TableNotFound(table_name.to_string()))?
        .clone();
    let col_types = schema.column_types();

    let rowid = match evaluate(rowid_expr, &[], params)? {
        Value::Integer(i) if i > 0 => i as u64,
        _ => return Ok(0),
    };

    let data_name = data_table_name(table_name);
    let def = TableDefinition::<u64, &[u8]>::new(data_name);
    let mut data_table = txn.open_table(def)?;

    let existing = data_table.get(rowid).map_err(SqlError::Storage)?;
    let Some(guard) = existing else {
        return Ok(0);
    };

    let mut row = vec![Value::Integer(rowid as i64)];
    row.extend(decode_row(&col_types, guard.value())?);
    drop(guard);

    // Remove from data table
    data_table.remove(rowid).map_err(SqlError::Storage)?;

    // Update indexes
    let indexes: Vec<_> = catalog
        .indexes_for_table(schema.id)
        .into_iter()
        .cloned()
        .collect();
    update_indexes_delete(txn, table_name, &indexes, &row[1..], rowid)?;

    // Check FK constraints (parent side)
    check_fk_on_delete(txn, catalog, &schema, &row[1..])?;

    Ok(1)
}
```

Wire into `execute_plan_mut` and `execute_plan_mut_buffered`.

- [ ] **Step 4: Add EXPLAIN support and verify**

Add `RowidDelete` to `explain.rs`. Run `cargo test -p manifold-sql`. Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/manifold-sql/src/planner/plan.rs crates/manifold-sql/src/optimizer/rules/index_selection.rs crates/manifold-sql/src/executor/mod.rs crates/manifold-sql/src/executor/explain.rs
git commit -m "perf(manifold-sql): direct rowid DELETE skips full table scan"
```

---

### Task 4: Hash join for equi-joins

The current join is nested-loop O(n*m). For equi-joins (`ON a.id = b.a_id`), a hash join is O(n+m): build a hash table on the smaller side, probe with the larger side.

**Files:**
- Modify: `src/executor/join.rs`

- [ ] **Step 1: Add HashJoinExecutor**

Add to `join.rs` after `NestedLoopJoin`:

```rust
/// Hash join for equi-join conditions. Builds a hash table on the right
/// side, then probes with each left row. O(n+m) vs O(n*m) for nested loop.
pub struct HashJoinExecutor {
    left: Box<dyn super::Executor>,
    /// Hash table: join key → list of matching right rows
    hash_table: HashMap<Vec<u8>, Vec<Vec<Value>>>,
    /// Current left row being probed
    current_left: Option<Vec<Value>>,
    /// Iterator over matching right rows for current left
    match_idx: usize,
    current_matches: Vec<Vec<Value>>,
    /// Column index in left row to use as join key
    left_key_idx: usize,
    /// Column index in right row that was used to build hash
    right_key_idx: usize,
    left_skip: usize,
    right_skip: usize,
    left_matched: bool,
    join_type: JoinType,
    right_width: usize,
    done: bool,
}

impl HashJoinExecutor {
    pub fn new(
        mut left: Box<dyn super::Executor>,
        mut right: Box<dyn super::Executor>,
        left_key_idx: usize,
        right_key_idx: usize,
        left_skip: usize,
        right_skip: usize,
        join_type: JoinType,
    ) -> Result<Self> {
        // Build hash table from right side
        let mut hash_table: HashMap<Vec<u8>, Vec<Vec<Value>>> = HashMap::new();
        let mut right_width = 0;
        while let Some(row) = right.next()? {
            right_width = row.len();
            let key = hash_key(&row[right_key_idx]);
            hash_table.entry(key).or_default().push(row);
        }

        Ok(Self {
            left,
            hash_table,
            current_left: None,
            match_idx: 0,
            current_matches: Vec::new(),
            left_key_idx,
            right_key_idx,
            left_skip,
            right_skip,
            left_matched: false,
            join_type,
            right_width,
            done: false,
        })
    }
}

fn hash_key(val: &Value) -> Vec<u8> {
    // Simple byte encoding for hash lookup
    match val {
        Value::Integer(i) => i.to_le_bytes().to_vec(),
        Value::BigInt(i) => i.to_le_bytes().to_vec(),
        Value::Text(s) => s.as_bytes().to_vec(),
        Value::Null => vec![0xFF],
        other => format!("{other:?}").into_bytes(),
    }
}

fn combine_rows(
    left: &[Value],
    right: &[Value],
    left_skip: usize,
    right_skip: usize,
) -> Vec<Value> {
    let mut out = Vec::with_capacity(left.len() - left_skip + right.len() - right_skip);
    out.extend_from_slice(&left[left_skip..]);
    out.extend_from_slice(&right[right_skip..]);
    out
}

impl super::Executor for HashJoinExecutor {
    fn next(&mut self) -> Result<Option<Vec<Value>>> {
        if self.done {
            return Ok(None);
        }
        loop {
            // Yield remaining matches for current left row
            if self.match_idx < self.current_matches.len() {
                let right = &self.current_matches[self.match_idx];
                self.match_idx += 1;
                self.left_matched = true;
                let combined = combine_rows(
                    self.current_left.as_ref().unwrap(),
                    right,
                    self.left_skip,
                    self.right_skip,
                );
                return Ok(Some(combined));
            }

            // Emit NULL-padded left row if LEFT JOIN and no matches
            if matches!(self.join_type, JoinType::Left)
                && self.current_left.is_some()
                && !self.left_matched
            {
                let left = self.current_left.take().unwrap();
                let nulls = vec![Value::Null; self.right_width - self.right_skip];
                let mut out = left[self.left_skip..].to_vec();
                out.extend(nulls);
                return Ok(Some(out));
            }

            // Fetch next left row
            match self.left.next()? {
                Some(left_row) => {
                    let key = hash_key(&left_row[self.left_key_idx]);
                    self.current_matches = self
                        .hash_table
                        .get(&key)
                        .cloned()
                        .unwrap_or_default();
                    self.match_idx = 0;
                    self.left_matched = false;
                    self.current_left = Some(left_row);
                }
                None => {
                    self.done = true;
                    return Ok(None);
                }
            }
        }
    }
}
```

- [ ] **Step 2: Detect equi-join condition and use HashJoin**

In `mod.rs`, update the `LogicalPlan::Join` handler to detect equi-join conditions of the form `col_a = col_b` and use `HashJoinExecutor`:

```rust
LogicalPlan::Join {
    join_type,
    left,
    right,
    condition,
    schema,
} => {
    let left_has_scan = plan_has_scan_leaf(left);
    let right_has_scan = plan_has_scan_leaf(right);
    let left_exec = build_read_query_executor(txn, catalog, left, params)?;
    let right_exec = build_read_query_executor(txn, catalog, right, params)?;
    let left_skip = if left_has_scan { 1 } else { 0 };
    let right_skip = if right_has_scan { 1 } else { 0 };

    // Try hash join for equi-join conditions
    if let Some(cond) = condition {
        if let Some((left_idx, right_idx)) = extract_equi_join_keys(&cond, left_skip, right_skip, left.schema().map_or(0, |s| s.columns.len()) + left_skip) {
            let executor = join::HashJoinExecutor::new(
                left_exec, right_exec,
                left_idx, right_idx,
                left_skip, right_skip,
                join_type.clone(),
            )?;
            return Ok(Box::new(executor));
        }
    }

    // Fall back to nested loop for non-equi joins
    let cond = condition.as_ref().map(|c| { /* existing shift logic */ });
    Ok(Box::new(join::NestedLoopJoin::new(left_exec, right_exec, cond, left_skip, right_skip, join_type.clone(), params.to_vec())?))
}
```

Add helper:

```rust
/// Extract (left_col_idx, right_col_idx) from an equi-join condition like `col_a = col_b`.
fn extract_equi_join_keys(
    cond: &ScalarExpr,
    left_skip: usize,
    right_skip: usize,
    left_width: usize,
) -> Option<(usize, usize)> {
    if let ScalarExpr::BinaryOp {
        op: BinaryOp::Eq,
        left,
        right,
    } = cond
    {
        if let (ScalarExpr::ColumnRef { index: li }, ScalarExpr::ColumnRef { index: ri }) =
            (left.as_ref(), right.as_ref())
        {
            // Determine which column belongs to which side
            let li = *li + left_skip;
            let ri = *ri + left_skip;
            if li < left_width && ri >= left_width {
                return Some((li, ri - left_width + right_skip));
            }
            if ri < left_width && li >= left_width {
                return Some((ri, li - left_width + right_skip));
            }
        }
    }
    None
}
```

- [ ] **Step 3: Verify compilation and tests**

Run: `cargo check -p manifold-sql`
Run: `cargo test -p manifold-sql`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add crates/manifold-sql/src/executor/join.rs crates/manifold-sql/src/executor/mod.rs
git commit -m "perf(manifold-sql): hash join for equi-join conditions"
```

---

### Task 5: Benchmark verification

- [ ] **Step 1: Run full SQLite comparison benchmark**

```bash
cargo bench -p manifold-sql --bench sqlite_comparison -- --noplot 2>/dev/null > /tmp/bench_final.txt
```

- [ ] **Step 2: Compare results**

Extract and compare all `time:` lines. Target improvements:

| Benchmark | Target |
|---|---|
| aggregate/1K | < 3x (from 13.5x, via COUNT(*) optimization) |
| update_by_pk | < 5x (from 30x, via RowidUpdate) |
| delete_by_pk | < 5x (from 9.5x, via RowidDelete) |
| inner_join/1K | < 5x (from 12.5x, via hash join) |

- [ ] **Step 3: Commit benchmark results**

```bash
git add crates/manifold-sql/benches/sqlite_comparison.rs
git commit -m "bench: SQLite comparison benchmark with all optimizations"
```
