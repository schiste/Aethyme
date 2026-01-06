from fastapi import APIRouter

router = APIRouter()

@router.post("/users")
async def create_user(data: dict):
    return {"id": "123"}

@router.get("/users/{user_id}")
async def get_user(user_id: str):
    return {"id": user_id}

@router.delete("/users/{user_id}")
async def delete_user(user_id: str):
    return {"deleted": True}
