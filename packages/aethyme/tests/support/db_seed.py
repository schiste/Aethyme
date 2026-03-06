"""Canonical database bootstrap and seed data for Aethyme tests."""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Protocol, cast

import psycopg2.extras as psycopg2_extras
from psycopg2.extensions import connection as PGConnection
from psycopg2.extras import Json

from src.models.graph import Edge, Node, NodeKind

PROJECT_ROOT = Path(__file__).resolve().parents[2]
MIGRATION_FILES = [
    PROJECT_ROOT / "migrations" / "001_initial_schema.sql",
    PROJECT_ROOT / "migrations" / "002_add_rls_hardening.sql",
    PROJECT_ROOT / "migrations" / "003_scorecard_tables.sql",
]

PRIMARY_ORG_ID = "10000000-0000-0000-0000-000000000001"
PRIMARY_TENANT_ID = "10000000-0000-0000-0000-000000000101"
PRIMARY_REPOSITORY_ID = "10000000-0000-0000-0000-000000000201"
SECONDARY_ORG_ID = "20000000-0000-0000-0000-000000000001"
SECONDARY_TENANT_ID = "20000000-0000-0000-0000-000000000101"
SECONDARY_REPOSITORY_ID = "20000000-0000-0000-0000-000000000201"
RUNTIME_ROLE = "aethyme_runtime_test"


SQLParams = tuple[object, ...]


class ExecuteBatchFunction(Protocol):
    def __call__(
        self,
        cur: object,
        sql: str,
        argslist: Iterable[SQLParams],
        page_size: int = 100,
    ) -> None: ...


class ExecuteBatchModule(Protocol):
    execute_batch: object


typed_execute_batch = cast(
    ExecuteBatchFunction,
    cast(ExecuteBatchModule, psycopg2_extras).execute_batch,
)


@dataclass(frozen=True)
class GraphSeedContext:
    """Stable IDs exposed to tests."""

    primary_org_id: str = PRIMARY_ORG_ID
    primary_tenant_id: str = PRIMARY_TENANT_ID
    primary_repository_id: str = PRIMARY_REPOSITORY_ID
    secondary_org_id: str = SECONDARY_ORG_ID
    secondary_tenant_id: str = SECONDARY_TENANT_ID
    secondary_repository_id: str = SECONDARY_REPOSITORY_ID


@dataclass(frozen=True)
class SeedNodeSpec:
    alias: str
    symbol: str
    file_path: str
    line_number: int
    column_number: int
    kind: str
    language: str
    signature: str | None = None
    documentation: str | None = None


def rebuild_test_database(conn: PGConnection) -> None:
    """Recreate the Aethyme schema from migrations."""
    with conn.cursor() as cur:
        cur.execute("DROP SCHEMA IF EXISTS aethyme CASCADE")
    conn.commit()

    for migration_path in MIGRATION_FILES:
        with conn.cursor() as cur:
            cur.execute(migration_path.read_text())
        conn.commit()

    with conn.cursor() as cur:
        cur.execute(f"DROP ROLE IF EXISTS {RUNTIME_ROLE}")
        cur.execute(f"CREATE ROLE {RUNTIME_ROLE} NOLOGIN")
        cur.execute(f"GRANT {RUNTIME_ROLE} TO CURRENT_USER")
        cur.execute("GRANT USAGE ON SCHEMA aethyme TO aethyme_runtime_test")
        cur.execute("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA aethyme TO aethyme_runtime_test")
    conn.commit()


