from fastapi import APIRouter, Depends

router = APIRouter()

@router.post("/users")
def create_user(current_user=Depends(get_current_user)):
    return {"status": "ok"}

def get_current_user():
    return {}
