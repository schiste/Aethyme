"""Setup configuration for Aethyme Python SDK."""

from pathlib import Path

from setuptools import find_packages, setup

long_description = Path("README.md").read_text(encoding="utf-8")

setup(
    name="aethyme-sdk",
    version="2.0.0",
    author="Aethyme Team",
    author_email="support@aethyme.com",
    description="Official Python SDK for Aethyme API",
    long_description=long_description,
    long_description_content_type="text/markdown",
    url="https://github.com/aethyme/python-sdk",
    packages=find_packages(),
    classifiers=[
        "Development Status :: 4 - Beta",
        "Intended Audience :: Developers",
        "License :: OSI Approved :: MIT License",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 3.9",
        "Programming Language :: Python :: 3.10",
        "Programming Language :: Python :: 3.11",
        "Programming Language :: Python :: 3.12",
    ],
    python_requires=">=3.9",
    install_requires=[
        "httpx>=0.25.0",
        "pydantic>=2.0.0",
    ],
    extras_require={
        "dev": [
            "pytest>=7.4.0",
            "pytest-asyncio>=0.21.0",
            "pytest-cov>=4.1.0",
            "black>=23.0.0",
            "mypy>=1.0.0",
        ],
    },
    project_urls={
        "Bug Reports": "https://github.com/aethyme/python-sdk/issues",
        "Documentation": "https://docs.aethyme.com/sdk/python",
        "Source": "https://github.com/aethyme/python-sdk",
    },
)