PRIMARY_NODE_SPECS = [
    SeedNodeSpec("user_service", "services/user.py:UserService", "services/user.py", 10, 0, NodeKind.CLASS.value, "python", "class UserService", "User authentication and profile management service"),
    SeedNodeSpec("user_auth", "services/user.py:UserService.authenticate", "services/user.py", 25, 4, NodeKind.METHOD.value, "python", "def authenticate(email: str, password: str) -> User", "Authenticate a user with email and password"),
    SeedNodeSpec("user_create", "services/user.py:UserService.create_user", "services/user.py", 45, 4, NodeKind.METHOD.value, "python", "def create_user(data: dict) -> User", "Create a new user account"),
    SeedNodeSpec("user_profile", "services/user.py:UserService.get_profile", "services/user.py", 65, 4, NodeKind.METHOD.value, "python", "def get_profile(user_id: str) -> UserProfile", "Get a user profile by identifier"),
    SeedNodeSpec("user_update", "services/user.py:UserService.update_profile", "services/user.py", 82, 4, NodeKind.METHOD.value, "python", "def update_profile(user_id: str, data: dict) -> UserProfile", "Update an existing profile"),
    SeedNodeSpec("product_service", "services/product.py:ProductService", "services/product.py", 10, 0, NodeKind.CLASS.value, "python", "class ProductService", "Product catalog and inventory management"),
    SeedNodeSpec("product_search", "services/product.py:ProductService.search", "services/product.py", 25, 4, NodeKind.METHOD.value, "python", "def search(query: str, filters: dict) -> list[Product]", "Search products with filters"),
    SeedNodeSpec("product_get", "services/product.py:ProductService.get_by_id", "services/product.py", 44, 4, NodeKind.METHOD.value, "python", "def get_by_id(product_id: str) -> Product", "Get a product by identifier"),
    SeedNodeSpec("product_stock", "services/product.py:ProductService.update_stock", "services/product.py", 60, 4, NodeKind.METHOD.value, "python", "def update_stock(product_id: str, quantity: int)", "Update product stock quantity"),
    SeedNodeSpec("product_create", "services/product.py:ProductService.create_product", "services/product.py", 78, 4, NodeKind.METHOD.value, "python", "def create_product(data: dict) -> Product", "Create a catalog product"),
    SeedNodeSpec("order_service", "services/order.py:OrderService", "services/order.py", 15, 0, NodeKind.CLASS.value, "python", "class OrderService", "Order processing and fulfillment service"),
    SeedNodeSpec("order_create", "services/order.py:OrderService.create_order", "services/order.py", 34, 4, NodeKind.METHOD.value, "python", "def create_order(user_id: str, items: list[dict]) -> Order", "Create a new order"),
    SeedNodeSpec("order_payment", "services/order.py:OrderService.process_payment", "services/order.py", 55, 4, NodeKind.METHOD.value, "python", "def process_payment(order_id: str, payment_info: dict) -> bool", "Process payment for an order"),
    SeedNodeSpec("order_get", "services/order.py:OrderService.get_order", "services/order.py", 78, 4, NodeKind.METHOD.value, "python", "def get_order(order_id: str) -> Order", "Get an order by identifier"),
    SeedNodeSpec("order_status", "services/order.py:OrderService.update_status", "services/order.py", 95, 4, NodeKind.METHOD.value, "python", "def update_status(order_id: str, status: str)", "Update order lifecycle status"),
    SeedNodeSpec("payment_service", "services/payment.py:PaymentService", "services/payment.py", 18, 0, NodeKind.CLASS.value, "python", "class PaymentService", "Payment processing service"),
    SeedNodeSpec("payment_charge", "services/payment.py:PaymentService.charge", "services/payment.py", 38, 4, NodeKind.METHOD.value, "python", "def charge(amount: float, payment_method: str) -> Transaction", "Charge a payment method"),
    SeedNodeSpec("payment_refund", "services/payment.py:PaymentService.refund", "services/payment.py", 58, 4, NodeKind.METHOD.value, "python", "def refund(transaction_id: str, amount: float) -> Transaction", "Refund a transaction"),
    SeedNodeSpec("payment_validate", "services/payment.py:PaymentService.validate_card", "services/payment.py", 82, 4, NodeKind.METHOD.value, "python", "def validate_card(card_info: dict) -> bool", "Validate card details"),
    SeedNodeSpec("user_model", "models/user.py:User", "models/user.py", 10, 0, NodeKind.CLASS.value, "python", "class User(BaseModel)", "User data model"),
    SeedNodeSpec("user_profile_model", "models/user.py:UserProfile", "models/user.py", 30, 0, NodeKind.CLASS.value, "python", "class UserProfile(BaseModel)", "User profile data"),
    SeedNodeSpec("product_model", "models/product.py:Product", "models/product.py", 12, 0, NodeKind.CLASS.value, "python", "class Product(BaseModel)", "Product data model"),
    SeedNodeSpec("order_model", "models/order.py:Order", "models/order.py", 15, 0, NodeKind.CLASS.value, "python", "class Order(BaseModel)", "Order data model"),
    SeedNodeSpec("transaction_model", "models/payment.py:Transaction", "models/payment.py", 18, 0, NodeKind.CLASS.value, "python", "class Transaction(BaseModel)", "Payment transaction model"),
    SeedNodeSpec("user_repo", "db/user_repository.py:UserRepository", "db/user_repository.py", 10, 0, NodeKind.CLASS.value, "python", "class UserRepository", "User database operations"),
    SeedNodeSpec("user_repo_find", "db/user_repository.py:UserRepository.find_by_email", "db/user_repository.py", 26, 4, NodeKind.METHOD.value, "python", "def find_by_email(email: str) -> User | None", "Find a user by email"),
    SeedNodeSpec("user_repo_save", "db/user_repository.py:UserRepository.save", "db/user_repository.py", 42, 4, NodeKind.METHOD.value, "python", "def save(user: User) -> User", "Save a user"),
    SeedNodeSpec("product_repo", "db/product_repository.py:ProductRepository", "db/product_repository.py", 12, 0, NodeKind.CLASS.value, "python", "class ProductRepository", "Product database operations"),
    SeedNodeSpec("product_repo_search", "db/product_repository.py:ProductRepository.search", "db/product_repository.py", 28, 4, NodeKind.METHOD.value, "python", "def search(query: str) -> list[Product]", "Search products in storage"),
    SeedNodeSpec("order_repo", "db/order_repository.py:OrderRepository", "db/order_repository.py", 15, 0, NodeKind.CLASS.value, "python", "class OrderRepository", "Order database operations"),
    SeedNodeSpec("order_repo_save", "db/order_repository.py:OrderRepository.save", "db/order_repository.py", 34, 4, NodeKind.METHOD.value, "python", "def save(order: Order) -> Order", "Persist an order"),
    SeedNodeSpec("validate_email", "utils/validators.py:validate_email", "utils/validators.py", 10, 0, NodeKind.FUNCTION.value, "python", "def validate_email(email: str) -> bool", "Validate email format"),
    SeedNodeSpec("validate_password", "utils/validators.py:validate_password", "utils/validators.py", 25, 0, NodeKind.FUNCTION.value, "python", "def validate_password(password: str) -> bool", "Validate password strength"),
    SeedNodeSpec("hash_password", "utils/crypto.py:hash_password", "utils/crypto.py", 15, 0, NodeKind.FUNCTION.value, "python", "def hash_password(password: str) -> str", "Hash a password using bcrypt"),
    SeedNodeSpec("verify_password", "utils/crypto.py:verify_password", "utils/crypto.py", 32, 0, NodeKind.FUNCTION.value, "python", "def verify_password(password: str, hash_value: str) -> bool", "Verify a password against a hash"),
    SeedNodeSpec("send_email", "utils/email.py:send_email", "utils/email.py", 20, 0, NodeKind.FUNCTION.value, "python", "def send_email(to: str, subject: str, body: str)", "Send an email notification"),
    SeedNodeSpec("create_user_route", "api/routes/users.py:create_user", "api/routes/users.py", 20, 0, NodeKind.FUNCTION.value, "python", '@app.post("/api/users/")', "Create user endpoint"),
    SeedNodeSpec("login_route", "api/routes/users.py:login", "api/routes/users.py", 35, 0, NodeKind.FUNCTION.value, "python", '@app.post("/api/login/")', "Login endpoint"),
    SeedNodeSpec("profile_route", "api/routes/users.py:get_profile", "api/routes/users.py", 50, 0, NodeKind.FUNCTION.value, "python", '@app.get("/api/users/{user_id}")', "Get user profile endpoint"),
    SeedNodeSpec("products_route", "api/routes/products.py:search_products", "api/routes/products.py", 25, 0, NodeKind.FUNCTION.value, "python", '@app.get("/api/products/search/")', "Search products endpoint"),
    SeedNodeSpec("product_route", "api/routes/products.py:get_product", "api/routes/products.py", 44, 0, NodeKind.FUNCTION.value, "python", '@app.get("/api/products/{product_id}")', "Get product endpoint"),
    SeedNodeSpec("orders_route", "api/routes/orders.py:create_order", "api/routes/orders.py", 30, 0, NodeKind.FUNCTION.value, "python", '@app.post("/api/orders/")', "Create order endpoint"),
    SeedNodeSpec("order_route", "api/routes/orders.py:get_order", "api/routes/orders.py", 55, 0, NodeKind.FUNCTION.value, "python", '@app.get("/api/orders/{order_id}")', "Get order endpoint"),
    SeedNodeSpec("login_form", "frontend/components/LoginForm.tsx:LoginForm", "frontend/components/LoginForm.tsx", 15, 0, NodeKind.FUNCTION.value, "typescript", "export const LoginForm: React.FC", "Login form component"),
    SeedNodeSpec("product_card", "frontend/components/ProductCard.tsx:ProductCard", "frontend/components/ProductCard.tsx", 20, 0, NodeKind.FUNCTION.value, "typescript", "export const ProductCard: React.FC<Props>", "Product display card component"),
    SeedNodeSpec("cart_component", "frontend/components/Cart.tsx:Cart", "frontend/components/Cart.tsx", 25, 0, NodeKind.FUNCTION.value, "typescript", "export const Cart: React.FC", "Shopping cart component"),
    SeedNodeSpec("api_client", "frontend/api/client.ts:apiClient", "frontend/api/client.ts", 10, 0, NodeKind.VARIABLE.value, "typescript", "export const apiClient", "Frontend API client instance"),
    SeedNodeSpec("frontend_login", "frontend/api/auth.ts:login", "frontend/api/auth.ts", 15, 0, NodeKind.FUNCTION.value, "typescript", "export async function login(email: string, password: string)", "Login API call"),
    SeedNodeSpec("notification_service", "services/notification.py:NotificationService", "services/notification.py", 10, 0, NodeKind.CLASS.value, "python", "class NotificationService", "Notification service"),
    SeedNodeSpec("notification_send", "services/notification.py:NotificationService.send", "services/notification.py", 25, 4, NodeKind.METHOD.value, "python", "def send(user_id: str, message: str)", "Send a notification"),
    SeedNodeSpec("analytics_service", "services/analytics.py:AnalyticsService", "services/analytics.py", 12, 0, NodeKind.CLASS.value, "python", "class AnalyticsService", "Analytics tracking"),
    SeedNodeSpec("analytics_track", "services/analytics.py:AnalyticsService.track_event", "services/analytics.py", 30, 4, NodeKind.METHOD.value, "python", "def track_event(event: str, data: dict)", "Track analytics events"),
    SeedNodeSpec("cache_service", "services/cache.py:CacheService", "services/cache.py", 15, 0, NodeKind.CLASS.value, "python", "class CacheService", "Redis cache service"),
    SeedNodeSpec("cache_get", "services/cache.py:CacheService.get", "services/cache.py", 35, 4, NodeKind.METHOD.value, "python", "def get(key: str) -> object | None", "Get a value from cache"),
    SeedNodeSpec("cache_set", "services/cache.py:CacheService.set", "services/cache.py", 50, 4, NodeKind.METHOD.value, "python", "def set(key: str, value: object, ttl: int)", "Set a value in cache"),
    SeedNodeSpec("require_auth", "middleware/auth.py:require_auth", "middleware/auth.py", 20, 0, NodeKind.FUNCTION.value, "python", "def require_auth(fn)", "Authentication decorator"),
    SeedNodeSpec("settings", "config/settings.py:Settings", "config/settings.py", 10, 0, NodeKind.CLASS.value, "python", "class Settings(BaseSettings)", "Application settings"),
    SeedNodeSpec("db_connection", "config/database.py:get_db_connection", "config/database.py", 20, 0, NodeKind.FUNCTION.value, "python", "def get_db_connection() -> Connection", "Get a database connection"),
    SeedNodeSpec("send_welcome_email", "tasks/email_worker.py:send_welcome_email", "tasks/email_worker.py", 15, 0, NodeKind.FUNCTION.value, "python", "def send_welcome_email(user_id: str)", "Send welcome email task"),
    SeedNodeSpec("process_order", "tasks/order_worker.py:process_order", "tasks/order_worker.py", 22, 0, NodeKind.FUNCTION.value, "python", "def process_order(order_id: str)", "Process order background task"),
    SeedNodeSpec("update_inventory", "tasks/inventory_worker.py:update_inventory", "tasks/inventory_worker.py", 25, 0, NodeKind.FUNCTION.value, "python", "def update_inventory(product_id: str, quantity: int)", "Update inventory task"),
    SeedNodeSpec("test_authenticate", "tests/test_user_service.py:test_authenticate", "tests/test_user_service.py", 30, 0, NodeKind.FUNCTION.value, "python", "def test_authenticate()", "Test user authentication"),
    SeedNodeSpec("test_search", "tests/test_product_service.py:test_search", "tests/test_product_service.py", 35, 0, NodeKind.FUNCTION.value, "python", "def test_search()", "Test product search"),
    SeedNodeSpec("test_create_order", "tests/test_order_service.py:test_create_order", "tests/test_order_service.py", 40, 0, NodeKind.FUNCTION.value, "python", "def test_create_order()", "Test order creation"),
]

