# Contributing to Aethyme

Thank you for your interest in contributing to Aethyme! This guide will help you get started.

---

## Code of Conduct

We are committed to providing a welcoming and inclusive environment. Please be respectful and professional in all interactions.

---

## Getting Started

### Prerequisites

- Python 3.11+
- PostgreSQL 15+
- Redis 6+
- Git
- Basic knowledge of FastAPI and SQLAlchemy

### Development Setup

See our [Onboarding Guide](docs/getting-started/onboarding.md) for detailed setup instructions.

Quick start:

```bash
# Clone and setup
git clone https://github.com/aeptus/aethyme.git
cd aethyme/packages/aethyme
python3 -m venv venv
source venv/bin/activate
pip install -e ".[dev]"

# Start services
bash scripts/start-api.sh

# Run tests
pytest tests/
```

---

## How to Contribute

### Reporting Bugs

Before creating a bug report:
1. Check existing [GitHub Issues](https://github.com/aeptus/aethyme/issues)
2. Verify the bug in the latest version

Create a detailed bug report including:
- **Description**: Clear description of the problem
- **Steps to Reproduce**: Exact steps to trigger the bug
- **Expected Behavior**: What should happen
- **Actual Behavior**: What actually happens
- **Environment**: OS, Python version, Aethyme version
- **Logs**: Relevant error messages or logs

### Suggesting Features

Feature requests are welcome! Please:
1. Check if the feature already exists or is planned (see [ROADMAP](ROADMAP.md))
2. Open a GitHub Discussion to discuss the idea
3. If approved, create a feature request issue

### Pull Requests

#### Before You Start

1. Open an issue to discuss significant changes
2. Fork the repository
3. Create a feature branch from `main`

```bash
git checkout -b feature/your-feature-name
```

#### Development Workflow

1. **Make Changes**
   - Follow our [Code Style](#code-style) guidelines
   - Write tests for new functionality
   - Update documentation as needed

2. **Test Locally**
   ```bash
   # Run tests
   pytest tests/

   # Format code
   black src/ tests/

   # Lint
   ruff check src/ tests/

   # Type check
   mypy src/
   ```

3. **Commit Changes**
   ```bash
   git add .
   git commit -m "feat: add new feature"
   ```

   Follow [Conventional Commits](https://www.conventionalcommits.org/):
   - `feat:` New feature
   - `fix:` Bug fix
   - `docs:` Documentation only
   - `style:` Formatting, missing semi colons, etc.
   - `refactor:` Code change that neither fixes a bug nor adds a feature
   - `perf:` Performance improvement
   - `test:` Adding tests
   - `chore:` Maintenance tasks

4. **Push and Create PR**
   ```bash
   git push origin feature/your-feature-name
   ```

   Then open a Pull Request on GitHub.

#### Pull Request Guidelines

**PR Title**: Use conventional commit format
```
feat: add search autocomplete
fix: resolve indexing timeout issue
docs: update API reference
```

**PR Description**: Include:
- Summary of changes
- Related issue(s)
- Testing performed
- Screenshots (if UI changes)
- Breaking changes (if any)

**PR Checklist**:
- [ ] Tests pass locally
- [ ] New tests added for new functionality
- [ ] Documentation updated
- [ ] Code follows style guidelines
- [ ] Commit messages follow conventional commits
- [ ] No merge conflicts

---

## Code Style

### Python

We follow **PEP 8** with these specifications:

- **Line Length**: 88 characters (Black default)
- **Formatter**: Black
- **Linter**: Ruff
- **Type Hints**: Required for public APIs

Example:

```python
from typing import List, Optional

async def search_symbols(
    query: str,
    *,
    limit: int = 20,
    filters: Optional[dict] = None
) -> List[SearchResult]:
    """
    Search for code symbols.

    Args:
        query: Search query string
        limit: Maximum results to return
        filters: Optional filters (kind, language, etc.)

    Returns:
        List of search results ordered by relevance

    Raises:
        ValueError: If query is empty
    """
    if not query:
        raise ValueError("Query cannot be empty")

    # Implementation
    results = await store.search(query, limit=limit)
    return results
```

### Documentation

- **Docstrings**: Google style
- **Comments**: Explain *why*, not *what*
- **README**: Update if behavior changes
- **API Docs**: Update OpenAPI schema if endpoints change

---

## Testing

### Test Requirements

- **Unit Tests**: Test individual functions
  ```python
  def test_parse_symbol():
      result = parse_symbol("file.py:ClassName.method")
      assert result.file == "file.py"
      assert result.class_name == "ClassName"
      assert result.method == "method"
  ```

- **Integration Tests**: Test API endpoints
  ```python
  @pytest.mark.asyncio
  async def test_search_endpoint(client):
      response = await client.post("/api/search/",
          json={"query": "GraphStore"}
      )
      assert response.status_code == 200
      assert len(response.json()["results"]) > 0
  ```

- **Coverage**: Aim for > 80% code coverage

### Running Tests

```bash
# All tests
pytest tests/

# Specific test file
pytest tests/test_graph_store.py

# Specific test
pytest tests/test_graph_store.py::test_search

# With coverage
pytest tests/ --cov=src --cov-report=html

# Fast (skip slow tests)
pytest tests/ -m "not slow"
```

---

## Documentation

### When to Update Docs

- Adding/changing API endpoints → Update `docs/reference/api.md`
- Adding/changing CLI commands → Update `docs/reference/cli.md`
- Changing behavior → Update relevant guide
- Adding features → Update `ROADMAP.md` and `CHANGELOG.md`

### Documentation Style

- Use **Markdown** for all docs
- Include **code examples** that work
- Keep docs **up-to-date** with code changes
- Use **relative links** between docs

---

## Review Process

### What Reviewers Look For

1. **Functionality**: Does it work as intended?
2. **Tests**: Are there adequate tests?
3. **Code Quality**: Is it readable and maintainable?
4. **Documentation**: Are docs updated?
5. **Performance**: No significant performance regressions?
6. **Security**: No security vulnerabilities?

### Addressing Feedback

- Respond to all review comments
- Make requested changes or explain why not
- Re-request review when ready
- Be patient and respectful

---

## Community

### Communication Channels

- **GitHub Issues**: Bug reports and feature requests
- **GitHub Discussions**: Questions and ideas
- **Slack** (internal): #aethyme-dev

### Getting Help

- Review [documentation](docs/)
- Check [troubleshooting guide](docs/guides/troubleshooting.md)
- Ask in GitHub Discussions
- Reach out in Slack (internal contributors)

---

## Recognition

Contributors are recognized in:
- `CONTRIBUTORS.md` file
- Release notes
- Project README

---

## License

By contributing, you agree that your contributions will be licensed under the same license as the project (see `LICENSE` file).

---

**Thank you for contributing to Aethyme!**

For questions about contributing: dev@aethyme.com
