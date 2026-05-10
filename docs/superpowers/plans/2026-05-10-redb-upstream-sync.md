# Redb Upstream Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Incrementally merge upstream redb changes (v3.1.1 through upstream/master) into the manifold fork to pick up critical bug fixes, performance improvements, and new features.

**Architecture:** Merge each upstream release tag one at a time into manifold's master branch. Each merge is tested independently before proceeding to the next. Conflicts are resolved by preserving manifold's additions (column families, WAL, WASM) while accepting upstream's changes to shared core files. Our savepoint fix (commit 1c974d5) will be superseded by upstream's more comprehensive fixes in v4.1.0.

**Tech Stack:** Git merge, Rust/Cargo, existing test suite (180+ tests including 25 stress tests)

**Upstream remote:** `upstream` pointing to `https://github.com/cberner/redb.git` (already added)

---

## Conflict Analysis Summary

Files changed by BOTH manifold and upstream:

| File | Manifold changes | Conflict risk |
|------|-----------------|---------------|
| `src/transactions.rs` | WAL integration, CF support, savepoint fix | **HIGH** — upstream has major refactors + savepoint fixes |
| `src/db.rs` | CF database, builder changes | **HIGH** — upstream has PageAllocator refactor |
| `src/table.rs` | Minor (get_bulk, ReadableDatabase) | MEDIUM — upstream adds entry() API, get_mut fix |
| `src/tree_store/table_tree.rs` | clear_pending_table_updates, minor | MEDIUM — upstream has refactors |
| `src/tree_store/page_store/page_manager.rs` | CF partition support | MEDIUM — upstream has PageAllocator |
| `src/tree_store/page_store/cached_file.rs` | CF file handle pool | MEDIUM — upstream has cache partitioning |
| `src/lib.rs` | Column family module export | LOW — mostly additive |
| `src/tree_store/mod.rs` | Module re-exports | LOW |
| `src/tree_store/page_store/backends.rs` | Minor | LOW |
| `src/tree_store/page_store/lru_cache.rs` | Minor | LOW |
| `src/tree_store/page_store/mod.rs` | Minor | LOW |

Manifold-only files (no conflicts): `src/column_family/**`, `src/wasm.rs`

---

## Pre-Merge: Preparation

### Task 0: Create sync branch and verify clean state

**Files:**
- None (git operations only)

- [ ] **Step 1: Create a dedicated branch for the sync work**

```bash
git checkout -b sync/redb-upstream
```

- [ ] **Step 2: Verify all tests pass on current master**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: All test binaries show `0 failed` (the only failure is a pre-existing doc-test)

- [ ] **Step 3: Verify upstream remote is configured**

```bash
git remote -v | grep upstream
git fetch upstream
```

Expected: `upstream` points to `https://github.com/cberner/redb.git`

- [ ] **Step 4: Commit nothing — just verify clean working tree**

```bash
git status
```

Expected: `nothing to commit, working tree clean`

---

## Phase 1: Merge v3.1.1

**Upstream changes (3 commits):**
- `eb1c9da` Fix panic inserting into fixed size key table
- `c14d694` Bump dependencies (pyo3, rusqlite, rand)
- `b752306` Add additional information to cache stats

**Expected conflicts:** `src/db.rs` (cache stats), `src/tree_store/page_store/cached_file.rs`
**Risk:** LOW — small patch release, mostly bug fixes

### Task 1: Merge v3.1.1

**Files:**
- Modify: `src/db.rs` (conflict resolution)
- Modify: `src/tree_store/page_store/cached_file.rs` (conflict resolution)

- [ ] **Step 1: Attempt the merge**

```bash
git merge v3.1.1 --no-commit
```

If no conflicts, skip to Step 3. If conflicts, proceed to Step 2.

- [ ] **Step 2: Resolve conflicts**

