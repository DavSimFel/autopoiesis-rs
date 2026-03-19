# PLAN: Shell output cap with file-backed result storage

## Goal
Every shell result is saved to a file. Output below a threshold is also inline in history. Output above threshold: only metadata in history with a pointer to the file.

## Files to read

| File | Lines | What to look for |
|------|-------|-----------------|
| `src/tool.rs` | full | `Shell` struct, `execute()`, `run_with_timeout()`, how output is returned as String |
| `src/agent.rs` | 300-330 | Where tool results are appended to session — `session.append(ChatMessage::tool_result(...))` |
| `src/session.rs` | 1-40 | `Session` struct — `sessions_dir` field, `new()` constructor |
| `src/session.rs` | 180-240 | `append()` and `today_path()` — understand directory structure |
| `VISION.md` | search "Shell output is capped" | Design spec for this feature |

## Design

### Result storage
- Save every shell result to `{sessions_dir}/results/{call_id}.txt`
- Create `results/` directory on first use (lazy mkdir)
- `sessions_dir` is already known — it's `Session.sessions_dir`

### Threshold
- Add `output_cap_bytes: usize` to `Shell` struct (default 4096 = 4KB)
- This is the inline threshold, not a truncation limit — the full output always goes to the file

### Flow in agent.rs
After tool execution returns a result string:
1. Write full output to `{sessions_dir}/results/{call_id}.txt`
2. If `output.len() <= threshold`: append full output as tool result in session (current behavior)
3. If `output.len() > threshold`: append metadata-only tool result:
   ```
   [output exceeded inline limit ({lines} lines, {kb} KB) → results/{call_id}.txt]
   To read: cat results/{call_id}.txt
   To read specific lines: sed -n '10,20p' results/{call_id}.txt
   ```

### Where the cap logic lives
In `agent.rs`, right after `tool.execute()` returns and after guard redaction. New helper function:

```rust
fn cap_tool_output(
    sessions_dir: &Path,
    call_id: &str,
    output: String,
    threshold: usize,
) -> Result<String>
```

This function:
1. Creates `sessions_dir/results/` if needed
2. Writes `output` to `sessions_dir/results/{call_id}.txt`
3. Returns either the full output (if below threshold) or the metadata string

### Session awareness
`run_agent_loop` needs to know `sessions_dir`. Options:
- Pass `sessions_dir` as parameter (cleanest — Session already has it)
- Or access it via `session.sessions_dir()` (add a getter if needed)

Prefer: add `pub fn sessions_dir(&self) -> &Path` getter to Session, pass to `cap_tool_output`.

## Per-file changes

### `src/session.rs`
- Add `pub fn sessions_dir(&self) -> &Path` getter that returns `&self.sessions_dir`

### `src/agent.rs`
- Add `fn cap_tool_output(sessions_dir: &Path, call_id: &str, output: String, threshold: usize) -> Result<String>`
- In the tool execution block (~line 310-320), after `guard_text_output`:
  1. Call `cap_tool_output(session.sessions_dir(), &call.id, result, 4096)?`
  2. Use the returned string as the tool result content
- Add constant `const DEFAULT_OUTPUT_CAP_BYTES: usize = 4096;`

### `src/tool.rs`
- No changes needed — tool still returns the full output string, capping happens in agent.rs

## Tests

### In agent.rs
1. **test: tool output below threshold is inline and saved to file**
   - Mock provider returns a tool call
   - Tool output is 100 bytes
   - Assert: session history contains full output inline
   - Assert: `results/{call_id}.txt` file exists with full output

2. **test: tool output above threshold is capped with metadata pointer**
   - Mock provider returns a tool call
   - Tool output is 8KB
   - Assert: session history contains metadata string (not full output)
   - Assert: metadata mentions line count, KB size, file path
   - Assert: `results/{call_id}.txt` exists with full 8KB output

3. **test: cap_tool_output creates results directory**
   - Call with a non-existent results dir
   - Assert: directory created, file written

### In session.rs
4. **test: sessions_dir getter returns correct path**

## Order of operations

1. Add `sessions_dir()` getter to Session — `cargo test`
2. Add `cap_tool_output()` helper to agent.rs with `DEFAULT_OUTPUT_CAP_BYTES` constant — `cargo test` (no callers yet)
3. Wire `cap_tool_output()` into the tool execution block in `run_agent_loop` — `cargo test`
4. Add test for inline output (below threshold) — `cargo test`
5. Add test for capped output (above threshold) — `cargo test`
6. `cargo clippy` — no warnings
7. Commit: `feat: shell output cap with file-backed result storage`
8. Push: `git push origin feat/shell-output-cap --no-verify`
