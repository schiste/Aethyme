"""
Application Configuration

Loads and validates environment variables using Pydantic Settings.
"""

from typing import List, Optional
from pydantic import Field, PostgresDsn, RedisDsn
from pydantic_settings import BaseSettings, SettingsConfigDict


class Settings(BaseSettings):
    """Application settings loaded from environment variables."""

    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=True,
    )

    # Environment
    ENVIRONMENT: str = Field(default="development")
    DEBUG: bool = Field(default=False)
    LOG_LEVEL: str = Field(default="INFO")

    # API
    API_HOST: str = Field(default="0.0.0.0")
    API_PORT: int = Field(default=8000)
    API_CORS_ORIGINS: str = Field(default="http://localhost:3000")

    @property
    def CORS_ORIGINS(self) -> List[str]:
        """Parse CORS origins from comma-separated string."""
        return [origin.strip() for origin in self.API_CORS_ORIGINS.split(",")]

    # Database
    DATABASE_URL: PostgresDsn
    DB_POOL_MIN_SIZE: int = Field(default=2)
    DB_POOL_MAX_SIZE: int = Field(default=20)
    DB_ECHO: bool = Field(default=False)

    # Redis
    REDIS_URL: RedisDsn

    # Elasticsearch
    ELASTICSEARCH_URL: str = Field(default="http://localhost:9200")

    # JWT Authentication
    JWT_SECRET_KEY: str
    JWT_ALGORITHM: str = Field(default="HS256")
    JWT_EXPIRATION_MINUTES: int = Field(default=1440)  # 24 hours

    REFRESH_TOKEN_SECRET_KEY: str
    REFRESH_TOKEN_EXPIRATION_DAYS: int = Field(default=30)

    # Encryption
    ENCRYPTION_KEY: str  # Fernet encryption key for sensitive data (tokens, etc.)

    # OAuth - GitHub
    GITHUB_CLIENT_ID: Optional[str] = None
    GITHUB_CLIENT_SECRET: Optional[str] = None
    GITHUB_REDIRECT_URI: Optional[str] = None

    # OAuth - GitLab
    GITLAB_CLIENT_ID: Optional[str] = None
    GITLAB_CLIENT_SECRET: Optional[str] = None
    GITLAB_REDIRECT_URI: Optional[str] = None

    # OAuth - Bitbucket
    BITBUCKET_CLIENT_ID: Optional[str] = None
    BITBUCKET_CLIENT_SECRET: Optional[str] = None
    BITBUCKET_REDIRECT_URI: Optional[str] = None

    # Webhooks
    GITHUB_WEBHOOK_SECRET: Optional[str] = None
    GITLAB_WEBHOOK_SECRET: Optional[str] = None
    BITBUCKET_WEBHOOK_SECRET: Optional[str] = None

    # Stripe
    STRIPE_SECRET_KEY: Optional[str] = None
    STRIPE_PUBLISHABLE_KEY: Optional[str] = None
    STRIPE_WEBHOOK_SECRET: Optional[str] = None
    STRIPE_PRICE_PRO_MONTHLY: Optional[str] = None
    STRIPE_PRICE_TEAM_MONTHLY: Optional[str] = None

    # Email
    SENDGRID_API_KEY: Optional[str] = None
    FROM_EMAIL: str = Field(default="noreply@aethyme.com")
    SUPPORT_EMAIL: str = Field(default="support@aethyme.com")

    # Sentry
    SENTRY_DSN: Optional[str] = None
    SENTRY_ENVIRONMENT: str = Field(default="development")
    SENTRY_TRACES_SAMPLE_RATE: float = Field(default=0.1)

    # Google Cloud Storage
    GCS_BUCKET_NAME: Optional[str] = None
    GCS_PROJECT_ID: Optional[str] = None
    GOOGLE_APPLICATION_CREDENTIALS: Optional[str] = None

    # Celery
    CELERY_BROKER_URL: Optional[RedisDsn] = None
    CELERY_RESULT_BACKEND: Optional[RedisDsn] = None
    CELERY_WORKER_CONCURRENCY: int = Field(default=10)

    # Rate Limiting
    RATE_LIMIT_PER_MINUTE: int = Field(default=100)
    RATE_LIMIT_PER_HOUR: int = Field(default=1000)
    RATE_LIMIT_PER_DAY: int = Field(default=10000)

    # Feature Flags
    FEATURE_GITLAB_INTEGRATION: bool = Field(default=True)
    FEATURE_BITBUCKET_INTEGRATION: bool = Field(default=True)
    FEATURE_SSO: bool = Field(default=False)
    FEATURE_ANALYTICS: bool = Field(default=True)


# Global settings instance
settings = Settings()