SECONDARY_NODE_SPECS = [
    SeedNodeSpec("blog_service", "services/blog.py:BlogService", "services/blog.py", 10, 0, NodeKind.CLASS.value, "python", "class BlogService", "Blog management service"),
    SeedNodeSpec("blog_create", "services/blog.py:BlogService.create_post", "services/blog.py", 25, 4, NodeKind.METHOD.value, "python", "def create_post(title: str, content: str)", "Create blog post"),
    SeedNodeSpec("blog_list", "services/blog.py:BlogService.get_posts", "services/blog.py", 42, 4, NodeKind.METHOD.value, "python", "def get_posts() -> list[Post]", "Get all blog posts"),
    SeedNodeSpec("blog_route", "api/routes/blog.py:list_posts", "api/routes/blog.py", 20, 0, NodeKind.FUNCTION.value, "python", '@app.get("/api/posts/")', "List blog posts endpoint"),
]

PRIMARY_EDGE_SPECS = [
    ("user_service", "user_auth", "contain"),
    ("user_service", "user_create", "contain"),
    ("user_service", "user_profile", "contain"),
    ("user_service", "user_update", "contain"),
    ("user_service", "user_model", "import"),
    ("user_service", "user_profile_model", "import"),
    ("user_service", "user_repo", "import"),
    ("user_auth", "user_repo_find", "invoke"),
    ("user_auth", "verify_password", "invoke"),
    ("user_create", "user_repo_save", "invoke"),
    ("user_create", "hash_password", "invoke"),
    ("user_create", "validate_email", "invoke"),
    ("user_create", "validate_password", "invoke"),
    ("user_profile", "user_repo", "import"),
    ("user_update", "user_profile_model", "return"),
    ("product_service", "product_search", "contain"),
    ("product_service", "product_get", "contain"),
    ("product_service", "product_stock", "contain"),
    ("product_service", "product_create", "contain"),
    ("product_service", "product_model", "import"),
    ("product_service", "product_repo", "import"),
    ("product_search", "product_repo_search", "invoke"),
    ("product_search", "cache_service", "import"),
    ("product_search", "cache_get", "invoke"),
    ("product_stock", "cache_set", "invoke"),
    ("order_service", "order_create", "contain"),
    ("order_service", "order_payment", "contain"),
    ("order_service", "order_get", "contain"),
    ("order_service", "order_status", "contain"),
    ("order_service", "order_model", "import"),
    ("order_service", "order_repo", "import"),
    ("order_service", "product_service", "import"),
    ("order_service", "payment_service", "import"),
    ("order_create", "order_repo_save", "invoke"),
    ("order_create", "product_stock", "invoke"),
    ("order_payment", "payment_charge", "invoke"),
    ("order_get", "order_repo", "import"),
    ("order_status", "order_repo_save", "invoke"),
    ("payment_service", "payment_charge", "contain"),
    ("payment_service", "payment_refund", "contain"),
    ("payment_service", "payment_validate", "contain"),
    ("payment_service", "transaction_model", "import"),
    ("payment_charge", "payment_validate", "invoke"),
    ("payment_charge", "notification_send", "invoke"),
    ("notification_service", "notification_send", "contain"),
    ("analytics_service", "analytics_track", "contain"),
    ("cache_service", "cache_get", "contain"),
    ("cache_service", "cache_set", "contain"),
    ("create_user_route", "user_service", "import"),
    ("create_user_route", "user_create", "invoke"),
    ("create_user_route", "analytics_track", "invoke"),
    ("login_route", "user_service", "import"),
    ("login_route", "user_auth", "invoke"),
    ("login_route", "analytics_track", "invoke"),
    ("profile_route", "require_auth", "import"),
    ("profile_route", "user_service", "import"),
    ("profile_route", "user_profile", "invoke"),
    ("products_route", "product_service", "import"),
    ("products_route", "product_search", "invoke"),
    ("product_route", "product_service", "import"),
    ("product_route", "product_get", "invoke"),
    ("orders_route", "order_service", "import"),
    ("orders_route", "order_create", "invoke"),
    ("orders_route", "analytics_track", "invoke"),
    ("order_route", "order_service", "import"),
    ("order_route", "order_get", "invoke"),
    ("login_form", "frontend_login", "invoke"),
    ("frontend_login", "api_client", "import"),
    ("frontend_login", "login_route", "invoke"),
    ("product_card", "api_client", "import"),
    ("cart_component", "api_client", "import"),
    ("send_welcome_email", "send_email", "invoke"),
    ("process_order", "order_service", "import"),
    ("process_order", "order_status", "invoke"),
    ("update_inventory", "product_service", "import"),
    ("update_inventory", "product_stock", "invoke"),
    ("test_authenticate", "user_service", "import"),
    ("test_authenticate", "user_auth", "invoke"),
    ("test_search", "product_service", "import"),
    ("test_search", "product_search", "invoke"),
    ("test_create_order", "order_service", "import"),
    ("test_create_order", "order_create", "invoke"),
    ("user_repo", "db_connection", "import"),
    ("product_repo", "db_connection", "import"),
    ("order_repo", "db_connection", "import"),
    ("user_repo_find", "user_model", "return"),
    ("user_repo_save", "user_model", "import"),
    ("product_repo_search", "product_model", "return"),
    ("order_repo_save", "order_model", "import"),
    ("settings", "db_connection", "import"),
    ("user_service", "order_service", "import"),
    ("order_service", "user_service", "import"),
]

