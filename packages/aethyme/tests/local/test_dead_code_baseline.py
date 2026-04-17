from __future__ import annotations

from src.eval.schemas import aethyme_dead_code_reference, mediawiki_dead_code_reference


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


def test_aethyme_dead_code_reference_uses_reviewed_indexing_baseline() -> None:
    reference = aethyme_dead_code_reference()

    assert reference["baseline_id"] == "aethyme-dead-code-indexing-v1"
    assert len(reference["unused_functions"]) == 13

    by_name = {item["function_name"]: item for item in reference["unused_functions"]}
    assert by_name["setup_indexing_logging"]["defined_in"] == "packages/aethyme/src/indexing/logging.py"
    assert by_name["index_repository"]["defined_in"] == "packages/aethyme/src/indexing/service.py"
    assert by_name["workspace_blast_radius"]["defined_in"] == "packages/aethyme/src/indexing/engine.py"


def test_aethyme_dead_code_reference_preserves_engineering_review_split() -> None:
    reference = aethyme_dead_code_reference()

    likely = {
        item["function_name"]
        for item in reference["engineering_review"]["likely_dead_code"]
    }
    assert "activate" in likely
    assert "workspace_inspect" in likely
