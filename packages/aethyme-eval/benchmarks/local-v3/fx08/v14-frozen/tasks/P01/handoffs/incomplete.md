# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Preserve catalog keyboard search while improving the header interaction.",
  "decisions": [
    {
      "scope_ref": "search-control",
      "choice": "Keep the visible search label clear and the magnifier decorative.",
      "reason": "The header has one search field and a visible Search action."
    }
  ],
  "preserves": [
    "The search remains easy to identify in the header."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
