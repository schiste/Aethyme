from .models import Parcel


def list_parcels():
    return [Parcel(identifier="p-1", name="sample")]
