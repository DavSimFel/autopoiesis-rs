# Task: Always-On Tier Architecture MVP

## Design (binding decisions from operator + Codex analysis)

### The Model
- T1 and T2 are always-on persistent sessions, created at server startup from `agents.toml`
- Session IDs are well-known, derived from config (e.g. `silas-t1`, `silas-t2`)
- T1/T2 are NEVER spawned at runtime — only via `agents.toml`
- T3 stays exactly as today: spawned by plan engine, ephemeral
- Inter-tier communication = `enqueue_message(target_session_id, ...)` via existing queue
- No new transport

### NOT in MVP
- Domain T2s (later)
- T3 reuse (later)
- Hot reload (later)
- enqueue_message tool for agents (later — shell `autopoiesis enqueue` is enough for now)

## What to Build

### 1. SessionRegistry (`src/session_registry.rs` or `src/session_registry/mod.rs`)

A startup-built registry that expands `agents.toml` into concrete always-on session specs.

```rust
pub struct SessionSpec {
    pub session_id: String,        // "silas-t1", "silas-t2"
    pub tier: String,              // "t1", "t2"
    pub config: Config,            // per-session Config clone with correct model/reasoning/tier
    pub description: String,       // human-readable
}

pub struct SessionRegistry {
    specs: HashMap<String, SessionSpec>,
}

impl SessionRegistry {
    pub fn from_config(config: &Config) -> Result<Self>;
    pub fn get(&self, session_id: &str) -> Option<&SessionSpec>;
    pub fn always_on_sessions(&self) -> Vec<&SessionSpec>;
}
```

The registry reads the existing `[agents.silas.t1]` and `[agents.silas.t2]` config sections and creates specs with:
- `silas-t1`: tier=t1, model from t1 config, shell tool surface
- `silas-t2`: tier=t2, model from t2 config, read_file tool surface

Session IDs: `{agent_name}-{tier}` (e.g. `silas-t1`, `silas-t2`).

### 2. Startup Session Creation

In `src/server/mod.rs` startup:
1. Build `SessionRegistry` from config
2. For each always-on session: `store.create_session(session_id, metadata)` (idempotent — `INSERT OR IGNORE`)
3. Store the registry in `ServerState`

### 3. ServerState Changes (`src/server/state.rs`)

Add `registry: SessionRegistry` to `ServerState`. Keep existing `config` for backward compat.

### 4. Per-Session Drain Loops

For each always-on session, spawn a persistent background drain task:

```rust
// In server startup, after creating sessions:
for spec in registry.always_on_sessions() {
    spawn_persistent_drain_loop(state.clone(), spec.session_id.clone());
}
```

Each drain loop:
- Runs forever (loop + sleep on empty queue)
- Claims messages from its session queue
- Builds turns using the session's tier-specific config (T1=shell, T2=read_file)
- Uses the existing `drain_queue_with_shared_store` or equivalent
- Handles provider construction per-turn using the session-spec config

### 5. Per-Session Turn Building

When a drain loop processes a message for `silas-t1`, it builds a T1 turn.
When a drain loop processes a message for `silas-t2`, it builds a T2 turn.

Use `build_turn_for_config()` with the session spec's `Config` — the tier is already resolved there.

### 6. Runtime Capability Manifest

At turn construction time, inject a system message block listing peer sessions:

```
## Available Sessions
- silas-t1: Fast operator-facing tier (shell)
- silas-t2: Deep analysis tier (read_file, planning)

To delegate work to another tier, use shell:
  autopoiesis enqueue --session silas-t2 "your task here"
```

This is assembled from the registry at turn-build time and appended to the identity context.

### 7. CLI Enqueue Command (`src/app/args.rs` + handler)

Add a new CLI subcommand:
```
autopoiesis enqueue --session <session_id> <message>
```

This just calls `store.enqueue_message(session_id, "user", message, "cli")`.

### 8. CLI Default Session

When running `autopoiesis "prompt"` or `autopoiesis` (REPL), default to `silas-t1`.

The existing `--session` flag already exists for named sessions. Make T1's well-known ID the default when no `--session` is provided.

### 9. Update Server HTTP/WS

When HTTP `POST /api/sessions/{id}/messages` or WS connects to a session:
- Look up the session in the registry
- Use the session spec's config for turn building (not the global config)
- Fall back to global config for non-registry sessions (ad-hoc, child sessions)

## Constraints

- All 624 tests must pass
- Existing ad-hoc sessions and child sessions must continue working
- The plan engine must work unchanged (T3 still spawned as today)
- No changes to session/jsonl.rs
- No changes to the plan engine internals
- Backward compatible: if agents.toml doesn't define t1/t2 explicitly, behavior is unchanged

## Order of Operations

1. SessionRegistry + tests
2. ServerState + startup session creation
3. Persistent drain loops
4. CLI enqueue command
5. CLI default to silas-t1
6. Per-session turn building in server drain path
7. Capability manifest injection
8. HTTP/WS per-session config lookup

Commit after each step passes `cargo test`.

When completely finished, run: openclaw system event --text "Done: always-on tier architecture MVP shipped" --mode now