For each conflicted file, the strategy is:
- **Accept upstream changes** to core redb logic (bug fixes, cache stats)
- **Preserve manifold additions** (column family code, WAL integration, any manifold-specific modifications)
- Use `git diff --check` to verify no conflict markers remain

```bash
# List conflicted files
git diff --name-only --diff-filter=U

# For each file, open and resolve, then:
git add <resolved-file>
```

Conflict resolution principles:
- If upstream added new code that doesn't conflict with manifold, accept it
- If upstream modified a function that manifold also modified, keep both changes by integrating upstream's fix into manifold's version of the function
- If upstream renamed/restructured something manifold depends on, update manifold's code to use the new names/structure

- [ ] **Step 3: Run the full test suite**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: All test binaries show `0 failed`

- [ ] **Step 4: Run stress tests specifically**

```bash
cargo test --test stress_tests 2>&1 | tail -5
```

Expected: `25 passed; 0 failed`

- [ ] **Step 5: Commit the merge**

```bash
git commit -m "merge: sync with upstream redb v3.1.1

Upstream changes:
- Fix panic inserting into fixed size key table
- Add additional information to cache stats
- Bump dependencies"
```

---

## Phase 2: Merge v3.1.2

**Upstream changes (1 commit):**
- `6340178` perf: lazily size RegionTracker bitmaps to reduce memory overhead

**Expected conflicts:** None (bitmap.rs and region.rs not modified by manifold)
**Risk:** MINIMAL

### Task 2: Merge v3.1.2

**Files:**
- Modify: `src/tree_store/page_store/bitmap.rs` (upstream only)
- Modify: `src/tree_store/page_store/region.rs` (upstream only)

- [ ] **Step 1: Merge**

```bash
git merge v3.1.2 --no-commit
```

Expected: Clean merge (no conflicts)

- [ ] **Step 2: Run full test suite**

```bash
cargo test 2>&1 | grep "test result"
```

Expected: All pass

- [ ] **Step 3: Run stress tests**

```bash
cargo test --test stress_tests 2>&1 | tail -5
```

Expected: `25 passed; 0 failed`

- [ ] **Step 4: Commit**

```bash
git commit -m "merge: sync with upstream redb v3.1.2

Upstream changes:
- perf: lazily size RegionTracker bitmaps to reduce memory overhead"
```

---

## Phase 3: Merge v3.1.3

**Upstream changes (3 commits):**
- `50e100b` Hacky fix for potential data loss when using Table::get_mut()
- `99dd419` Add deprecation warning to Table::get_mut()
- `96a8c8a` Correct deprecation warning

**Expected conflicts:** `src/table.rs` (manifold added get_bulk, ReadableDatabase trait)
**Risk:** LOW — changes are localized to get_mut()

### Task 3: Merge v3.1.3

**Files:**
- Modify: `src/table.rs` (conflict resolution — upstream's get_mut fix + manifold's additions)

- [ ] **Step 1: Merge**

```bash
git merge v3.1.3 --no-commit
```

- [ ] **Step 2: Resolve any conflicts in src/table.rs**

The upstream changes add a deprecation warning and safety fix to `get_mut()`. Manifold's changes to this file are `get_bulk`, `insert_bulk`, `remove_bulk` — different functions. Conflicts should be positional only (nearby lines), not semantic.

```bash
git diff --name-only --diff-filter=U
# Resolve conflicts, keeping both sides
git add src/table.rs
```

- [ ] **Step 3: Run full test suite**

```bash
cargo test 2>&1 | grep "test result"
```

- [ ] **Step 4: Run stress tests**

```bash
cargo test --test stress_tests 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git commit -m "merge: sync with upstream redb v3.1.3

Upstream changes:
- Fix potential data loss when using Table::get_mut()
- Add deprecation warning to Table::get_mut()"
```

---

## Phase 4: Merge v4.0.0

**Upstream changes (4 commits):**
- `97fc5ea` Remove Legacy type
- `df5759e` Add a lifetime to PageMut
- `8a3524a` Add Drop impl to AccessGuardMut
- `9cce627` Add Drop impl to AccessGuardMutInPlace

