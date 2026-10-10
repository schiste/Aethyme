# Handoff

A previous contributor left a short decision brief for the next change. Read it before editing.

## Decision brief

```json
{
  "intent": "Add a quantity-field shortcut while preserving the form's input constraints.",
  "decisions": [
    {
      "scope_ref": "form-validity",
      "choice": "Submit the quantity form through its native submit path.",
      "reason": "The required, minimum, and maximum checks prevent invalid cart updates."
    }
  ],
  "preserves": [
    "Invalid quantities never change the cart count."
  ],
  "assumptions": [
    "Only this catalog view is mounted in the page."
  ]
}
```
