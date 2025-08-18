# Security Policy

## Supported Versions

We actively support the following versions of Winx Code Agent with security updates:

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1.0 | :x:                |

## Security Considerations

### API Key Security

Winx Code Agent integrates with multiple AI providers and requires API keys for:
- DashScope (Alibaba Cloud)
- NVIDIA NIM
- Google Gemini

**Important Security Practices:**

1. **Never commit API keys to version control**
2. **Use environment variables or secure configuration files**
3. **Rotate API keys regularly**
4. **Monitor API usage for unusual activity**
5. **Use least-privilege access when possible**

### MCP Protocol Security

As an MCP (Model Context Protocol) server, Winx Code Agent:

- **File System Access**: Has read/write access to your file system through MCP tools
- **Command Execution**: Can execute shell commands via the `bash_command` tool
- **Network Access**: Makes API calls to AI providers
- **Data Processing**: Processes and analyzes your code and files

**Security Recommendations:**

1. **Run in isolated environments** when possible
2. **Review file permissions** in your working directory
3. **Monitor command execution** and file access logs
4. **Use firewall rules** to restrict network access if needed
5. **Regularly update** to the latest version

### Data Privacy

**Local Processing:**
- Code analysis and file operations are performed locally
- No code is sent to external services unless using AI features

**AI Provider Integration:**
- When using AI features, code snippets may be sent to:
  - DashScope (Alibaba Cloud)
  - NVIDIA NIM
  - Google Gemini
- Review each provider's privacy policy and terms of service
- Consider using local AI models for sensitive code

**Logging:**
- Server logs may contain file paths and command information
- Logs are stored locally in your system's log directory
- API keys and sensitive data are not logged

## Reporting a Vulnerability

We take security vulnerabilities seriously. If you discover a security vulnerability, please follow these steps:

### 1. Do Not Create Public Issues

**Please do not report security vulnerabilities through public GitHub issues, discussions, or pull requests.**

### 2. Contact Us Privately

Send vulnerability reports to: **[security@winx-code-agent.dev]** (replace with actual email)

Alternatively, you can:
- Use GitHub's private vulnerability reporting feature
- Contact the maintainers directly through GitHub

### 3. Include Detailed Information

Please include as much information as possible:

- **Vulnerability Description**: Clear description of the security issue
- **Impact Assessment**: Potential impact and severity
- **Reproduction Steps**: Step-by-step instructions to reproduce
- **Affected Versions**: Which versions are affected
- **Proof of Concept**: Code or screenshots demonstrating the issue
- **Suggested Fix**: If you have ideas for remediation
- **Your Contact Information**: For follow-up questions

### 4. Response Timeline

We aim to respond to security reports within:

- **Initial Response**: 48 hours
- **Vulnerability Assessment**: 7 days
- **Fix Development**: 30 days (depending on complexity)
- **Public Disclosure**: After fix is released and users have time to update

### 5. Coordinated Disclosure

We follow responsible disclosure practices:

1. **Private Investigation**: We investigate and develop fixes privately
2. **User Notification**: We notify users of security updates
3. **Public Disclosure**: We publish security advisories after fixes are available
4. **Credit**: We acknowledge security researchers (with permission)

## Security Best Practices for Users

### Installation Security

```bash
# Verify checksums when downloading releases
shasum -a 256 winx-code-agent-v0.1.5.tar.gz

# Use official installation methods
cargo install winx-code-agent

# Or build from source
git clone https://github.com/your-org/winx-code-agent.git
cd winx-code-agent
cargo build --release
```

### Configuration Security

```bash
# Use environment variables for API keys
export DASHSCOPE_API_KEY="your-key-here"
export NVIDIA_API_KEY="your-key-here"
export GEMINI_API_KEY="your-key-here"

# Set appropriate file permissions
chmod 600 ~/.config/winx-code-agent/config.toml

# Use secure directories
mkdir -p ~/.config/winx-code-agent
```

### Runtime Security

```bash
# Run with limited permissions when possible
# Monitor file system access
# Review command execution logs
# Use network monitoring tools
```

### MCP Client Security

When using with MCP clients (like Claude Desktop):

1. **Review MCP client security settings**
2. **Understand what data is shared**
3. **Monitor server logs**
4. **Use secure communication channels**

## Security Updates

Security updates are released as:

- **Patch Releases**: For critical security fixes
- **Security Advisories**: Published on GitHub Security tab
- **Release Notes**: Include security fix details
- **Changelog**: Documents all security-related changes

### Staying Updated

- **Watch this repository** for security notifications
- **Subscribe to releases** to get notified of updates
- **Follow security advisories** on GitHub
- **Check the changelog** regularly

## Threat Model

### Assets Protected
- User source code and files
- API keys and credentials
- System integrity
- Network communications

### Potential Threats
- Malicious code execution
- API key exposure
- Unauthorized file access
- Network-based attacks
- Supply chain attacks

### Security Controls
- Input validation and sanitization
- Secure API key handling
- File system permission checks
- Network request validation
- Dependency security scanning

## Compliance

Winx Code Agent aims to comply with:

- **OWASP Top 10** security practices
- **Secure coding standards** for Rust
- **API security best practices**
- **Data protection regulations** (where applicable)

## Security Resources

- [Rust Security Guidelines](https://doc.rust-lang.org/book/ch09-00-error-handling.html)
- [MCP Security Considerations](https://modelcontextprotocol.io/docs/security)
- [OWASP Secure Coding Practices](https://owasp.org/www-project-secure-coding-practices-quick-reference-guide/)
- [GitHub Security Features](https://docs.github.com/en/code-security)

---

**Note**: This security policy is subject to updates. Please check back regularly for the latest information.

**Last Updated**: January 2025