**Expected conflicts:** `src/lib.rs` (manifold exports Legacy), `src/table.rs`, `src/tree_store/page_store/page_manager.rs`
**Risk:** MEDIUM — `Legacy` removal may affect manifold if it re-exports it. PageMut lifetime change may affect WAL/CF code.

### Task 4: Merge v4.0.0

**Files:**
- Modify: `src/lib.rs` (remove Legacy re-export if conflicted)
- Modify: `src/table.rs` (AccessGuardMut Drop)
- Modify: `src/tree_store/page_store/page_manager.rs` (PageMut lifetime)
- Possibly modify: `src/column_family/**` (if PageMut lifetime change breaks CF code)
- Delete: `src/legacy_tuple_types.rs` (removed upstream)

- [ ] **Step 1: Merge**

```bash
git merge v4.0.0 --no-commit
```

- [ ] **Step 2: Resolve conflicts**

Key decisions:
- **Legacy type removal:** Manifold re-exports `Legacy` in `src/lib.rs`. Remove the re-export since upstream removed the type entirely. Check if any manifold code uses it (likely only the backward_compatibility test).
- **PageMut lifetime:** Upstream adds a lifetime parameter. Manifold's column family and WAL code uses `PageMut` — will need the lifetime parameter added.
- **AccessGuardMut Drop:** Accept upstream's Drop impl. Check manifold code for any manual drop patterns that might conflict.

```bash
git diff --name-only --diff-filter=U
# Resolve each file
git add <resolved-files>
```

- [ ] **Step 3: Fix any compilation errors from API changes**

```bash
cargo check 2>&1 | head -50
```

If the `Legacy` type removal breaks `src/lib.rs` or `tests/legacy_tuple_types_tests.rs`:
- Remove `pub use legacy_tuple_types::Legacy;` from `src/lib.rs`
- Remove or update `tests/legacy_tuple_types_tests.rs` if it depends on the removed type
- Check `tests/backward_compatibility.rs` for Legacy usage

If PageMut lifetime changes break column family code:
- Update function signatures in `src/column_family/` files to propagate the new lifetime

- [ ] **Step 4: Run full test suite**

```bash
cargo test 2>&1 | grep -E "(FAILED|test result)"
```

- [ ] **Step 5: Run stress tests**

```bash
cargo test --test stress_tests 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git commit -m "merge: sync with upstream redb v4.0.0

BREAKING: Upstream removed Legacy type and added lifetime to PageMut.
AccessGuardMut and AccessGuardMutInPlace now implement Drop.

Upstream changes:
- Remove Legacy type (breaking)
- Add lifetime to PageMut (breaking)  
- Add Drop impl to AccessGuardMut
- Add Drop impl to AccessGuardMutInPlace"
```

---

## Phase 5: Merge v4.1.0

**Upstream changes (29 commits) — THIS IS THE CRITICAL MERGE**

Key changes:
- 5 savepoint bug fixes (supersedes our fix in commit 1c974d5)
- Fix panic in delete_table() when modified in same txn
- Fix data corruption when renaming tables with dirty checksums
- Fix panic in BtreeRangeIter::next_back
- Fix MultimapValue::next_back()
- Dynamic read/write cache partitioning
- Optimize write performance
- Consolidate WriteTransaction savepoint lifecycle
- Add SavepointError::ImmediateDurabilityRequired
- PageAllocator abstraction

**Expected conflicts:** `src/transactions.rs` (HEAVY), `src/db.rs`, `src/table.rs`, `src/tree_store/table_tree.rs`, `src/tree_store/page_store/page_manager.rs`, `src/tree_store/page_store/cached_file.rs`, `src/tree_store/page_store/backends.rs`
**Risk:** HIGH — upstream heavily refactored transactions.rs which is also manifold's most modified file

### Task 5: Merge v4.1.0

