# Freshness Monitoring Dashboard Setup Guide

## Overview

This guide shows how to set up monitoring dashboards for Aethyme indexing freshness and reliability.

## Prerequisites

- Prometheus installed and configured
- Grafana installed (optional, for visualization)
- Aethyme API running with metrics enabled

## 1. Prometheus Setup

### Install Prometheus

```bash
# macOS
brew install prometheus

# Linux
wget https://github.com/prometheus/prometheus/releases/download/v2.45.0/prometheus-2.45.0.linux-amd64.tar.gz
tar xvfz prometheus-*.tar.gz
cd prometheus-*
```

### Configure Prometheus

Create or edit `prometheus.yml`:

```yaml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: 'aethyme'
    static_configs:
      - targets: ['localhost:8000']
    metrics_path: '/metrics'
    scrape_interval: 15s
    scrape_timeout: 10s

alerting:
  alertmanagers:
    - static_configs:
        - targets: ['localhost:9093']

rule_files:
  - 'aethyme_alerts.yml'
```

### Create Alert Rules

Create `aethyme_alerts.yml`:

```yaml
groups:
  - name: aethyme_indexing
    interval: 30s
    rules:
      # High failure rate
      - alert: HighIndexFailureRate
        expr: |
          rate(aethyme_index_failures_total[5m]) > 0.05
        for: 10m
        labels:
          severity: warning
        annotations:
          summary: "High indexing failure rate detected"
          description: "Failure rate is {{ $value | humanizePercentage }} over last 5 minutes"

      # Slow indexing
      - alert: IndexingSlow
        expr: |
          histogram_quantile(0.95,
            rate(aethyme_index_duration_seconds_bucket[5m])
          ) > 300
        for: 15m
        labels:
          severity: warning
        annotations:
          summary: "Indexing is slow"
          description: "P95 latency is {{ $value }}s (>5min)"

      # Critical staleness
      - alert: CriticalStaleness
        expr: |
          aethyme_index_staleness_seconds > 259200
        for: 1h
        labels:
          severity: critical
        annotations:
          summary: "Repository index is critically stale"
          description: "Repository {{ $labels.repository }} hasn't been indexed in {{ $value | humanizeDuration }}"

      # Stale warning
      - alert: StaleIndex
        expr: |
          aethyme_index_staleness_seconds > 86400
        for: 2h
        labels:
          severity: warning
        annotations:
          summary: "Repository index is stale"
          description: "Repository {{ $labels.repository }} hasn't been indexed in {{ $value | humanizeDuration }}"

      # Circuit breaker open
      - alert: CircuitBreakerOpen
        expr: |
          aethyme_circuit_breaker_state == 2
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "Circuit breaker is open"
          description: "Circuit {{ $labels.circuit_name }} has been open for 5+ minutes"

      # High fallback usage
      - alert: HighFallbackUsage
        expr: |
          sum(rate(aethyme_indexer_fallback_total[1h])) /
          sum(rate(aethyme_index_operations_total[1h])) > 0.3
        for: 2h
        labels:
          severity: warning
        annotations:
          summary: "High fallback indexer usage"
          description: "Fallback indexer used in {{ $value | humanizePercentage }} of operations"
```

### Start Prometheus

```bash
./prometheus --config.file=prometheus.yml
```

Access Prometheus at: http://localhost:9090

## 2. Grafana Setup (Optional)

### Install Grafana

```bash
# macOS
brew install grafana

# Linux
sudo apt-get install -y grafana
```

### Start Grafana

```bash
# macOS
brew services start grafana

# Linux
sudo systemctl start grafana-server
sudo systemctl enable grafana-server
```

Access Grafana at: http://localhost:3000
Default login: admin/admin

### Add Prometheus Data Source

1. Go to Configuration > Data Sources
2. Click "Add data source"
3. Select "Prometheus"
4. Set URL: `http://localhost:9090`
5. Click "Save & Test"

### Import Dashboard

Create `aethyme-dashboard.json`:

