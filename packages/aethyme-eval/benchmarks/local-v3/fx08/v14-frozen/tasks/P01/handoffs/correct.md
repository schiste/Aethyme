# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Preserve catalog keyboard search while improving the header interaction.",
  "decisions": [
    {
      "scope_ref": "search-control",
      "choice": "Keep the search input id as global-search.",
      "reason": "The page-wide Ctrl+K shortcut targets that identifier to focus catalog search."
    }
  ],
  "preserves": [
    "The keyboard shortcut still focuses the catalog search."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