**Files:**
- Modify: `src/transactions.rs` (heavy conflict resolution)
- Modify: `src/db.rs` (conflict resolution)
- Modify: `src/table.rs` (conflict resolution)
- Modify: `src/tree_store/table_tree.rs` (conflict resolution — our savepoint fix will be superseded)
- Modify: `src/tree_store/page_store/page_manager.rs` (conflict resolution)
- Modify: `src/tree_store/page_store/cached_file.rs` (conflict resolution)
- Modify: `src/tree_store/page_store/backends.rs` (conflict resolution)
- Modify: `src/tree_store/page_store/base.rs` (upstream adds PageTrackerPolicy::reset)

- [ ] **Step 1: Merge**

```bash
git merge v4.1.0 --no-commit
```

- [ ] **Step 2: List all conflicts and assess scope**

```bash
git diff --name-only --diff-filter=U
# Count conflict markers per file:
for f in $(git diff --name-only --diff-filter=U); do echo "$f: $(grep -c '<<<<<<<' $f) conflicts"; done
```

- [ ] **Step 3: Resolve src/transactions.rs (hardest file)**

This file has manifold's WAL integration, CF support, and our savepoint fix. Upstream has:
- Savepoint lifecycle consolidation
- `PageTrackerPolicy::reset()` usage in `restore_savepoint()`
- `freed_pages.clear()` in `restore_savepoint()`
- `pending_table_updates` clearing in `set_root()` via `table_tree.rs`
- Various other fixes

Strategy:
1. Accept ALL upstream changes to `restore_savepoint()` and savepoint-related methods — they're more comprehensive than our fix
2. Re-apply manifold's WAL integration code (the `wal_journal`, `cf_name`, `checkpoint_manager` fields and their usage in `commit_inner`)
3. Re-apply manifold's column family transaction support
4. Verify our `clear_pending_table_updates()` addition to `table_tree.rs` is now redundant (upstream clears pending in `set_root`) — if so, remove our addition

```bash
git add src/transactions.rs
```

- [ ] **Step 4: Resolve src/tree_store/table_tree.rs**

Upstream now clears `pending_table_updates` in `set_root()`. Our `clear_pending_table_updates()` helper is redundant. Accept upstream's version and remove our helper if upstream's `set_root` already does the clear.

```bash
git add src/tree_store/table_tree.rs
```

- [ ] **Step 5: Resolve remaining conflicted files**

For each remaining file (`src/db.rs`, `src/table.rs`, `src/tree_store/page_store/*`):
- Accept upstream's core logic changes
- Preserve manifold's column family and WAL additions
- Resolve positional conflicts by keeping both sides where they don't semantically overlap

```bash
for f in $(git diff --name-only --diff-filter=U); do
  echo "Resolving: $f"
  # resolve...
  git add "$f"
done
```

- [ ] **Step 6: Compile check**

```bash
cargo check 2>&1 | head -80
```

Fix any compilation errors. Common issues:
- Upstream renamed types/methods that manifold uses
- New function signatures (e.g., `PageTrackerPolicy::reset()`) need to be used by manifold code
- New error variants that manifold needs to handle

- [ ] **Step 7: Run full test suite**

```bash
cargo test 2>&1 | grep -E "(FAILED|test result)"
```

- [ ] **Step 8: Run stress tests — critical for savepoint validation**

```bash
cargo test --test stress_tests 2>&1 | tail -10
```

Expected: All 25 pass. The `stress_savepoint_rollback` test now validates upstream's fix instead of ours.

- [ ] **Step 9: Run upstream's integration tests for savepoint regressions**

```bash
cargo test --test integration_tests savepoint 2>&1 | grep -E "(test |FAILED)"
```

Expected: All savepoint tests pass

- [ ] **Step 10: Commit**