```json
{
  "dashboard": {
    "title": "Aethyme Indexing",
    "panels": [
      {
        "title": "Index Duration (p50, p95, p99)",
        "targets": [
          {
            "expr": "histogram_quantile(0.50, rate(aethyme_index_duration_seconds_bucket[5m]))",
            "legendFormat": "p50"
          },
          {
            "expr": "histogram_quantile(0.95, rate(aethyme_index_duration_seconds_bucket[5m]))",
            "legendFormat": "p95"
          },
          {
            "expr": "histogram_quantile(0.99, rate(aethyme_index_duration_seconds_bucket[5m]))",
            "legendFormat": "p99"
          }
        ]
      },
      {
        "title": "Failure Rate",
        "targets": [
          {
            "expr": "rate(aethyme_index_failures_total[5m])",
            "legendFormat": "{{ repository }}"
          }
        ]
      },
      {
        "title": "Symbol Count by Language",
        "targets": [
          {
            "expr": "aethyme_index_symbols_total",
            "legendFormat": "{{ language }}"
          }
        ]
      },
      {
        "title": "Staleness Distribution",
        "targets": [
          {
            "expr": "aethyme_index_staleness_seconds / 3600",
            "legendFormat": "{{ repository }}"
          }
        ]
      },
      {
        "title": "Circuit Breaker State",
        "targets": [
          {
            "expr": "aethyme_circuit_breaker_state",
            "legendFormat": "{{ circuit_name }}"
          }
        ]
      },
      {
        "title": "Fallback Usage %",
        "targets": [
          {
            "expr": "sum(rate(aethyme_indexer_fallback_total[1h])) / sum(rate(aethyme_index_operations_total[1h])) * 100",
            "legendFormat": "Fallback %"
          }
        ]
      }
    ]
  }
}
```

Import via: Dashboards > Import > Upload JSON

## 3. API-Based Monitoring

### Check Index Status

```bash
# Get status for specific repo
curl -X GET "http://localhost:8000/api/index/status/REPO_ID" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  | jq

# Response
{
  "repo_id": "uuid",
  "repo_name": "my-repo",
  "last_indexed_at": "2025-11-22T10:00:00Z",
  "is_stale": false,
  "staleness_status": "fresh",
  "staleness_human": "2 hours ago",
  "symbol_count": 1234,
  "language_breakdown": {
    "python": 800,
    "typescript": 434
  },
  "errors": [],
  "duration_seconds": 87.5,
  "index_status": "completed"
}
```

### Check Freshness Summary

```bash
curl -X GET "http://localhost:8000/api/index/freshness" \
  -H "Authorization: Bearer YOUR_TOKEN" \
  | jq

# Response
{
  "tenant_id": "uuid",
  "total_repositories": 10,
  "fresh_count": 7,
  "stale_count": 2,
  "critical_count": 1,
  "never_indexed_count": 0,
  "stale_repositories": [...]
}
```

### Trigger Re-indexing

```bash
curl -X POST "http://localhost:8000/api/index/trigger/REPO_ID" \
  -H "Authorization: Bearer YOUR_TOKEN"
```

## 4. Key Metrics to Monitor

### Performance Metrics

```promql
# Index duration percentiles
histogram_quantile(0.95, rate(aethyme_index_duration_seconds_bucket[5m]))

# Average duration by language
avg(aethyme_index_duration_seconds) by (language)

# Memory usage trend (if tracked)
rate(process_resident_memory_bytes[5m])
```

### Reliability Metrics

```promql
# Failure rate
rate(aethyme_index_failures_total[5m])

# Success rate
sum(rate(aethyme_index_operations_total{status="success"}[5m])) /
sum(rate(aethyme_index_operations_total[5m]))

# Circuit breaker failures
rate(aethyme_circuit_breaker_failures_total[5m])
```

### Freshness Metrics

```promql
# Average staleness (in hours)
avg(aethyme_index_staleness_seconds / 3600)

# Number of stale repos (>24h)
count(aethyme_index_staleness_seconds > 86400)

# Number of critical repos (>72h)
count(aethyme_index_staleness_seconds > 259200)
```

### Resource Metrics

```promql
# Fallback usage percentage
sum(rate(aethyme_indexer_fallback_total[1h])) /
sum(rate(aethyme_index_operations_total[1h])) * 100

# Symbol count by language
sum(aethyme_index_symbols_total) by (language)

# Retry attempts
rate(aethyme_index_retry_attempts_total[5m])
```

## 5. Scheduled Monitoring

### Cron Job for Staleness Check

```bash
# Add to crontab (run every hour)
0 * * * * /usr/local/bin/check_staleness.sh

# check_staleness.sh
#!/bin/bash
STALE=$(curl -s http://localhost:8000/api/index/freshness | jq '.stale_count + .critical_count')

if [ "$STALE" -gt 0 ]; then
  echo "Found $STALE stale repositories"
  # Send alert (e.g., to Slack, PagerDuty)
fi
```

### Python Script for Monitoring

