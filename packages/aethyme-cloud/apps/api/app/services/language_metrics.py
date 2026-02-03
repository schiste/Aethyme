"""Language-specific code metrics and complexity analysis."""

from typing import Dict, Any
from .language_detector import Language


class LanguageMetrics:
    """Calculate language-specific metrics and complexity."""

    # Language complexity weights (higher = more complex language)
    LANGUAGE_COMPLEXITY_WEIGHTS = {
        Language.C: 1.2,  # Manual memory management
        Language.CPP: 1.3,  # C + OOP + templates
        Language.RUST: 1.1,  # Ownership model, but safe
        Language.JAVA: 1.0,  # Baseline OOP language
        Language.CSHARP: 1.0,  # Similar to Java
        Language.GO: 0.8,  # Simpler language design
        Language.PYTHON: 0.7,  # Dynamic, concise
        Language.RUBY: 0.7,  # Dynamic, concise
        Language.JAVASCRIPT: 0.8,  # Dynamic, event-driven
        Language.TYPESCRIPT: 0.9,  # JavaScript + types
        Language.SWIFT: 0.9,  # Modern, safe
        Language.KOTLIN: 0.9,  # Modern JVM language
    }

    # Decision keywords for cyclomatic complexity by language
    DECISION_KEYWORDS = {
        Language.PYTHON: ['if', 'elif', 'else', 'for', 'while', 'and', 'or', 'try', 'except', 'with'],
        Language.JAVA: ['if', 'else', 'for', 'while', 'do', 'switch', 'case', 'catch', '&&', '||', '?'],
        Language.JAVASCRIPT: ['if', 'else', 'for', 'while', 'do', 'switch', 'case', 'catch', '&&', '||', '?'],
        Language.TYPESCRIPT: ['if', 'else', 'for', 'while', 'do', 'switch', 'case', 'catch', '&&', '||', '?'],
        Language.GO: ['if', 'else', 'for', 'switch', 'case', 'select', '&&', '||'],
        Language.RUBY: ['if', 'elsif', 'else', 'unless', 'for', 'while', 'until', 'case', 'when', 'rescue', '&&', '||'],
        Language.RUST: ['if', 'else', 'for', 'while', 'loop', 'match', 'if let', 'while let', '&&', '||', '?'],
        Language.CSHARP: ['if', 'else', 'for', 'foreach', 'while', 'do', 'switch', 'case', 'catch', '&&', '||', '?'],
        Language.CPP: ['if', 'else', 'for', 'while', 'do', 'switch', 'case', 'catch', '&&', '||', '?'],
        Language.C: ['if', 'else', 'for', 'while', 'do', 'switch', 'case', '&&', '||', '?'],
    }

    # Typical lines of code per function by language
    TYPICAL_FUNCTION_LOC = {
        Language.PYTHON: 15,
        Language.RUBY: 15,
        Language.GO: 20,
        Language.JAVASCRIPT: 18,
        Language.TYPESCRIPT: 18,
        Language.JAVA: 25,
        Language.CSHARP: 25,
        Language.RUST: 22,
        Language.CPP: 30,
        Language.C: 28,
    }

    @classmethod
    def calculate_cyclomatic_complexity(
        cls, code: str, language: Language
    ) -> int:
        """Calculate cyclomatic complexity for a code block.

        Args:
            code: Source code snippet
            language: Programming language

        Returns:
            Cyclomatic complexity score
        """
        keywords = cls.DECISION_KEYWORDS.get(language, ['if', 'for', 'while'])

        complexity = 1  # Base complexity

        for keyword in keywords:
            # Use word boundary matching to avoid false positives
            import re
            pattern = r'\b' + re.escape(keyword) + r'\b'
            matches = re.findall(pattern, code)
            complexity += len(matches)

        # Apply language complexity weight
        weight = cls.LANGUAGE_COMPLEXITY_WEIGHTS.get(language, 1.0)
        adjusted_complexity = int(complexity * weight)

        return max(1, adjusted_complexity)  # Min complexity is 1

    @classmethod
    def calculate_cognitive_complexity(
        cls, code: str, language: Language
    ) -> int:
        """Calculate cognitive complexity (how hard to understand).

        Cognitive complexity factors:
        - Nesting level increases complexity
        - Logical operators increase complexity
        - Recursion increases complexity

        Args:
            code: Source code snippet
            language: Programming language

        Returns:
            Cognitive complexity score
        """
        import re

        complexity = 0
        lines = code.split('\n')

        # Estimate nesting level based on indentation
        max_indent = 0
        for line in lines:
            stripped = line.lstrip()
            if stripped:
                indent = len(line) - len(stripped)
                max_indent = max(max_indent, indent // 4)  # Assume 4-space indent

        # Nesting adds quadratic complexity
        complexity += max_indent ** 2

        # Logical operators
        complexity += len(re.findall(r'&&|\|\||and\s+|or\s+', code))

        # Recursive calls (simplified detection)
        # This would need function name extraction in production
        complexity += code.count('self.') // 2  # Rough heuristic

        return max(1, complexity)

    @classmethod
    def estimate_maintainability_index(
        cls,
        cyclomatic_complexity: int,
        lines_of_code: int,
        language: Language
    ) -> float:
        """Calculate maintainability index (0-100).

        Higher is better. Formula based on:
        MI = 171 - 5.2 * ln(HV) - 0.23 * CC - 16.2 * ln(LOC)

        Simplified version:
        MI = 100 - (CC_factor + LOC_factor)

        Args:
            cyclomatic_complexity: Cyclomatic complexity score
            lines_of_code: Number of lines of code
            language: Programming language

        Returns:
            Maintainability index (0-100)
        """
        import math

        # Normalize complexity and LOC
        cc_factor = min(30, cyclomatic_complexity * 1.5)
        loc_factor = min(40, math.log(max(1, lines_of_code)) * 3)

        # Calculate base MI
        mi = 100 - cc_factor - loc_factor

        # Language adjustment (easier languages are more maintainable)
        weight = cls.LANGUAGE_COMPLEXITY_WEIGHTS.get(language, 1.0)
        mi = mi / weight * 0.9  # Slight adjustment

        return max(0, min(100, mi))

    @classmethod
    def calculate_test_coverage_target(cls, language: Language) -> float:
        """Get recommended test coverage target for a language.

        Args:
            language: Programming language

        Returns:
            Recommended coverage percentage (0-100)
        """
        # Statically typed languages can rely more on type system
        static_languages = {
            Language.JAVA: 75,
            Language.CSHARP: 75,
            Language.GO: 70,
            Language.RUST: 70,
            Language.TYPESCRIPT: 75,
            Language.CPP: 80,  # More critical due to memory issues
            Language.C: 80,
        }

        # Dynamic languages need more tests
        dynamic_languages = {
            Language.PYTHON: 80,
            Language.RUBY: 80,
            Language.JAVASCRIPT: 80,
        }

        return static_languages.get(language) or dynamic_languages.get(language) or 75

    @classmethod
    def get_language_statistics(cls, language: Language) -> Dict[str, Any]:
        """Get statistics and recommendations for a language.

        Args:
            language: Programming language

        Returns:
            Dictionary with language statistics
        """
        return {
            "complexity_weight": cls.LANGUAGE_COMPLEXITY_WEIGHTS.get(language, 1.0),
            "typical_function_loc": cls.TYPICAL_FUNCTION_LOC.get(language, 20),
            "test_coverage_target": cls.calculate_test_coverage_target(language),
            "decision_keywords": cls.DECISION_KEYWORDS.get(language, []),
        }
