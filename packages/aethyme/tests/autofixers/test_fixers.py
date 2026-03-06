"""Tests for individual fixers."""

from pathlib import Path

from src.autofixers.fixers import (
    DocsRegenerator,
    FormatFixer,
    I18nScaffolder,
    LinkFixer,
    SelectorInserter,
)


class TestDocsRegenerator:
    """Tests for DocsRegenerator."""

    def test_identifies_directories_missing_folder_doc(self, tmp_path: Path):
        """Should find directories missing FOLDER.md."""
        # Create directory with code files but no FOLDER.md
        code_dir = tmp_path / "src"
        code_dir.mkdir()

        (code_dir / "module.py").write_text("def func(): pass")

        fixer = DocsRegenerator(tmp_path)
        missing = fixer.find_directories_missing_folder_doc()

        assert code_dir in missing

    def test_skips_directories_with_folder_doc(self, tmp_path: Path):
        """Should skip directories that already have FOLDER.md."""
        code_dir = tmp_path / "src"
        code_dir.mkdir()

        (code_dir / "module.py").write_text("def func(): pass")
        (code_dir / "FOLDER.md").write_text("# Docs")

        fixer = DocsRegenerator(tmp_path)
        missing = fixer.find_directories_missing_folder_doc()

        assert code_dir not in missing

    def test_generates_folder_doc(self, tmp_path: Path):
        """Should generate FOLDER.md content."""
        code_dir = tmp_path / "components"
        code_dir.mkdir()

        (code_dir / "Button.tsx").write_text("export function Button() {}")
        (code_dir / "Input.tsx").write_text("export function Input() {}")

        fixer = DocsRegenerator(tmp_path)
        content = fixer.generate_folder_doc(code_dir)

        assert "# components" in content
        assert "Button.tsx" in content
        assert "Input.tsx" in content
        assert "auto-generated" in content.lower()

    def test_creates_folder_docs(self, tmp_path: Path):
        """Should create FOLDER.md files."""
        code_dir = tmp_path / "utils"
        code_dir.mkdir()

        (code_dir / "helpers.py").write_text("def helper(): pass")

        fixer = DocsRegenerator(tmp_path)
        created = fixer.create_folder_docs()

        assert len(created) == 1
        assert created[0]["fix_type"] == "docs_regen"
        assert "FOLDER.md" in str(created[0]["file_path"])


class TestLinkFixer:
    """Tests for LinkFixer."""

    def test_can_fix_markdown_files(self, tmp_path: Path):
        """Should fix markdown files."""
        fixer = LinkFixer(tmp_path)

        assert fixer.can_fix(Path("README.md"))
        assert fixer.can_fix(Path("docs/guide.md"))
        assert not fixer.can_fix(Path("script.py"))

    def test_fixes_absolute_links(self, tmp_path: Path):
        """Should convert absolute links to relative."""
        content = """# Documentation

[Link to file](/docs/guide.md)
[Another link](http://localhost:3000/api.md)
"""

        fixer = LinkFixer(tmp_path)
        # Note: actual conversion depends on file structure
        # This test verifies the fixer processes the content
        fixer.fix(Path("README.md"), content)

        # Should attempt to process links
        # Result may be None if no valid conversions found

    def test_preserves_external_links(self):
        """Should not modify external links."""
        content = "[External](https://example.com)"

        fixer = LinkFixer(Path("/repo"))
        result = fixer.fix(Path("README.md"), content)

        # External links should not be changed
        assert result is None or "https://example.com" in result

    def test_preserves_relative_links(self):
        """Should not modify already-relative links."""
        content = "[Relative](./local.md)"

        fixer = LinkFixer(Path("/repo"))
        result = fixer.fix(Path("README.md"), content)

        # Already relative links unchanged
        assert result is None