```bash
git commit -m "merge: sync with upstream redb v4.1.0

Critical bug fixes from upstream:
- Fix restore_savepoint() to always revert changes (supersedes our 1c974d5)
- Fix data loss in restore_savepoint() by clearing freed_pages
- Fix aborting restore_savepoint()
- Fix bug in restore_savepoint() when durability is not Immediate
- Fix page leak when aborting txn with persistent savepoint
- Fix panic in delete_table() when table modified in same txn
- Fix data corruption when renaming tables with dirty checksums
- Fix panic in BtreeRangeIter::next_back after forward exhaustion
- Fix MultimapValue::next_back() remaining counter

Performance improvements:
- Dynamic read/write cache partitioning
- Optimized write performance
- Various allocation hygiene improvements

New features:
- SavepointError::ImmediateDurabilityRequired
- PageAllocator abstraction"
```

---

## Phase 6: Merge upstream/master (post-v4.1.0)

**Upstream changes (62 commits):**
- Major retain/extract_if optimization
- PageAllocator → PageResolver refactor
- Freed page reuse improvement
- Read cache optimization
- Non-durable commit perf fix
- Table::entry() API
- MultimapValue zero-copy iteration
- pop_first/pop_last optimization
- Various structural refactors

**Expected conflicts:** Many files (10+). This is the largest merge.
**Risk:** HIGH — includes major structural refactors to page management and iterators

### Task 6: Merge upstream/master

**Files:**
- Heavy conflicts in: `src/transactions.rs`, `src/db.rs`, `src/table.rs`, `src/tree_store/page_store/page_manager.rs`, `src/tree_store/page_store/cached_file.rs`, `src/tree_store/table_tree.rs`
- New upstream files: `src/tree_store/extract_if.rs`, `src/tree_store/retain.rs`, `src/tree_store/subtree_rebuild.rs`, `src/tree_store/multimap_btree.rs`

- [ ] **Step 1: Merge**

```bash
git merge upstream/master --no-commit
```

- [ ] **Step 2: Assess conflict scope**

```bash
git diff --name-only --diff-filter=U
for f in $(git diff --name-only --diff-filter=U); do echo "$f: $(grep -c '<<<<<<<' $f) conflicts"; done
```

- [ ] **Step 3: Accept new upstream files (no conflicts)**

New files from upstream that don't exist in manifold — accept directly:
- `src/tree_store/extract_if.rs`
- `src/tree_store/retain.rs`
- `src/tree_store/subtree_rebuild.rs`
- `src/tree_store/multimap_btree.rs`

```bash
git checkout --theirs src/tree_store/extract_if.rs src/tree_store/retain.rs src/tree_store/subtree_rebuild.rs src/tree_store/multimap_btree.rs 2>/dev/null
git add src/tree_store/extract_if.rs src/tree_store/retain.rs src/tree_store/subtree_rebuild.rs src/tree_store/multimap_btree.rs 2>/dev/null
```

- [ ] **Step 4: Resolve conflicts file by file**

Same strategy as Phase 5:
- Accept upstream's core logic changes
- Preserve manifold's column family, WAL, and WASM additions
- For PageAllocator/PageResolver refactors, update manifold's code to use new abstractions

```bash
for f in $(git diff --name-only --diff-filter=U); do
  echo "Resolving: $f"
  # resolve...
  git add "$f"
done
```

- [ ] **Step 5: Fix compilation errors**

```bash
cargo check 2>&1 | head -80
```

Likely issues:
- `PageAllocator` replaced by `PageResolver` abstraction — manifold's page_manager usage needs updating
- `PageTrackerPolicy` may have new variants
- New module exports needed in `src/tree_store/mod.rs`
- `entry()` API additions to Table

- [ ] **Step 6: Run full test suite**

```bash
cargo test 2>&1 | grep -E "(FAILED|test result)"
```

- [ ] **Step 7: Run stress tests**

```bash
cargo test --test stress_tests 2>&1 | tail -10
```

- [ ] **Step 8: Commit**

