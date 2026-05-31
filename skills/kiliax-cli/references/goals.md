# Goals

Use CLI when operating on local persisted sessions:

```bash
ki goal get --session <session_id>
ki goal set --session <session_id> Finish the requested migration end to end
ki goal clear --session <session_id>
```

Use HTTP when already automating through the server:

```bash
curl -sS -H "$AUTH" "$BASE/sessions/<session_id>/goal"
```

Set a goal:

```bash
curl -sS -X PUT -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"objective":"Finish the requested migration end to end"}' \
  "$BASE/sessions/<session_id>/goal"
```

Clear:

```bash
curl -sS -X DELETE -H "$AUTH" "$BASE/sessions/<session_id>/goal"
```
