# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Preserve catalog keyboard search while improving the header interaction.",
  "decisions": [
    {
      "scope_ref": "search-control",
      "choice": "Rename global-search to header-search during the markup cleanup.",
      "reason": "Only header styling uses that identifier; the keyboard shortcut finds the control by its label."
    }
  ],
  "preserves": [
    "The header search remains visually prominent."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