```bash
git commit -m "merge: sync with upstream redb (post-v4.1.0, latest master)

Upstream changes:
- Table::entry() API mirroring BTreeMap::entry
- Optimized retain() and retain_in() with subtree rebuild
- PageResolver abstraction replacing PageAllocator::mem()
- Freed pages reused one transaction sooner after durable commits
- Avoid invalidating entire read cache on file resize
- Non-durable commit performance regression fix
- MultimapValue zero-copy iteration
- Optimized pop_first/pop_last (single tree descent)
- Extract_if iterator moved to own module with panic poisoning
- Various structural refactors and clippy fixes"
```

---

## Post-Merge: Validation

### Task 7: Final validation and cleanup

**Files:**
- Possibly modify: `tests/stress_tests.rs` (update if savepoint test needs adjustment)
- Possibly modify: `Cargo.toml` (version bump)

- [ ] **Step 1: Run full test suite one final time**

```bash
cargo test 2>&1 | grep -E "(FAILED|test result)"
```

All should pass.

- [ ] **Step 2: Run stress tests with extra attention to savepoint**

```bash
cargo test --test stress_tests -- --nocapture 2>&1 | tail -30
```

- [ ] **Step 3: Verify column family functionality**

```bash
cargo test --test column_family_tests 2>&1 | tail -5
cargo test --test wal_advanced_tests 2>&1 | tail -5
cargo test --test wal_integration_test 2>&1 | tail -5
```

- [ ] **Step 4: Verify crash recovery**

```bash
cargo test --test crash_recovery_tests 2>&1 | tail -5
```

- [ ] **Step 5: Check for any dead code or unused imports**

```bash
cargo check 2>&1 | grep "warning" | head -20
```

Fix any warnings introduced by the merge.

- [ ] **Step 6: Run clippy**

```bash
cargo clippy 2>&1 | grep -E "(warning|error)" | head -20
```

Fix any new clippy warnings.

- [ ] **Step 7: Verify integrity check works**

The stress test `stress_integrity_check_after_heavy_use` and `stress_compaction_integrity` both call `check_integrity()`. Confirm they still pass:

```bash
cargo test --test stress_tests stress_integrity 2>&1 | tail -5
cargo test --test stress_tests stress_compaction_integrity 2>&1 | tail -5
```

- [ ] **Step 8: Merge sync branch into master**

```bash
git checkout master
git merge sync/redb-upstream
```

- [ ] **Step 9: Commit message for the merge to master**

This is a fast-forward or merge commit depending on strategy. No additional commit message needed if fast-forward.

---

## Rollback Plan

If any merge step produces unfixable conflicts or introduces regressions:

1. **Abort the current merge:** `git merge --abort`
2. **Fall back to cherry-picking:** Instead of merging the entire release tag, cherry-pick only the critical bug fixes from that release
3. **Document skipped commits:** Note which commits were skipped and why in a comment on the merge commit

Critical bug fixes that should be cherry-picked even if full merge fails:

| Commit | Description | Release |
|--------|------------|---------|
| `eb1c9da` | Fix panic inserting into fixed size key table | v3.1.1 |
| `50e100b` | Fix data loss with Table::get_mut() | v3.1.3 |
| `4550d5a` | Fix restore_savepoint() pending_table_updates | v4.1.0 |
| `19f77ab` | Fix restore_savepoint() freed_pages corruption | v4.1.0 |
| `75bf290` | Fix restore_savepoint() non-Immediate durability | v4.1.0 |
| `44a9ff5` | Fix aborting restore_savepoint() | v4.1.0 |
| `30c0c43` | Fix panic in delete_table() same txn | v4.1.0 |
| `3b16d2f` | Fix corruption renaming tables dirty checksums | v4.1.0 |
| `c7ee5c6` | Fix panic BtreeRangeIter::next_back | v4.1.0 |
| `d19960a` | Fix perf regression non-durable commits | post-v4.1.0 |
| `f6d88c9` | Fix compact() growing file on drop | post-v4.1.0 |
