# Pull Request

## Description

Please include a summary of the changes and the related issue. Please also include relevant motivation and context. List any dependencies that are required for this change.

Fixes # (issue)

## Type of change

Please delete options that are not relevant.

- [ ] Bug fix (non-breaking change which fixes an issue)
- [ ] New feature (non-breaking change which adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] This change requires a documentation update
- [ ] Performance improvement
- [ ] Code refactoring
- [ ] Test coverage improvement
- [ ] Documentation update

## Changes Made

### Core Changes
- [ ] Modified MCP protocol implementation
- [ ] Added/modified tools
- [ ] Added/modified resources
- [ ] Added/modified prompts
- [ ] Updated server capabilities

### AI Integration Changes
- [ ] DashScope/Qwen integration changes
- [ ] NVIDIA NIM integration changes
- [ ] Gemini integration changes
- [ ] New AI provider support
- [ ] Prompt engineering improvements

### Infrastructure Changes
- [ ] Build system changes
- [ ] CI/CD pipeline changes
- [ ] Dependency updates
- [ ] Configuration changes
- [ ] Performance optimizations

## Testing

### Test Coverage
- [ ] Unit tests added/updated
- [ ] Integration tests added/updated
- [ ] Performance tests added/updated
- [ ] Manual testing completed

### Test Results
- [ ] All existing tests pass
- [ ] New tests pass
- [ ] No performance regressions
- [ ] Memory usage is acceptable

### Testing Commands
Please list the commands used to test your changes:

```bash
# Example:
cargo test
cargo clippy
cargo fmt --check
cargo bench
```

## Checklist

### Code Quality
- [ ] My code follows the style guidelines of this project
- [ ] I have performed a self-review of my own code
- [ ] I have commented my code, particularly in hard-to-understand areas
- [ ] I have made corresponding changes to the documentation
- [ ] My changes generate no new warnings
- [ ] I have added tests that prove my fix is effective or that my feature works
- [ ] New and existing unit tests pass locally with my changes
- [ ] Any dependent changes have been merged and published in downstream modules

### Documentation
- [ ] Updated README.md if needed
- [ ] Updated API documentation
- [ ] Added/updated code comments
- [ ] Updated configuration examples
- [ ] Added migration guide (for breaking changes)

### MCP Protocol Compliance
- [ ] Changes follow MCP protocol specifications
- [ ] Server capabilities updated correctly
- [ ] Tool schemas are valid
- [ ] Resource URIs follow conventions
- [ ] Prompt definitions are complete

## Performance Impact

### Benchmarks
If applicable, include benchmark results:

```
# Before:
[benchmark results]

# After:
[benchmark results]
```

### Memory Usage
- [ ] No significant memory usage increase
- [ ] Memory leaks checked and resolved
- [ ] Resource cleanup implemented

### Network Usage
- [ ] API calls are efficient
- [ ] Proper error handling for network failures
- [ ] Rate limiting considerations addressed

## Breaking Changes

If this PR introduces breaking changes, please describe:

1. What breaks:
2. Why it was necessary:
3. Migration path for users:
4. Version compatibility:

## Screenshots/Examples

If applicable, add screenshots or code examples to help explain your changes.

## Additional Notes

Add any other notes about the PR here, including:
- Known limitations
- Future improvements planned
- Alternative approaches considered
- Dependencies on other PRs/issues

## Reviewer Notes

Specific areas where you'd like reviewer focus:
- [ ] Architecture decisions
- [ ] Performance implications
- [ ] Security considerations
- [ ] Error handling
- [ ] Test coverage
- [ ] Documentation clarity

## Related Issues/PRs

- Closes #
- Related to #
- Depends on #
- Blocks #