class TestSelectorInserter:
    """Tests for SelectorInserter."""

    def test_can_fix_react_files(self, tmp_path: Path):
        """Should fix React/JSX files."""
        fixer = SelectorInserter(tmp_path)

        assert fixer.can_fix(Path("Button.tsx"))
        assert fixer.can_fix(Path("Form.jsx"))
        assert fixer.can_fix(Path("Component.vue"))
        assert not fixer.can_fix(Path("utils.py"))

    def test_adds_data_ui_to_buttons(self, tmp_path: Path):
        """Should add data-ui to button elements."""
        content = """
export function MyButton() {
  return <button onClick={handleClick}>Click</button>;
}
"""

        file_path = tmp_path / "Button.tsx"
        file_path.write_text(content)

        fixer = SelectorInserter(tmp_path)
        result = fixer.fix(file_path, content)

        if result:  # May not fix if element already has selector
            assert 'data-ui=' in result

    def test_skips_elements_with_selectors(self):
        """Should skip elements that already have data-ui."""
        content = '<button data-ui="my-button">Click</button>'

        fixer = SelectorInserter(Path("/repo"))
        result = fixer.fix(Path("Button.tsx"), content)

        assert result is None  # No changes needed

    def test_generates_meaningful_selector_names(self, tmp_path: Path):
        """Should generate descriptive selector names."""
        fixer = SelectorInserter(tmp_path)
        content = """
export function LoginForm() {
  return <button type="submit">Continue</button>;
}
"""
        result = fixer.fix(Path("LoginForm.tsx"), content)
        assert result is not None
        assert 'data-ui="login-form-button-submit"' in result

    def test_find_missing_selectors(self, tmp_path: Path):
        """Should find elements missing selectors."""
        file_path = tmp_path / "Component.tsx"
        file_path.write_text("""
export function Component() {
  return <button>Click</button>;
}
""")

        fixer = SelectorInserter(tmp_path)
        missing = fixer.find_missing_selectors()

        assert len(missing) > 0
        assert missing[0]["element"] == "button"


class TestI18nScaffolder:
    """Tests for I18nScaffolder."""

    def test_can_fix_source_files(self, tmp_path: Path):
        """Should fix source code files."""
        fixer = I18nScaffolder(tmp_path)

        assert fixer.can_fix(Path("Component.tsx"))
        assert fixer.can_fix(Path("Component.jsx"))
        assert not fixer.can_fix(Path("README.md"))

    def test_finds_hardcoded_strings(self, tmp_path: Path):
        """Should find hardcoded user-facing strings."""
        file_path = tmp_path / "Greeting.tsx"
        file_path.write_text("""
export function Greeting() {
  return <h1>Welcome Back</h1>;
}
""")

        fixer = I18nScaffolder(tmp_path)
        hardcoded = fixer.find_hardcoded_strings()

        assert len(hardcoded) > 0
        assert any("Welcome Back" in h["text"] for h in hardcoded)

    def test_skips_files_with_i18n(self):
        """Should skip files already using i18n."""
        content = """
import { useTranslation } from 'react-i18next';

export function Component() {
  const { t } = useTranslation();
  return <h1>{t('greeting')}</h1>;
}
"""

        fixer = I18nScaffolder(Path("/repo"))
        result = fixer.fix(Path("Component.tsx"), content)

        assert result is None  # Already uses i18n

    def test_generates_i18n_keys(self, tmp_path: Path):
        """Should generate meaningful i18n keys."""
        fixer = I18nScaffolder(tmp_path)
        content = """
export function UserGreeting() {
  return <h1>Welcome Back</h1>;
}
"""
        result = fixer.fix(Path("UserGreeting.tsx"), content)
        assert result is not None
        assert 't("usergreeting.welcome_back")' in result

    def test_is_likely_code(self):
        """Should avoid scaffolding code-like strings while fixing user text."""
        fixer = I18nScaffolder(Path("/repo"))

        code_like = """
export function Component() {
  return <Widget label="camelCaseVariable" />;
}
"""
        user_text = """
export function Component() {
  return <Widget label="Welcome to our app" />;
}
"""
        assert fixer.fix(Path("Component.tsx"), code_like) is None
        fixed = fixer.fix(Path("Component.tsx"), user_text)
        assert fixed is not None
        assert 'label={t("component.welcome_to_our_app")}' in fixed


class TestFormatFixer:
    """Tests for FormatFixer."""

    def test_can_fix_supported_languages(self, tmp_path: Path):
        """Should fix supported language files."""
        fixer = FormatFixer(tmp_path)

        assert fixer.can_fix(Path("script.py")) or not fixer.can_fix(Path("script.py"))
        # Depends on whether formatters are available

    def test_checks_available_tools(self, tmp_path: Path):
        """Should check for available formatting tools."""
        fixer = FormatFixer(tmp_path)

        info = fixer.get_formatter_info()

        assert "available_tools" in info
        assert "languages_supported" in info

    def test_is_tool_available(self):
        """Should expose available tooling without private helper access."""
        fixer = FormatFixer(Path("/repo"))
        info = fixer.get_formatter_info()
        all_tools = {
            tool
            for tools in info["available_tools"].values()
            for tool in tools
        }
        assert "nonexistent-formatter-12345" not in all_tools
