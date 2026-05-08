# Pull Request

## Summary

Describe what changed and why.

Closes #

## Type of Change

- [ ] Bug fix
- [ ] Feature
- [ ] Refactor
- [ ] Documentation
- [ ] Tests
- [ ] Dependency or tooling update
- [ ] Breaking change

## MCP / Runtime Impact

- [ ] Tool schema changed
- [ ] Tool behavior changed
- [ ] Shell execution behavior changed
- [ ] File read/write behavior changed
- [ ] Persistence or thread-id behavior changed
- [ ] No MCP/runtime behavior change

## Security Impact

- [ ] Filesystem access affected
- [ ] Shell command execution affected
- [ ] Mode restrictions affected
- [ ] State persistence affected
- [ ] No security-sensitive behavior changed

Notes:

## Testing

Commands run:

```bash
cargo fmt --all -- --check
cargo check --tests
cargo clippy --all-targets --all-features
cargo test --all-features
```

## Checklist

- [ ] I kept the change focused.
- [ ] I added or updated tests for behavior changes.
- [ ] I updated docs/templates when user-facing behavior changed.
- [ ] I verified failure paths where applicable.
- [ ] I did not weaken command, file, workspace, or thread-id safety.

## Additional Notes

Add migration notes, known limitations, or reviewer focus areas here.
