# Contributing to Sieve

Thank you for your help. This guide is short. Read it before you open a pull request.

## Build

```
cargo build
```

## Run the gate

Run the gate before every commit.

```
scripts/check.sh
```

The gate must pass. A pull request with a failing gate is not merged.

## Commit messages

- Use conventional commits. Example: `fix(cli): reject an empty query`.
- Write the message in Simplified Technical English.
- Use short sentences. Name the actor. Use the plain word.
- State what changed and why in the body, if the subject is not enough.

## Pull requests

- Make one change per pull request.
- Add a test for each fix. The test must fail before the fix and pass after it.
- Write each test beside the code it tests.
- Fill in the pull request template.

## Code style

- Do not use `unwrap` or `expect` in library code. Return a `Result`.
- Keep clippy clean: `cargo clippy --all-targets -- -D warnings`.
- Document each public item with one sentence.

## Security problems

Do not open a public issue for a security problem. Read
[SECURITY.md](SECURITY.md) and report the problem in private.

## License

Sieve uses the MIT OR Apache-2.0 license. Your contribution uses the same license.
