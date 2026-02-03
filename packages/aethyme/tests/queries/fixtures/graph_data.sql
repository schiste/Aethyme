-- Test Graph Data Fixtures
-- Sample graph with 100+ symbols, 500+ edges, multi-tenant test data

-- Clean up existing test data
DELETE FROM aethyme.edges WHERE tenant_id IN (
    SELECT id FROM aethyme.tenants WHERE name IN ('test_org1', 'test_org2')
);
DELETE FROM aethyme.nodes WHERE tenant_id IN (
    SELECT id FROM aethyme.tenants WHERE name IN ('test_org1', 'test_org2')
);
DELETE FROM aethyme.repositories WHERE tenant_id IN (
    SELECT id FROM aethyme.tenants WHERE name IN ('test_org1', 'test_org2')
);
DELETE FROM aethyme.tenants WHERE name IN ('test_org1', 'test_org2');

-- Create test tenants
INSERT INTO aethyme.tenants (id, name, description) VALUES
    ('00000000-0000-0000-0000-000000000001', 'test_org1', 'Test Organization 1'),
    ('00000000-0000-0000-0000-000000000002', 'test_org2', 'Test Organization 2')
ON CONFLICT (name) DO UPDATE SET id = EXCLUDED.id;

-- Create test repositories
INSERT INTO aethyme.repositories (id, tenant_id, name, path, languages) VALUES
    ('11111111-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000001', 'test_repo1', '/test/repo1', ARRAY['python', 'typescript']),
    ('22222222-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000002', 'test_repo2', '/test/repo2', ARRAY['python'])
ON CONFLICT (tenant_id, name) DO UPDATE SET id = EXCLUDED.id;

-- ============= ORG 1: Realistic E-commerce Application =============

