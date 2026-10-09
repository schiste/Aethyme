# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Add keyboard selection without breaking the downstream order summary.",
  "decisions": [
    {
      "scope_ref": "selected-plan",
      "choice": "Expose a stable accessible active option and keep the order summary readable.",
      "reason": "Customers need to understand both the highlighted plan and its price."
    }
  ],
  "preserves": [
    "The current plan is announced to assistive technology."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