SECONDARY_EDGE_SPECS = [
    ("blog_service", "blog_create", "contain"),
    ("blog_service", "blog_list", "contain"),
    ("blog_route", "blog_service", "import"),
    ("blog_route", "blog_list", "invoke"),
]


def seed_graph_dataset(conn: PGConnection) -> GraphSeedContext:
    """Populate the canonical graph dataset used by query tests."""
    context = GraphSeedContext()

    with conn.cursor() as cur:
        cur.execute(
            """
            INSERT INTO aethyme.orgs (id, name, slug, description)
            VALUES (%s, %s, %s, %s), (%s, %s, %s, %s)
            ON CONFLICT (id) DO NOTHING
            """,
            (
                context.primary_org_id,
                "Primary Test Org",
                "primary-test-org",
                "Seeded graph dataset primary organization",
                context.secondary_org_id,
                "Secondary Test Org",
                "secondary-test-org",
                "Seeded graph dataset secondary organization",
            ),
        )
        cur.execute(
            """
            INSERT INTO aethyme.tenants (id, org_id, name, slug, description)
            VALUES (%s, %s, %s, %s, %s), (%s, %s, %s, %s, %s)
            ON CONFLICT (id) DO NOTHING
            """,
            (
                context.primary_tenant_id,
                context.primary_org_id,
                "Primary Test Tenant",
                "primary-test-tenant",
                "Primary seeded tenant",
                context.secondary_tenant_id,
                context.secondary_org_id,
                "Secondary Test Tenant",
                "secondary-test-tenant",
                "Secondary seeded tenant",
            ),
        )
        for org_id, tenant_id, repository_id, name, path, languages in [
            (
                context.primary_org_id,
                context.primary_tenant_id,
                context.primary_repository_id,
                "primary-query-repo",
                "/tmp/aethyme-primary-query-repo",
                ["python", "typescript"],
            ),
            (
                context.secondary_org_id,
                context.secondary_tenant_id,
                context.secondary_repository_id,
                "secondary-query-repo",
                "/tmp/aethyme-secondary-query-repo",
                ["python"],
            ),
        ]:
            cur.execute("SELECT set_config('app.current_org', %s, false)", (org_id,))
            cur.execute("SELECT set_config('app.current_tenant', %s, false)", (tenant_id,))
            cur.execute(
                "SELECT set_config('app.current_scopes', %s, false)",
                ('[\"repo:write\",\"org:admin\"]',),
            )
            cur.execute(
                """
                INSERT INTO aethyme.repositories (
                    id, org_id, tenant_id, name, path, languages, index_status
                )
                VALUES (%s, %s, %s, %s, %s, %s, %s)
                ON CONFLICT (id) DO NOTHING
                """,
                (
                    repository_id,
                    org_id,
                    tenant_id,
                    name,
                    path,
                    languages,
                    "ready",
                ),
            )

    _insert_nodes(conn, PRIMARY_NODE_SPECS, context.primary_org_id, context.primary_tenant_id, context.primary_repository_id)
    _insert_nodes(conn, SECONDARY_NODE_SPECS, context.secondary_org_id, context.secondary_tenant_id, context.secondary_repository_id)

    primary_nodes = _build_node_map(PRIMARY_NODE_SPECS)
    secondary_nodes = _build_node_map(SECONDARY_NODE_SPECS)

    _insert_edges(conn, PRIMARY_EDGE_SPECS, primary_nodes, context.primary_org_id, context.primary_tenant_id, context.primary_repository_id)
    _insert_edges(conn, SECONDARY_EDGE_SPECS, secondary_nodes, context.secondary_org_id, context.secondary_tenant_id, context.secondary_repository_id)

    conn.commit()
    return context


