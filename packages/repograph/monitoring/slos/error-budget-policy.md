# Error Budget Policy

## Overview

Error budgets quantify how unreliable our service is allowed to be within a period. This policy defines what actions we take as error budgets are consumed.

## What is an Error Budget?

If our SLO is 99.9% availability, we have a 0.1% error budget:
- **Monthly budget:** 43.2 minutes of downtime
- **Daily budget:** 1.44 minutes of downtime

## Policy Levels

### Level 1: Healthy (75-100% budget remaining)

**Status:** Normal operations

**Actions:**
- Continue planned feature development
- Deploy on regular cadence
- Standard monitoring and alerting
- Weekly SLO review in team meeting

**Example:** Budget remaining: 40 minutes / 43.2 minutes (92%)

---

### Level 2: Warning (50-75% budget remaining)

**Status:** Increased vigilance

**Actions:**
- Review recent changes and deployments
- Increase monitoring frequency
- Slow down non-critical feature rollout
- Daily SLO review in standups
- Identify top error contributors
- Plan reliability improvements

**Restrictions:**
- Require senior engineer approval for risky deployments
- Extend canary deployment windows
- Enhanced testing for new features

**Example:** Budget remaining: 25 minutes / 43.2 minutes (58%)

---

### Level 3: Critical (25-50% budget remaining)

**Status:** Reliability crisis

**Actions:**
- Freeze non-critical feature deployments
- Incident response mode activated
- Root cause analysis for all outages
- Prioritize reliability work over features
- Daily incident review meetings
- Update stakeholders daily

**Restrictions:**
- All deployments require VP Engineering approval
- Only deploy bug fixes and rollbacks
- Cancel or postpone risky releases
- All changes must improve reliability

**Example:** Budget remaining: 15 minutes / 43.2 minutes (35%)

---

### Level 4: Emergency (0-25% budget remaining)

**Status:** All hands on deck

**Actions:**
- Complete deployment freeze except rollbacks
- Dedicated war room established
- Executive leadership informed
- Customer communication prepared
- Comprehensive post-mortem in progress
- Recovery roadmap required

**Restrictions:**
- Zero tolerance for new feature work
- Only P0 incident fixes allowed
- Every change requires CTO approval
- 24/7 on-call coverage

**Example:** Budget remaining: 8 minutes / 43.2 minutes (18%)

---

### Level 5: Exhausted (0% budget remaining)

**Status:** SLO violation

**Actions:**
- Formal incident declared
- All stakeholders notified
- Customer communication sent
- Fix-forward only strategy
- Post-mortem mandatory
- Reliability roadmap presented to leadership
- Next month budget allocation reviewed

**Restrictions:**
- Absolute deployment freeze
- Emergency changes only
- All changes paired/reviewed
- Daily executive briefings

**Consequences:**
- Delayed feature releases
- Reliability sprint next iteration
- Possible impact on team bonuses/metrics
- Public status page updates

---

## Burn Rate Alerts

Error budgets can be exhausted quickly. We use burn rate alerts to catch rapid consumption:

### Fast Burn (1-hour window)
- **Threshold:** 14.4x normal burn rate
- **Impact:** Exhausts monthly budget in 2 days
- **Action:** Page on-call engineer immediately
- **Response time:** 15 minutes

### Medium Burn (6-hour window)
- **Threshold:** 6x normal burn rate
- **Impact:** Exhausts monthly budget in 5 days
- **Action:** Create high-priority ticket
- **Response time:** 2 hours

### Slow Burn (24-hour window)
- **Threshold:** 3x normal burn rate
- **Impact:** Exhausts monthly budget in 10 days
- **Action:** Warning alert
- **Response time:** Next business day

## Budget Reallocation

If we consistently exceed SLO targets, we can reallocate error budget:

**Eligibility:**
- Maintained >95% budget remaining for 3 consecutive months
- No P0 incidents in period
- Approved by engineering leadership

**Options:**
- Invest budget in risky feature launches
- Reduce monitoring overhead
- Accelerate deployment cadence

## Reporting and Accountability

### Weekly Reports
- Current error budget status
- Trend analysis
- Top error sources
- Action items

**Recipients:** Engineering team, Product Management

### Monthly Reviews
- SLO compliance summary
- Error budget utilization
- Policy effectiveness
- Improvement recommendations

**Recipients:** Engineering leadership, Executive team

### Quarterly Planning
- Review SLO targets
- Adjust error budgets if needed
- Update policies
- Plan reliability investments

**Participants:** Engineering, Product, Customer Success

## Exceptions

Policy can be overridden in exceptional circumstances:

**Valid Reasons:**
- Security vulnerability requiring immediate patch
- Data loss prevention
- Legal/compliance requirement

**Approval Required:**
- VP Engineering (Levels 1-3)
- CTO (Levels 4-5)

**Documentation:**
- Exception must be logged
- Post-mortem required
- Review in next SLO meeting

## Examples

### Example 1: Slow Degradation
- Week 1: 5% budget consumed (normal)
- Week 2: 15% consumed (warning triggered)
- Week 3: 35% consumed (critical level)
- **Action:** Deployment freeze, identify source, fix root cause

### Example 2: Sudden Incident
- Day 1: 10% budget consumed (normal)
- Day 2: 90% consumed in 4 hours (fast burn alert)
- **Action:** Immediate page, rollback deployment, incident response

### Example 3: Healthy Service
- Month 1: 2% consumed
- Month 2: 3% consumed
- Month 3: 1% consumed
- **Action:** Consider SLO tightening or error budget reallocation

## References

- SLO definitions: `monitoring/slos/slo_definitions.yaml`
- Alert rules: `monitoring/alerts/prometheus_alerts.yaml`
- Incident response: `docs/runbooks/incident-response.md`

---

**Last Updated:** 2024-11-22
**Owner:** Platform Team
**Reviewers:** Engineering Leadership