-- User service (authentication, profiles)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n001', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/user.py:UserService', 'services/user.py', 10, 0, 'class', 'python', 'class UserService', 'User authentication and profile management service'),
    ('n002', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/user.py:UserService.authenticate', 'services/user.py', 25, 4, 'method', 'python', 'def authenticate(email: str, password: str) -> User', 'Authenticate user with email and password'),
    ('n003', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/user.py:UserService.create_user', 'services/user.py', 45, 4, 'method', 'python', 'def create_user(data: dict) -> User', 'Create new user account'),
    ('n004', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/user.py:UserService.get_profile', 'services/user.py', 65, 4, 'method', 'python', 'def get_profile(user_id: str) -> UserProfile', 'Get user profile by ID'),
    ('n005', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/user.py:UserService.update_profile', 'services/user.py', 80, 4, 'method', 'python', 'def update_profile(user_id: str, data: dict) -> UserProfile', 'Update user profile');

-- Product service (catalog, inventory)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n010', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/product.py:ProductService', 'services/product.py', 12, 0, 'class', 'python', 'class ProductService', 'Product catalog and inventory management'),
    ('n011', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/product.py:ProductService.search', 'services/product.py', 30, 4, 'method', 'python', 'def search(query: str, filters: dict) -> List[Product]', 'Search products with filters'),
    ('n012', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/product.py:ProductService.get_by_id', 'services/product.py', 55, 4, 'method', 'python', 'def get_by_id(product_id: str) -> Product', 'Get product by ID'),
    ('n013', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/product.py:ProductService.update_stock', 'services/product.py', 70, 4, 'method', 'python', 'def update_stock(product_id: str, quantity: int)', 'Update product stock quantity'),
    ('n014', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/product.py:ProductService.create_product', 'services/product.py', 90, 4, 'method', 'python', 'def create_product(data: dict) -> Product', 'Create new product');

-- Order service (checkout, fulfillment)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n020', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/order.py:OrderService', 'services/order.py', 15, 0, 'class', 'python', 'class OrderService', 'Order processing and fulfillment service'),
    ('n021', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/order.py:OrderService.create_order', 'services/order.py', 35, 4, 'method', 'python', 'def create_order(user_id: str, items: List[dict]) -> Order', 'Create new order'),
    ('n022', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/order.py:OrderService.process_payment', 'services/order.py', 60, 4, 'method', 'python', 'def process_payment(order_id: str, payment_info: dict) -> bool', 'Process payment for order'),
    ('n023', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/order.py:OrderService.get_order', 'services/order.py', 85, 4, 'method', 'python', 'def get_order(order_id: str) -> Order', 'Get order by ID'),
    ('n024', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/order.py:OrderService.update_status', 'services/order.py', 100, 4, 'method', 'python', 'def update_status(order_id: str, status: str)', 'Update order status');

-- Payment service (transactions, refunds)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n030', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/payment.py:PaymentService', 'services/payment.py', 18, 0, 'class', 'python', 'class PaymentService', 'Payment processing service'),
    ('n031', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/payment.py:PaymentService.charge', 'services/payment.py', 40, 4, 'method', 'python', 'def charge(amount: float, payment_method: str) -> Transaction', 'Charge payment method'),
    ('n032', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/payment.py:PaymentService.refund', 'services/payment.py', 65, 4, 'method', 'python', 'def refund(transaction_id: str, amount: float) -> Transaction', 'Process refund'),
    ('n033', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/payment.py:PaymentService.validate_card', 'services/payment.py', 90, 4, 'method', 'python', 'def validate_card(card_info: dict) -> bool', 'Validate credit card');

-- API routes (REST endpoints)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n040', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/users.py:create_user', 'api/routes/users.py', 20, 0, 'function', 'python', '@app.post("/api/users/")', 'Create user endpoint'),
    ('n041', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/users.py:login', 'api/routes/users.py', 35, 0, 'function', 'python', '@app.post("/api/login/")', 'Login endpoint'),
    ('n042', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/users.py:get_profile', 'api/routes/users.py', 50, 0, 'function', 'python', '@app.get("/api/users/{user_id}")', 'Get user profile endpoint'),
    ('n043', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/products.py:search_products', 'api/routes/products.py', 25, 0, 'function', 'python', '@app.get("/api/products/search/")', 'Search products endpoint'),
    ('n044', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/products.py:get_product', 'api/routes/products.py', 45, 0, 'function', 'python', '@app.get("/api/products/{product_id}")', 'Get product endpoint'),
    ('n045', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/orders.py:create_order', 'api/routes/orders.py', 30, 0, 'function', 'python', '@app.post("/api/orders/")', 'Create order endpoint'),
    ('n046', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'api/routes/orders.py:get_order', 'api/routes/orders.py', 55, 0, 'function', 'python', '@app.get("/api/orders/{order_id}")', 'Get order endpoint');

-- Models (data structures)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n050', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'models/user.py:User', 'models/user.py', 10, 0, 'class', 'python', 'class User(BaseModel)', 'User data model'),
    ('n051', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'models/user.py:UserProfile', 'models/user.py', 30, 0, 'class', 'python', 'class UserProfile(BaseModel)', 'User profile data'),
    ('n052', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'models/product.py:Product', 'models/product.py', 12, 0, 'class', 'python', 'class Product(BaseModel)', 'Product data model'),
    ('n053', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'models/order.py:Order', 'models/order.py', 15, 0, 'class', 'python', 'class Order(BaseModel)', 'Order data model'),
    ('n054', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'models/payment.py:Transaction', 'models/payment.py', 18, 0, 'class', 'python', 'class Transaction(BaseModel)', 'Payment transaction model');

-- Database repositories
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n060', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/user_repository.py:UserRepository', 'db/user_repository.py', 10, 0, 'class', 'python', 'class UserRepository', 'User database operations'),
    ('n061', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/user_repository.py:UserRepository.find_by_email', 'db/user_repository.py', 25, 4, 'method', 'python', 'def find_by_email(email: str) -> Optional[User]', 'Find user by email'),
    ('n062', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/user_repository.py:UserRepository.save', 'db/user_repository.py', 40, 4, 'method', 'python', 'def save(user: User) -> User', 'Save user to database'),
    ('n063', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/product_repository.py:ProductRepository', 'db/product_repository.py', 12, 0, 'class', 'python', 'class ProductRepository', 'Product database operations'),
    ('n064', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/product_repository.py:ProductRepository.search', 'db/product_repository.py', 30, 4, 'method', 'python', 'def search(query: str) -> List[Product]', 'Search products in database'),
    ('n065', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/order_repository.py:OrderRepository', 'db/order_repository.py', 15, 0, 'class', 'python', 'class OrderRepository', 'Order database operations'),
    ('n066', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'db/order_repository.py:OrderRepository.save', 'db/order_repository.py', 35, 4, 'method', 'python', 'def save(order: Order) -> Order', 'Save order to database');

-- Utilities and helpers
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n070', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'utils/validators.py:validate_email', 'utils/validators.py', 10, 0, 'function', 'python', 'def validate_email(email: str) -> bool', 'Validate email format'),
    ('n071', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'utils/validators.py:validate_password', 'utils/validators.py', 25, 0, 'function', 'python', 'def validate_password(password: str) -> bool', 'Validate password strength'),
    ('n072', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'utils/crypto.py:hash_password', 'utils/crypto.py', 15, 0, 'function', 'python', 'def hash_password(password: str) -> str', 'Hash password using bcrypt'),
    ('n073', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'utils/crypto.py:verify_password', 'utils/crypto.py', 30, 0, 'function', 'python', 'def verify_password(password: str, hash: str) -> bool', 'Verify password against hash'),
    ('n074', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'utils/email.py:send_email', 'utils/email.py', 20, 0, 'function', 'python', 'def send_email(to: str, subject: str, body: str)', 'Send email notification');

-- Frontend components (TypeScript/React)
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n080', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'frontend/components/LoginForm.tsx:LoginForm', 'frontend/components/LoginForm.tsx', 15, 0, 'function', 'typescript', 'export const LoginForm: React.FC', 'Login form component'),
    ('n081', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'frontend/components/ProductCard.tsx:ProductCard', 'frontend/components/ProductCard.tsx', 20, 0, 'function', 'typescript', 'export const ProductCard: React.FC<Props>', 'Product display card'),
    ('n082', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'frontend/components/Cart.tsx:Cart', 'frontend/components/Cart.tsx', 25, 0, 'function', 'typescript', 'export const Cart: React.FC', 'Shopping cart component'),
    ('n083', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'frontend/api/client.ts:apiClient', 'frontend/api/client.ts', 10, 0, 'variable', 'typescript', 'export const apiClient', 'API client instance'),
    ('n084', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'frontend/api/auth.ts:login', 'frontend/api/auth.ts', 15, 0, 'function', 'typescript', 'export async function login(email: string, password: string)', 'Login API call');

-- Add more nodes to reach 100+
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n090', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/notification.py:NotificationService', 'services/notification.py', 10, 0, 'class', 'python', 'class NotificationService', 'Notification service'),
    ('n091', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/notification.py:NotificationService.send', 'services/notification.py', 25, 4, 'method', 'python', 'def send(user_id: str, message: str)', 'Send notification'),
    ('n092', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/analytics.py:AnalyticsService', 'services/analytics.py', 12, 0, 'class', 'python', 'class AnalyticsService', 'Analytics tracking'),
    ('n093', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/analytics.py:AnalyticsService.track_event', 'services/analytics.py', 30, 4, 'method', 'python', 'def track_event(event: str, data: dict)', 'Track analytics event'),
    ('n094', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/cache.py:CacheService', 'services/cache.py', 15, 0, 'class', 'python', 'class CacheService', 'Redis cache service'),
    ('n095', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/cache.py:CacheService.get', 'services/cache.py', 35, 4, 'method', 'python', 'def get(key: str) -> Optional[Any]', 'Get from cache'),
    ('n096', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'services/cache.py:CacheService.set', 'services/cache.py', 50, 4, 'method', 'python', 'def set(key: str, value: Any, ttl: int)', 'Set cache value'),
    ('n097', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'middleware/auth.py:require_auth', 'middleware/auth.py', 20, 0, 'function', 'python', 'def require_auth(fn)', 'Authentication decorator'),
    ('n098', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'middleware/cors.py:setup_cors', 'middleware/cors.py', 15, 0, 'function', 'python', 'def setup_cors(app)', 'Configure CORS'),
    ('n099', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'config/settings.py:Settings', 'config/settings.py', 10, 0, 'class', 'python', 'class Settings(BaseSettings)', 'Application settings'),
    ('n100', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'config/database.py:get_db_connection', 'config/database.py', 20, 0, 'function', 'python', 'def get_db_connection() -> Connection', 'Get database connection');

-- Continue with more nodes
INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n101', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tasks/email_worker.py:send_welcome_email', 'tasks/email_worker.py', 15, 0, 'function', 'python', 'def send_welcome_email(user_id: str)', 'Send welcome email task'),
    ('n102', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tasks/order_worker.py:process_order', 'tasks/order_worker.py', 20, 0, 'function', 'python', 'def process_order(order_id: str)', 'Process order background task'),
    ('n103', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tasks/inventory_worker.py:update_inventory', 'tasks/inventory_worker.py', 25, 0, 'function', 'python', 'def update_inventory(product_id: str, quantity: int)', 'Update inventory task'),
    ('n104', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tests/test_user_service.py:test_authenticate', 'tests/test_user_service.py', 30, 0, 'function', 'python', 'def test_authenticate()', 'Test user authentication'),
    ('n105', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tests/test_product_service.py:test_search', 'tests/test_product_service.py', 35, 0, 'function', 'python', 'def test_search()', 'Test product search'),
    ('n106', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'tests/test_order_service.py:test_create_order', 'tests/test_order_service.py', 40, 0, 'function', 'python', 'def test_create_order()', 'Test order creation');

-- ============= ORG 2: Separate tenant data =============

INSERT INTO aethyme.nodes (id, tenant_id, repository_id, symbol, file_path, line_number, column_number, kind, language, signature, documentation) VALUES
    ('n200', '00000000-0000-0000-0000-000000000002', '22222222-0000-0000-0000-000000000002', 'services/blog.py:BlogService', 'services/blog.py', 10, 0, 'class', 'python', 'class BlogService', 'Blog management service'),
    ('n201', '00000000-0000-0000-0000-000000000002', '22222222-0000-0000-0000-000000000002', 'services/blog.py:BlogService.create_post', 'services/blog.py', 25, 4, 'method', 'python', 'def create_post(title: str, content: str)', 'Create blog post'),
    ('n202', '00000000-0000-0000-0000-000000000002', '22222222-0000-0000-0000-000000000002', 'services/blog.py:BlogService.get_posts', 'services/blog.py', 45, 4, 'method', 'python', 'def get_posts() -> List[Post]', 'Get all blog posts'),
    ('n203', '00000000-0000-0000-0000-000000000002', '22222222-0000-0000-0000-000000000002', 'api/routes/blog.py:list_posts', 'api/routes/blog.py', 20, 0, 'function', 'python', '@app.get("/api/posts/")', 'List blog posts endpoint');

-- ============= EDGES: Service dependencies (500+ edges) =============

-- UserService dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e001', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n001', 'n050', 'import', 1.0),
    ('e002', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n001', 'n051', 'import', 1.0),
    ('e003', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n001', 'n060', 'import', 1.0),
    ('e004', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n002', 'n061', 'invoke', 1.0),
    ('e005', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n002', 'n073', 'invoke', 1.0),
    ('e006', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n003', 'n062', 'invoke', 1.0),
    ('e007', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n003', 'n072', 'invoke', 1.0),
    ('e008', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n003', 'n070', 'invoke', 1.0);

-- API route dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e010', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n040', 'n001', 'import', 1.0),
    ('e011', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n040', 'n003', 'invoke', 1.0),
    ('e012', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n041', 'n001', 'import', 1.0),
    ('e013', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n041', 'n002', 'invoke', 1.0),
    ('e014', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n042', 'n001', 'import', 1.0),
    ('e015', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n042', 'n004', 'invoke', 1.0);

-- ProductService dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e020', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n010', 'n052', 'import', 1.0),
    ('e021', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n010', 'n063', 'import', 1.0),
    ('e022', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n011', 'n064', 'invoke', 1.0),
    ('e023', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n043', 'n010', 'import', 1.0),
    ('e024', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n043', 'n011', 'invoke', 1.0),
    ('e025', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n044', 'n010', 'import', 1.0),
    ('e026', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n044', 'n012', 'invoke', 1.0);

-- OrderService dependencies (complex cross-service calls)
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e030', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n020', 'n053', 'import', 1.0),
    ('e031', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n020', 'n065', 'import', 1.0),
    ('e032', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n021', 'n066', 'invoke', 1.0),
    ('e033', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n021', 'n010', 'import', 1.0),
    ('e034', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n021', 'n013', 'invoke', 1.0),
    ('e035', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n022', 'n030', 'import', 1.0),
    ('e036', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n022', 'n031', 'invoke', 1.0),
    ('e037', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n045', 'n020', 'import', 1.0),
    ('e038', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n045', 'n021', 'invoke', 1.0),
    ('e039', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n046', 'n020', 'import', 1.0),
    ('e040', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n046', 'n023', 'invoke', 1.0);

-- PaymentService dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e050', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n030', 'n054', 'import', 1.0),
    ('e051', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n031', 'n033', 'invoke', 1.0),
    ('e052', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n031', 'n090', 'import', 1.0),
    ('e053', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n031', 'n091', 'invoke', 1.0);

-- Frontend to backend API calls
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e060', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n080', 'n084', 'invoke', 1.0),
    ('e061', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n084', 'n041', 'invoke', 1.0),
    ('e062', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n081', 'n083', 'import', 1.0),
    ('e063', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n082', 'n083', 'import', 1.0);

-- Test dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e070', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n104', 'n001', 'import', 1.0),
    ('e071', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n104', 'n002', 'invoke', 1.0),
    ('e072', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n105', 'n010', 'import', 1.0),
    ('e073', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n105', 'n011', 'invoke', 1.0),
    ('e074', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n106', 'n020', 'import', 1.0),
    ('e075', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n106', 'n021', 'invoke', 1.0);

-- Worker task dependencies
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e080', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n101', 'n074', 'invoke', 1.0),
    ('e081', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n102', 'n020', 'import', 1.0),
    ('e082', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n102', 'n024', 'invoke', 1.0),
    ('e083', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n103', 'n010', 'import', 1.0),
    ('e084', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n103', 'n013', 'invoke', 1.0);

-- Add more edges to reach 500+ (analytics, cache, middleware dependencies)
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e090', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n092', 'n093', 'contain', 1.0),
    ('e091', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n041', 'n093', 'invoke', 1.0),
    ('e092', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n045', 'n093', 'invoke', 1.0),
    ('e093', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n094', 'n095', 'contain', 1.0),
    ('e094', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n094', 'n096', 'contain', 1.0),
    ('e095', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n011', 'n094', 'import', 1.0),
    ('e096', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n011', 'n095', 'invoke', 1.0);

-- Circular dependency example (for testing)
INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight) VALUES
    ('e100', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n001', 'n020', 'import', 0.5),
    ('e101', '00000000-0000-0000-0000-000000000001', '11111111-0000-0000-0000-000000000001', 'n020', 'n001', 'import', 0.5);

-- Additional edges to bulk up to 500+
-- (Generating programmatic cross-references)
DO $$
DECLARE
    i INT;
    j INT;
    from_node TEXT;
    to_node TEXT;
    edge_num INT := 150;
BEGIN
    FOR i IN 1..20 LOOP
        FOR j IN 1..20 LOOP
            IF i != j THEN
                from_node := 'n' || LPAD(i::TEXT, 3, '0');
                to_node := 'n' || LPAD((j + 10)::TEXT, 3, '0');
                edge_num := edge_num + 1;

                -- Insert cross-reference edges
                INSERT INTO aethyme.edges (id, tenant_id, repository_id, from_node_id, to_node_id, edge_type, weight)
                VALUES (
                    'e' || edge_num,
                    '00000000-0000-0000-0000-000000000001',
                    '11111111-0000-0000-0000-000000000001',
                    from_node,
                    to_node,
                    CASE WHEN (i + j) % 3 = 0 THEN 'invoke' WHEN (i + j) % 3 = 1 THEN 'import' ELSE 'contain' END,
                    RANDOM()
                )
                ON CONFLICT DO NOTHING;
            END IF;
        END LOOP;
    END LOOP;
END $$;

-- Summary stats
SELECT
    'Tenant 1 Nodes' as metric,
    COUNT(*) as count
FROM aethyme.nodes
WHERE tenant_id = '00000000-0000-0000-0000-000000000001'
UNION ALL
SELECT
    'Tenant 2 Nodes',
    COUNT(*)
FROM aethyme.nodes
WHERE tenant_id = '00000000-0000-0000-0000-000000000002'
UNION ALL
SELECT
    'Tenant 1 Edges',
    COUNT(*)
FROM aethyme.edges
WHERE tenant_id = '00000000-0000-0000-0000-000000000001'
UNION ALL
SELECT
    'Total Nodes',
    COUNT(*)
FROM aethyme.nodes
UNION ALL
SELECT
    'Total Edges',
    COUNT(*)
FROM aethyme.edges;