def _build_node_map(specs: Iterable[SeedNodeSpec]) -> dict[str, Node]:
    nodes: dict[str, Node] = {}
    for spec in specs:
        nodes[spec.alias] = Node(
            symbol=spec.symbol,
            file_path=spec.file_path,
            line_number=spec.line_number,
            column_number=spec.column_number,
            kind=spec.kind,
            language=spec.language,
            signature=spec.signature,
            documentation=spec.documentation,
        )
    return nodes


def _insert_nodes(
    conn: PGConnection,
    specs: Iterable[SeedNodeSpec],
    org_id: str,
    tenant_id: str,
    repository_id: str,
) -> None:
    node_map = _build_node_map(specs)
    params: list[SQLParams] = [
        (
            node.id,
            node.display_id,
            org_id,
            tenant_id,
            repository_id,
            node.symbol,
            node.file_path,
            node.line_number,
            node.column_number,
            node.kind,
            node.language,
            node.signature,
            node.documentation,
            Json(node.metadata or {}),
        )
        for node in node_map.values()
    ]

    with conn.cursor() as cur:
        cur.execute("SELECT set_config('app.current_org', %s, false)", (org_id,))
        cur.execute("SELECT set_config('app.current_tenant', %s, false)", (tenant_id,))
        cur.execute(
            "SELECT set_config('app.current_scopes', %s, false)",
            ('[\"repo:write\",\"org:admin\"]',),
        )
        typed_execute_batch(
            cur,
            """
            INSERT INTO aethyme.nodes (
                id, display_id, org_id, tenant_id, repository_id, symbol, file_path,
                line_number, column_number, kind, language, signature, documentation, metadata
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (id) DO NOTHING
            """,
            params,
            page_size=200,
        )


