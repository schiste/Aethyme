"""Simple test script for authentication endpoints."""

import asyncio
import httpx
from datetime import datetime

BASE_URL = "http://localhost:8001/api/v1"

async def test_auth_flow():
    """Test the complete authentication flow."""
    async with httpx.AsyncClient() as client:
        print("🧪 Testing Aethyme Cloud Authentication\n")
        print("=" * 60)

        # Test 1: Register a new user
        print("\n1️⃣ Testing user registration...")
        register_data = {
            "email": f"test.{datetime.now().timestamp()}@example.com",
            "password": "SecurePassword123!",
            "full_name": "Test User"
        }

        try:
            response = await client.post(f"{BASE_URL}/auth/register", json=register_data)
            print(f"   Status: {response.status_code}")

            if response.status_code == 201:
                data = response.json()
                access_token = data["access_token"]
                refresh_token = data["refresh_token"]
                user = data["user"]

                print(f"   ✅ User registered successfully!")
                print(f"   User ID: {user['id']}")
                print(f"   Email: {user['email']}")
                print(f"   Organization ID: {user['organization_id']}")
                print(f"   Token type: {data['token_type']}")

                # Test 2: Access protected endpoint
                print("\n2️⃣ Testing protected endpoint (GET /users/me)...")
                headers = {"Authorization": f"Bearer {access_token}"}
                me_response = await client.get(f"{BASE_URL}/users/me", headers=headers)
                print(f"   Status: {me_response.status_code}")

                if me_response.status_code == 200:
                    me_data = me_response.json()
                    print(f"   ✅ Successfully accessed protected endpoint!")
                    print(f"   User: {me_data['full_name']} ({me_data['email']})")
                else:
                    print(f"   ❌ Failed: {me_response.text}")

                # Test 3: Get organization details
                print("\n3️⃣ Testing organization endpoint...")
                org_response = await client.get(f"{BASE_URL}/organizations/me", headers=headers)
                print(f"   Status: {org_response.status_code}")

                if org_response.status_code == 200:
                    org_data = org_response.json()
                    print(f"   ✅ Organization loaded!")
                    print(f"   Name: {org_data['name']}")
                    print(f"   Plan: {org_data['plan']}")
                    print(f"   Max repositories: {org_data['max_repositories']}")
                    print(f"   Queries/month: {org_data['max_queries_per_month']}")
                else:
                    print(f"   ❌ Failed: {org_response.text}")

                # Test 4: Refresh token
                print("\n4️⃣ Testing token refresh...")
                refresh_response = await client.post(
                    f"{BASE_URL}/auth/refresh",
                    json={"refresh_token": refresh_token}
                )
                print(f"   Status: {refresh_response.status_code}")

                if refresh_response.status_code == 200:
                    refresh_data = refresh_response.json()
                    print(f"   ✅ Token refreshed successfully!")
                    print(f"   New access token: {refresh_data['access_token'][:20]}...")
                else:
                    print(f"   ❌ Failed: {refresh_response.text}")

                # Test 5: Login with credentials
                print("\n5️⃣ Testing login with credentials...")
                login_data = {
                    "email": register_data["email"],
                    "password": register_data["password"]
                }
                login_response = await client.post(f"{BASE_URL}/auth/login", json=login_data)
                print(f"   Status: {login_response.status_code}")

                if login_response.status_code == 200:
                    login_result = login_response.json()
                    print(f"   ✅ Login successful!")
                    print(f"   Access token: {login_result['access_token'][:20]}...")
                else:
                    print(f"   ❌ Failed: {login_response.text}")

            else:
                print(f"   ❌ Registration failed: {response.text}")

        except Exception as e:
            print(f"   ❌ Error: {str(e)}")

        print("\n" + "=" * 60)
        print("✅ Authentication flow test complete!\n")


if __name__ == "__main__":
    asyncio.run(test_auth_flow())
