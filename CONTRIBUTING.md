# Contributing to Winx Code Agent

Thank you for your interest in contributing to Winx Code Agent! This document provides guidelines and information for contributors.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Contributing Guidelines](#contributing-guidelines)
- [Pull Request Process](#pull-request-process)
- [Issue Reporting](#issue-reporting)
- [Development Workflow](#development-workflow)
- [Testing](#testing)
- [Documentation](#documentation)

## Code of Conduct

This project adheres to a [Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code.

## Getting Started

### Prerequisites

- Rust 1.70+ (latest stable recommended)
- Cargo (comes with Rust)
- Git

### Development Setup

1. Fork the repository on GitHub
2. Clone your fork locally:
   ```bash
   git clone https://github.com/YOUR_USERNAME/winx-code-agent.git
   cd winx-code-agent
   ```

3. Add the upstream repository:
   ```bash
   git remote add upstream https://github.com/original-repo/winx-code-agent.git
   ```

4. Install dependencies and build:
   ```bash
   cargo build
   ```

5. Run tests to ensure everything works:
   ```bash
   cargo test
   ```

## Contributing Guidelines

### Types of Contributions

We welcome various types of contributions:

- **Bug fixes**: Help us identify and fix issues
- **Feature enhancements**: Propose and implement new features
- **Documentation**: Improve existing docs or add new ones
- **Performance improvements**: Optimize code for better performance
- **Test coverage**: Add or improve test cases
- **Code quality**: Refactoring and code cleanup

### Before You Start

1. **Check existing issues**: Look for existing issues or discussions related to your contribution
2. **Create an issue**: For significant changes, create an issue first to discuss the approach
3. **Follow conventions**: Adhere to the project's coding standards and conventions

## Pull Request Process

### 1. Create a Feature Branch

```bash
git checkout -b feature/your-feature-name
# or
git checkout -b fix/issue-description
```

### 2. Make Your Changes

- Write clean, readable code
- Follow Rust best practices and idioms
- Add tests for new functionality
- Update documentation as needed
- Ensure all tests pass

### 3. Commit Your Changes

Use clear, descriptive commit messages:

```bash
git commit -m "feat: add MCP resources support"
# or
git commit -m "fix: resolve memory leak in terminal state"
```

**Commit Message Format:**
- `feat:` for new features
- `fix:` for bug fixes
- `docs:` for documentation changes
- `test:` for test additions/modifications
- `refactor:` for code refactoring
- `perf:` for performance improvements
- `chore:` for maintenance tasks

### 4. Push and Create Pull Request

```bash
git push origin feature/your-feature-name
```

Then create a pull request on GitHub with:
- Clear title and description
- Reference to related issues
- Screenshots/examples if applicable
- Checklist of completed items

### 5. Code Review Process

- Maintainers will review your PR
- Address feedback and requested changes
- Keep your branch updated with main
- Once approved, your PR will be merged

## Issue Reporting

### Bug Reports

When reporting bugs, please include:

- **Environment**: OS, Rust version, dependencies
- **Steps to reproduce**: Clear, step-by-step instructions
- **Expected behavior**: What should happen
- **Actual behavior**: What actually happens
- **Error messages**: Full error output if applicable
- **Additional context**: Screenshots, logs, etc.

### Feature Requests

For feature requests, please provide:

- **Problem description**: What problem does this solve?
- **Proposed solution**: How should it work?
- **Alternatives considered**: Other approaches you've thought about
- **Additional context**: Use cases, examples, etc.

## Development Workflow

### Code Style

- Use `cargo fmt` to format code
- Use `cargo clippy` to catch common mistakes
- Follow Rust naming conventions
- Write self-documenting code with clear variable names
- Add comments for complex logic

### Testing

- Write unit tests for new functions
- Add integration tests for new features
- Ensure all tests pass: `cargo test`
- Aim for good test coverage
- Test edge cases and error conditions

### Performance

- Profile performance-critical code
- Use benchmarks for performance improvements
- Consider memory usage and allocations
- Test with realistic data sizes

## Documentation

### Code Documentation

- Add rustdoc comments for public APIs
- Include examples in documentation
- Document complex algorithms and data structures
- Keep documentation up-to-date with code changes

### User Documentation

- Update README.md for user-facing changes
- Add examples and usage instructions
- Document configuration options
- Include troubleshooting information

## Getting Help

- **GitHub Issues**: For bugs and feature requests
- **GitHub Discussions**: For questions and general discussion
- **Code Review**: Ask questions in PR comments

## Recognition

Contributors are recognized in:
- GitHub contributors list
- Release notes for significant contributions
- Special mentions for outstanding contributions

## License

By contributing to Winx Code Agent, you agree that your contributions will be licensed under the same license as the project.

Thank you for contributing to Winx Code Agent! 🚀