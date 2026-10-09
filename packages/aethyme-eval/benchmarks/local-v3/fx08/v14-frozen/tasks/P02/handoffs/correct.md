# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Adapt the primary catalog search for narrow screens without coupling the two search panels.",
  "decisions": [
    {
      "scope_ref": "quick-find",
      "choice": "Keep Quick Find backed by independent query and result state.",
      "reason": "It records recent lookups separately, so clearing the primary query must not erase that history."
    }
  ],
  "preserves": [
    "Quick Find results remain available after the primary search is cleared."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
