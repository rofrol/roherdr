---
name: mimo
description: Consult Xiaomi MiMo-V2.6-Pro over the user's xiaomi token plan (xiaomi-token-plan-ams), the route the coordinator session runs on, for a second opinion. Used by consult rounds and qflow consultations; the prompt is one argument, the answer goes to stdout and the call is logged through consult-stats.
---

# Ask MiMo over the ams token plan

`ask_mimo_ams.py "PROMPT"` runs one single-shot consultation over
`xiaomi-token-plan-ams` (the user's own token plan) and logs the call for
consult-stats. The model set names it as `mimo` in
`plugins/consult/consult-models.json`.

Why this exists (user, 2026-10-20): consultations must use mimo from the ams
plan, not the paid `xiaomi` route (answers 402, insufficient balance) and not
OpenRouter (a separate balance with its own secret vet).
