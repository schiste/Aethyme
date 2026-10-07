from pydantic import BaseModel

class User(BaseModel):
    name: str

def next_model():
    return None
