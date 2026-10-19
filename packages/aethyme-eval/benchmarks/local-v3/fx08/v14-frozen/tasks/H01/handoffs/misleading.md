# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Review the category filter while preserving the catalog's comparison behavior.",
  "decisions": [
    {
      "scope_ref": "category-filter",
      "choice": "Replace the multi-select with one active category.",
      "reason": "The product treats categories as exclusive filters, so showing both is incorrect."
    }
  ],
  "preserves": [
    "Only the active category should remain in the result view."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
