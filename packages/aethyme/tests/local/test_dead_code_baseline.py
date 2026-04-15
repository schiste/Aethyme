from __future__ import annotations

from src.eval.schemas import mediawiki_dead_code_reference


def test_mediawiki_dead_code_reference_uses_reviewed_watchlist_baseline() -> None:
    reference = mediawiki_dead_code_reference()

    assert reference["baseline_id"] == "mediawiki-dead-code-watchlist-v2"
    assert len(reference["unused_functions"]) == 10

    by_name = {item["function_name"]: item for item in reference["unused_functions"]}
    assert by_name["countAllForUser"]["defined_in"] == "includes/Watchlist/WatchlistLabelStore.php"
    assert by_name["duplicateEntry"]["defined_in"] == "includes/Watchlist/WatchedItemStore.php"
    assert by_name["newForUser"]["defined_in"] == "includes/Watchlist/ClearUserWatchlistJob.php"
    assert by_name["modifyWatchedItemsWithRCInfo"]["defined_in"] == "includes/Watchlist/WatchedItemQueryServiceExtension.php"


def test_mediawiki_dead_code_reference_preserves_engineering_review_split() -> None:
    reference = mediawiki_dead_code_reference()

    likely = {
        item["function_name"]
        for item in reference["engineering_review"]["likely_dead_code"]
    }
    assert likely == {"overrideDeferredUpdatesAddCallableUpdateCallback"}
