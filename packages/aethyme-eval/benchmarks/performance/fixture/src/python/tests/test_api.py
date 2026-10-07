from app.api import get_parcels


def test_get_parcels_returns_service_rows():
    assert get_parcels()[0].identifier == "p-1"
