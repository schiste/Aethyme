# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Add keyboard selection without breaking the downstream order summary.",
  "decisions": [
    {
      "scope_ref": "selected-plan",
      "choice": "Remove data-selected-plan and use aria-selected only.",
      "reason": "The summary reads the button text directly, so the data attribute is redundant."
    }
  ],
  "preserves": [
    "The selected plan remains exposed to assistive technology."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
