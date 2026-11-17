# rowser

Rust tool: headless remote hosts open URLs in the local machine's browser over SSH, and OAuth
`localhost` callbacks are forwarded back.

Read before changing code:

- `docs/spec.md` — what v1 does. The spec wins over a ticket unless the user decides otherwise.
- `GLOSSARY.md` — use these terms in code, docs and tickets; avoid the listed synonyms.
- `docs/adr/` — decisions that look odd without context. Do not reverse one without a new ADR.
- `docs/design.md` — module layout, interfaces and seams. Test through module interfaces.

Checks: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

Commit messages: do not add `Co-Authored-By` lines.

Ticket loop: see docs/agents/ticket-loop.md
