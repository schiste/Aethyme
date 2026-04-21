"""Unit tests for candidate_scrubber.

Run: pytest packages/aethyme-eval-ui/server/test_candidate_scrubber.py -v
Or: python -m pytest server/test_candidate_scrubber.py -v (from server/)
"""

from __future__ import annotations

from candidate_scrubber import scrub_candidate


def test_empty_candidate_returns_empty() -> None:
    out, stats = scrub_candidate("")
    assert out == ""
    assert stats.original_chars == 0
    assert stats.removed_chars == 0


def test_no_envelope_content_unchanged() -> None:
    """Plain technical prose with no envelope passes through untouched."""
    text = (
        "The root cause is in AuthManager.php line 42.\n"
        "The session token is not hashed before storage."
    )
    out, stats = scrub_candidate(text)
    assert out == text
    assert stats.lines_dropped == 0
    assert stats.lines_prefix_stripped == 0


def test_model_prefix_stripped_content_kept() -> None:
    out, _ = scrub_candidate("Claude: I found the bug in AuthManager.php")
    # Content is kept, the identity prefix is removed.
    assert "AuthManager.php" in out
    assert "Claude:" not in out


def test_thinking_block_dropped() -> None:
    out, stats = scrub_candidate("Thinking…\nThe bug is here.")
    assert "Thinking" not in out
    assert "The bug is here." in out
    assert stats.lines_dropped == 1


def test_claude_bullet_stripped() -> None:
    out, _ = scrub_candidate("● Running tool: Read\n⏺ Bash")
    assert "Running tool: Read" in out
    assert "●" not in out and "⏺" not in out


def test_repl_prompt_stripped() -> None:
    out, _ = scrub_candidate("$ grep -r 'foo' .\n> ls -la")
    assert "grep -r 'foo' ." in out
    assert "ls -la" in out
    # The prompt glyphs should be gone from line starts.
    for line in out.splitlines():
        assert not line.startswith("$")
        assert not line.startswith(">")


def test_provider_boilerplate_dropped() -> None:
    text = (
        "I am an AI assistant built by OpenAI.\n"
        "As an AI language model, I conclude X.\n"
        "The actual finding is here."
    )
    out, stats = scrub_candidate(text)
    assert "OpenAI" not in out
    assert "AI language model" not in out
    assert "The actual finding is here." in out
    assert stats.lines_dropped == 2


def test_codex_mentioned_in_content_not_scrubbed() -> None:
    """Mid-line 'codex' reference is content, not envelope — should be kept."""
    text = "The codex binary failed to start. grep returned no matches."
    out, _ = scrub_candidate(text)
    assert out == text  # No envelope pattern matches mid-line.


def test_file_paths_preserved() -> None:
    """Analysis text with paths, line numbers, code references is untouched."""
    text = (
        "The issue is in includes/auth/AuthManager.php:42.\n"
        "Relevant function: validateSessionToken().\n"
        "Fix: hash the token before storage."
    )
    out, stats = scrub_candidate(text)
    assert out == text
    assert stats.lines_dropped == 0


def test_mixed_realistic_transcript() -> None:
    """End-to-end: realistic transcript with mixed envelope + content."""
    text = (
        "Claude: let me investigate.\n"
        "● Running tool: Read\n"
        "Thinking…\n"
        "The root cause is in AuthManager.php line 42.\n"
        "$ grep -r 'session_token' .\n"
        "⏺ Bash\n"
        "I am an AI assistant.\n"
        "The fix requires hashing the token."
    )
    out, stats = scrub_candidate(text)
    # Envelope removed.
    assert "Claude:" not in out
    assert "Thinking" not in out
    assert "I am an AI" not in out
    # Content preserved.
    assert "AuthManager.php" in out
    assert "session_token" in out
    assert "hashing the token" in out
    # Stats sanity.
    assert stats.lines_dropped >= 2
    assert stats.lines_prefix_stripped >= 3
    assert stats.removed_chars > 0


if __name__ == "__main__":
    # Allow `python test_candidate_scrubber.py` without pytest.
    tests = [v for k, v in list(globals().items()) if k.startswith("test_")]
    failed = 0
    for t in tests:
        try:
            t()
            print(f"PASS {t.__name__}")
        except AssertionError as e:
            failed += 1
            print(f"FAIL {t.__name__}: {e}")
    raise SystemExit(1 if failed else 0)
