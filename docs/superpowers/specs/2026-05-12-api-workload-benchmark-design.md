# Social Feed API Workload Benchmark

## Purpose

Measure manifold-sql vs SQLite under a realistic concurrent API workload simulating a social media backend. This reflects SpinStack's actual use case: multiple concurrent HTTP requests hitting the same database.

## Schema

```sql
CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT, created_at INTEGER)
CREATE TABLE posts (id INTEGER PRIMARY KEY, author_id INTEGER, content TEXT, created_at INTEGER)
CREATE TABLE comments (id INTEGER PRIMARY KEY, post_id INTEGER, author_id INTEGER, body TEXT, created_at INTEGER)
CREATE TABLE likes (id INTEGER PRIMARY KEY, post_id INTEGER, user_id INTEGER)
```

## Operations

| Operation | Weight | SQL | Type |
|-----------|--------|-----|------|
| Create post | 15% | `INSERT INTO posts (author_id, content, created_at) VALUES (?, ?, ?)` | Write |
| Add comment | 15% | `INSERT INTO comments (post_id, author_id, body, created_at) VALUES (?, ?, ?, ?)` | Write |
| Like post | 15% | `INSERT INTO likes (post_id, user_id) VALUES (?, ?)` | Write |
| Get feed | 25% | `SELECT id, author_id, content, created_at FROM posts ORDER BY created_at DESC LIMIT 20` | Read |
| Get post detail | 20% | `SELECT id, author_id, body FROM comments WHERE post_id = ?` (after PK lookup on post) | Read |
| Get user profile | 10% | `SELECT COUNT(*) FROM posts WHERE author_id = ?` (after PK lookup on user) | Read |

Operations are chosen randomly per-thread according to these weights using a weighted distribution.

## Seed Data

Before benchmarking, pre-populate:
- 100 users
- 1,000 posts (random author_id from users)
- 5,000 comments (random post_id, random author_id)
- 10,000 likes (random post_id, random user_id)

Both databases get identical seed data.

## Concurrency Model

- Concurrency levels: 1, 2, 4, 8, 16 threads
- Each thread runs a tight loop picking random operations by weight
- Duration: 10 seconds per concurrency level
- Each thread tracks: operation count, per-operation latency (stored in a Vec for percentile calculation)
- SQLite uses a single `Connection` wrapped in `Mutex<Connection>` (simulating its single-writer model)
- Manifold uses `Database` directly (thread-safe via internal locking)

## Metrics

For each concurrency level, report:
- **Total ops/sec** (all threads combined)
- **Per-operation breakdown** (ops/sec by type)
- **p50 latency** (median)
- **p99 latency** (tail)

## Output Format

Printed to stdout as a formatted table:

```
=== Social Feed API Benchmark ===

Threads: 1
                    manifold        sqlite
  ops/sec           12,450          15,200
  p50 latency       78 us           64 us
  p99 latency       210 us          180 us

Threads: 4
                    manifold        sqlite
  ops/sec           11,800          14,100
  ...

Per-operation breakdown (4 threads):
  create_post       1,800/s         2,100/s
  add_comment       1,750/s         2,050/s
  ...
```

## Implementation

Single benchmark file: `crates/manifold-sql/benches/api_workload.rs`

Not using Criterion (it's designed for micro-benchmarks, not sustained workload measurement). This is a custom benchmark binary that prints results directly.

Add to `Cargo.toml`:
```toml
[[bench]]
name = "api_workload"
harness = false
```

## Success Criteria

The benchmark compiles, runs for both databases, produces comparable metrics, and gives us a clear picture of where manifold stands for concurrent API workloads.
