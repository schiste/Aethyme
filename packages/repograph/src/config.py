"""Configuration management for RepoGraph."""

from typing import Optional, List
from pydantic_settings import BaseSettings, SettingsConfigDict
from pydantic import Field, PostgresDsn, RedisDsn


class Settings(BaseSettings):
    """Application settings with environment variable support."""

    model_config = SettingsConfigDict(
        env_file=".env",
        env_file_encoding="utf-8",
        case_sensitive=False,
    )

    # Database
    database_url: PostgresDsn = Field(
        default="postgresql://repograph:password@localhost:5432/repograph",
        description="PostgreSQL connection URL",
    )
    db_pool_min_size: int = Field(default=2, description="Minimum connection pool size")
    db_pool_max_size: int = Field(default=20, description="Maximum connection pool size")
    db_echo: bool = Field(default=False, description="Echo SQL statements")

    # Redis
    redis_url: Optional[RedisDsn] = Field(
        default="redis://localhost:6379/0",
        description="Redis connection URL for caching",
    )
    redis_cache_ttl: int = Field(default=300, description="Cache TTL in seconds")

    # API
    api_host: str = Field(default="0.0.0.0", description="API host")
    api_port: int = Field(default=8001, description="API port")
    api_workers: int = Field(default=4, description="Number of API workers")
    cors_origins: List[str] = Field(
        default=["http://localhost:3000", "http://localhost:5173"],
        description="CORS allowed origins",
    )
    allowed_hosts: List[str] = Field(
        default=["localhost", "127.0.0.1"],
        description="Allowed hosts for the API",
    )

    # Authentication
    jwt_secret_key: str = Field(
        default="your-super-secret-jwt-key-change-this-in-production",
        description="JWT secret key",
    )
    jwt_algorithm: str = Field(default="HS256", description="JWT algorithm")
    jwt_expiration_delta: int = Field(default=86400, description="JWT expiration in seconds")

    # OIDC Configuration (optional)
    oidc_issuer_url: Optional[str] = Field(default=None, description="OIDC issuer URL")
    oidc_client_id: Optional[str] = Field(default=None, description="OIDC client ID")
    oidc_client_secret: Optional[str] = Field(default=None, description="OIDC client secret")
    oidc_redirect_uri: Optional[str] = Field(default=None, description="OIDC redirect URI")

    # Rate Limiting
    rate_limit_default: int = Field(default=100, description="Default rate limit (requests per minute)")
    rate_limit_enabled: bool = Field(default=True, description="Enable rate limiting")

    # Indexing
    indexing_batch_size: int = Field(default=1000, description="Batch size for indexing")
    indexing_timeout: int = Field(default=300, description="Timeout for indexing in seconds")
    indexing_concurrency: int = Field(default=4, description="Number of concurrent indexers")
    watch_enabled: bool = Field(default=True, description="Enable file watching")
    watch_batch_interval: int = Field(
        default=300, description="Batch interval for file watching in seconds"
    )

    # Monitoring
    log_level: str = Field(default="INFO", description="Log level")
    log_format: str = Field(default="json", description="Log format (json or plain)")
    metrics_enabled: bool = Field(default=True, description="Enable Prometheus metrics")
    metrics_port: int = Field(default=9090, description="Metrics port")

    # Cloud Configuration (optional)
    gcp_project_id: Optional[str] = Field(default=None, description="GCP Project ID")
    gcp_region: Optional[str] = Field(default=None, description="GCP Region")
    cloud_sql_instance: Optional[str] = Field(default=None, description="Cloud SQL instance")
    cloud_tasks_queue: Optional[str] = Field(default=None, description="Cloud Tasks queue")

    @property
    def database_url_sync(self) -> str:
        """Get synchronous database URL string."""
        return str(self.database_url)

    @property
    def redis_url_str(self) -> Optional[str]:
        """Get Redis URL as string."""
        return str(self.redis_url) if self.redis_url else None


# Global settings instance
settings = Settings()