def _insert_edges(
    conn: PGConnection,
    specs: Iterable[tuple[str, str, str]],
    node_map: dict[str, Node],
    org_id: str,
    tenant_id: str,
    repository_id: str,
) -> None:
    params: list[SQLParams] = []
    for from_alias, to_alias, edge_type in specs:
        edge = Edge.create(
            from_node_id=node_map[from_alias].id,
            to_node_id=node_map[to_alias].id,
            edge_type=edge_type,
        )
        params.append(
            (
                edge.id,
                org_id,
                tenant_id,
                repository_id,
                edge.from_node_id,
                edge.to_node_id,
                edge.edge_type,
                edge.weight,
                Json(edge.metadata or {}),
            )
        )

    with conn.cursor() as cur:
        cur.execute("SELECT set_config('app.current_org', %s, false)", (org_id,))
        cur.execute("SELECT set_config('app.current_tenant', %s, false)", (tenant_id,))
        cur.execute(
            "SELECT set_config('app.current_scopes', %s, false)",
            ('[\"repo:write\",\"org:admin\"]',),
        )
        typed_execute_batch(
            cur,
            """
            INSERT INTO aethyme.edges (
                id, org_id, tenant_id, repository_id, from_node_id,
                to_node_id, edge_type, weight, metadata
            ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
            ON CONFLICT (id) DO NOTHING
            """,
            params,
            page_size=200,
        )
