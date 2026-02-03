"""
Staleness Monitor

Tracks when invariants change, monitors parity score drops,
and triggers re-exports when significant changes occur.

Sprint 1 Task S1-T11 - Agent-Enablement Parity
"""

import hashlib
import json
import logging
from dataclasses import dataclass, asdict
from datetime import datetime, timedelta
from pathlib import Path
from typing import Dict, List, Optional, Any

logger = logging.getLogger(__name__)


@dataclass
class StalenessMetrics:
    """Metrics for staleness detection"""
    last_scan: str
    last_significant_change: Optional[str]
    parity_score: float
    parity_trend: float  # Change since last scan
    invariant_changes: Dict[str, float]
    context_pack_checksum: str
    needs_rescan: bool
    reason: Optional[str] = None


class StalenessMonitor:
    """
    Monitors repository staleness and triggers updates.

    Tracks:
    - Last parity scan timestamp
    - Parity score changes
    - Invariant violations changes
    - Context pack freshness
    - File modification times

    Triggers:
    - Re-scan if parity drops > 10 points
    - Re-export context pack if checksum changes
    - Alert if critical invariants violated
    """

    def __init__(self, cache_dir: Optional[Path] = None):
        self.cache_dir = cache_dir or Path.home() / ".aethyme" / "staleness"
        self.cache_dir.mkdir(parents=True, exist_ok=True)

        # Thresholds
        self.rescan_threshold_days = 7  # Re-scan weekly
        self.parity_drop_threshold = 10.0  # Re-scan if score drops > 10
        self.significant_change_threshold = 5.0  # Alert if any invariant changes > 5 points

    def check_staleness(
        self,
        repo_path: Path,
        current_parity_score: float,
        current_invariant_scores: Dict[str, float],
        context_pack_checksum: str
    ) -> StalenessMetrics:
        """
        Check if repository analysis is stale.

        Args:
            repo_path: Repository path
            current_parity_score: Current parity score
            current_invariant_scores: Current invariant scores by ID
            context_pack_checksum: Current context pack checksum

        Returns:
            Staleness metrics with needs_rescan flag
        """
        repo_id = self._get_repo_id(repo_path)
        cache_file = self.cache_dir / f"{repo_id}.json"

        # Load previous metrics
        previous = self._load_previous_metrics(cache_file)

        if not previous:
            # First scan
            metrics = StalenessMetrics(
                last_scan=datetime.utcnow().isoformat() + "Z",
                last_significant_change=None,
                parity_score=current_parity_score,
                parity_trend=0.0,
                invariant_changes={},
                context_pack_checksum=context_pack_checksum,
                needs_rescan=False
            )
            self._save_metrics(cache_file, metrics)
            return metrics

        # Calculate changes
        parity_trend = current_parity_score - previous["parity_score"]

        invariant_changes = {}
        for inv_id, current_score in current_invariant_scores.items():
            if inv_id in previous.get("invariant_scores", {}):
                prev_score = previous["invariant_scores"][inv_id]
                change = current_score - prev_score
                invariant_changes[inv_id] = change

        # Determine if rescan needed
        needs_rescan, reason = self._should_rescan(
            previous,
            parity_trend,
            invariant_changes,
            context_pack_checksum
        )

        # Detect significant changes
        last_significant_change = None
        for inv_id, change in invariant_changes.items():
            if abs(change) >= self.significant_change_threshold:
                last_significant_change = datetime.utcnow().isoformat() + "Z"
                break

        metrics = StalenessMetrics(
            last_scan=datetime.utcnow().isoformat() + "Z",
            last_significant_change=last_significant_change,
            parity_score=current_parity_score,
            parity_trend=parity_trend,
            invariant_changes=invariant_changes,
            context_pack_checksum=context_pack_checksum,
            needs_rescan=needs_rescan,
            reason=reason
        )

        # Save current metrics for next check
        self._save_metrics(cache_file, metrics, current_invariant_scores)

        return metrics

    def _should_rescan(
        self,
        previous: Dict[str, Any],
        parity_trend: float,
        invariant_changes: Dict[str, float],
        context_pack_checksum: str
    ) -> tuple[bool, Optional[str]]:
        """Determine if rescan is needed"""

        # Check time since last scan
        last_scan_str = previous.get("last_scan")
        if last_scan_str:
            last_scan = datetime.fromisoformat(last_scan_str.replace("Z", "+00:00"))
            days_since = (datetime.now() - last_scan.replace(tzinfo=None)).days

            if days_since >= self.rescan_threshold_days:
                return True, f"Weekly rescan ({days_since} days since last scan)"

        # Check parity score drop
        if parity_trend < -self.parity_drop_threshold:
            return True, f"Parity score dropped by {abs(parity_trend):.1f} points"

        # Check invariant changes
        for inv_id, change in invariant_changes.items():
            if change < -self.significant_change_threshold:
                return True, f"Invariant {inv_id} dropped by {abs(change):.1f} points"

        # Check context pack changes
        prev_checksum = previous.get("context_pack_checksum")
        if prev_checksum and prev_checksum != context_pack_checksum:
            return True, "Context pack changed (checksum mismatch)"

        return False, None

    def get_stale_repositories(self, repos: List[Path]) -> List[tuple[Path, str]]:
        """
        Get list of repositories that need rescanning.

        Returns:
            List of (repo_path, reason) tuples
        """
        stale_repos = []

        for repo_path in repos:
            repo_id = self._get_repo_id(repo_path)
            cache_file = self.cache_dir / f"{repo_id}.json"

            if not cache_file.exists():
                stale_repos.append((repo_path, "Never scanned"))
                continue

            previous = self._load_previous_metrics(cache_file)
            if not previous:
                stale_repos.append((repo_path, "Invalid cache"))
                continue

            # Check time-based staleness
            last_scan_str = previous.get("last_scan")
            if last_scan_str:
                last_scan = datetime.fromisoformat(last_scan_str.replace("Z", "+00:00"))
                days_since = (datetime.now() - last_scan.replace(tzinfo=None)).days

                if days_since >= self.rescan_threshold_days:
                    stale_repos.append((repo_path, f"Stale ({days_since} days)"))

        return stale_repos

    def alert_on_parity_drop(
        self,
        repo_path: Path,
        metrics: StalenessMetrics,
        alert_callback: Optional[callable] = None
    ) -> None:
        """Alert if parity score dropped significantly"""

        if metrics.parity_trend < -self.parity_drop_threshold:
            message = (
                f"ALERT: Repository {repo_path.name} parity score dropped "
                f"by {abs(metrics.parity_trend):.1f} points "
                f"(now {metrics.parity_score:.1f}/100)"
            )

            logger.warning(message)

            if alert_callback:
                alert_callback({
                    "type": "parity_drop",
                    "repo": str(repo_path),
                    "score": metrics.parity_score,
                    "trend": metrics.parity_trend,
                    "message": message
                })

    def monitor_context_pack_freshness(
        self,
        repo_path: Path,
        current_checksum: str
    ) -> bool:
        """
        Check if context pack needs re-export.

        Returns:
            True if re-export needed
        """
        repo_id = self._get_repo_id(repo_path)
        cache_file = self.cache_dir / f"{repo_id}.json"

        previous = self._load_previous_metrics(cache_file)

        if not previous:
            return True  # First export

        prev_checksum = previous.get("context_pack_checksum")
        if not prev_checksum or prev_checksum != current_checksum:
            logger.info(f"Context pack changed for {repo_path.name}")
            return True

        return False

    def get_monitoring_dashboard_data(self, repos: List[Path]) -> Dict[str, Any]:
        """Get monitoring dashboard data for multiple repositories"""

        dashboard = {
            "total_repos": len(repos),
            "healthy": 0,
            "stale": 0,
            "critical": 0,
            "repos": []
        }

        for repo_path in repos:
            repo_id = self._get_repo_id(repo_path)
            cache_file = self.cache_dir / f"{repo_id}.json"

            previous = self._load_previous_metrics(cache_file)

            if not previous:
                status = "never_scanned"
                dashboard["stale"] += 1
            else:
                last_scan = datetime.fromisoformat(
                    previous["last_scan"].replace("Z", "+00:00")
                )
                days_since = (datetime.now() - last_scan.replace(tzinfo=None)).days

                if days_since >= self.rescan_threshold_days:
                    status = "stale"
                    dashboard["stale"] += 1
                elif previous["parity_score"] < 60:
                    status = "critical"
                    dashboard["critical"] += 1
                else:
                    status = "healthy"
                    dashboard["healthy"] += 1

                dashboard["repos"].append({
                    "repo": repo_path.name,
                    "status": status,
                    "parity_score": previous.get("parity_score"),
                    "last_scan": previous.get("last_scan"),
                    "days_since_scan": days_since
                })

        return dashboard

    # Helper methods

    def _get_repo_id(self, repo_path: Path) -> str:
        """Generate unique ID for repository"""
        repo_str = str(repo_path.resolve())
        return hashlib.md5(repo_str.encode()).hexdigest()[:16]

    def _load_previous_metrics(self, cache_file: Path) -> Optional[Dict[str, Any]]:
        """Load previous metrics from cache"""
        if not cache_file.exists():
            return None

        try:
            return json.loads(cache_file.read_text())
        except Exception as e:
            logger.error(f"Error loading cache {cache_file}: {e}")
            return None

    def _save_metrics(
        self,
        cache_file: Path,
        metrics: StalenessMetrics,
        invariant_scores: Optional[Dict[str, float]] = None
    ) -> None:
        """Save current metrics to cache"""
        data = asdict(metrics)

        if invariant_scores:
            data["invariant_scores"] = invariant_scores

        try:
            cache_file.write_text(json.dumps(data, indent=2))
        except Exception as e:
            logger.error(f"Error saving cache {cache_file}: {e}")

    def clear_cache(self, repo_path: Optional[Path] = None) -> None:
        """Clear staleness cache for repo or all repos"""
        if repo_path:
            repo_id = self._get_repo_id(repo_path)
            cache_file = self.cache_dir / f"{repo_id}.json"
            if cache_file.exists():
                cache_file.unlink()
                logger.info(f"Cleared cache for {repo_path.name}")
        else:
            for cache_file in self.cache_dir.glob("*.json"):
                cache_file.unlink()
            logger.info("Cleared all staleness cache")