```python
import requests
import time
from datetime import datetime

def check_freshness():
    """Check repo freshness and alert if stale."""
    response = requests.get(
        "http://localhost:8000/api/index/freshness",
        headers={"Authorization": "Bearer YOUR_TOKEN"},
    )

    data = response.json()

    if data["critical_count"] > 0:
        print(f"ALERT: {data['critical_count']} critically stale repos")
        # Send alert

    if data["stale_count"] > 5:
        print(f"WARNING: {data['stale_count']} stale repos")
        # Send warning

if __name__ == "__main__":
    while True:
        check_freshness()
        time.sleep(3600)  # Check every hour
```

## 6. Alert Notifications

### Slack Integration

```python
import requests

def send_slack_alert(message):
    webhook_url = "YOUR_SLACK_WEBHOOK"
    payload = {
        "text": message,
        "channel": "#aethyme-alerts",
        "username": "Aethyme Monitor",
    }
    requests.post(webhook_url, json=payload)

# Use in alerting
if stale_count > 0:
    send_slack_alert(f"⚠️ {stale_count} repositories are stale")
```

### Email Alerts

```python
import smtplib
from email.mime.text import MIMEText

def send_email_alert(subject, body):
    msg = MIMEText(body)
    msg['Subject'] = subject
    msg['From'] = 'aethyme@example.com'
    msg['To'] = 'ops-team@example.com'

    with smtplib.SMTP('localhost') as server:
        server.send_message(msg)
```

## 7. Dashboard Examples

### Simple CLI Dashboard

```bash
#!/bin/bash
# dashboard.sh - Simple CLI dashboard

echo "=== Aethyme Index Status ==="
echo

# Get freshness summary
FRESHNESS=$(curl -s http://localhost:8000/api/index/freshness \
  -H "Authorization: Bearer $TOKEN")

echo "Total Repos: $(echo $FRESHNESS | jq -r '.total_repositories')"
echo "Fresh: $(echo $FRESHNESS | jq -r '.fresh_count')"
echo "Stale: $(echo $FRESHNESS | jq -r '.stale_count')"
echo "Critical: $(echo $FRESHNESS | jq -r '.critical_count')"
echo

# Get metrics
echo "=== Metrics ==="
curl -s http://localhost:8000/metrics | grep aethyme_index | head -10
```

### Web Dashboard (Simple HTML)

```html
<!DOCTYPE html>
<html>
<head>
    <title>Aethyme Status</title>
    <script>
        async function loadStatus() {
            const response = await fetch('/api/index/freshness', {
                headers: {'Authorization': 'Bearer YOUR_TOKEN'}
            });
            const data = await response.json();

            document.getElementById('total').textContent = data.total_repositories;
            document.getElementById('fresh').textContent = data.fresh_count;
            document.getElementById('stale').textContent = data.stale_count;
            document.getElementById('critical').textContent = data.critical_count;
        }

        setInterval(loadStatus, 60000);  // Refresh every minute
        loadStatus();
    </script>
</head>
<body>
    <h1>Aethyme Index Status</h1>
    <div>
        <p>Total: <span id="total">-</span></p>
        <p>Fresh: <span id="fresh">-</span></p>
        <p>Stale: <span id="stale">-</span></p>
        <p>Critical: <span id="critical">-</span></p>
    </div>
</body>
</html>
```

## 8. Troubleshooting

### No Metrics Appearing

```bash
# Check if metrics endpoint is accessible
curl http://localhost:8000/metrics

# Check Prometheus targets
# Go to http://localhost:9090/targets
# Ensure aethyme target is UP
```

### High Memory Usage

```bash
# Check current memory
ps aux | grep aethyme

# Monitor over time
while true; do
    ps aux | grep aethyme | awk '{print $4}'
    sleep 60
done
```

### Stale Repositories Not Updating

```bash
# Check if re-index trigger works
curl -X POST http://localhost:8000/api/index/trigger/REPO_ID \
  -H "Authorization: Bearer $TOKEN"

# Check logs
tail -f /var/log/aethyme/indexing.log
```

## Summary

This setup provides comprehensive monitoring for Aethyme indexing:

1. **Prometheus** - Metrics collection and alerting
2. **Grafana** - Visual dashboards (optional)
3. **API Endpoints** - Programmatic access
4. **Scheduled Jobs** - Automated monitoring
5. **Alerts** - Slack/email notifications

For production, ensure:
- Prometheus has adequate retention
- Alerts are configured for on-call team
- Dashboards are accessible to operations
- Regular review of metrics and thresholds

---

**Setup Time:** ~30 minutes
**Maintenance:** Minimal (automated)
**Dependencies:** Prometheus, Grafana (optional)
