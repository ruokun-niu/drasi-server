# Drasi Server Getting Started — Azure Container Apps Demo

## Deploy

```bash
# Deploy the getting-started environment
az deployment group create --resource-group ruokun-dev \
  --template-file azure/getting-started.bicep

# Get the Drasi Server URL and save it
export DRASI=$(az deployment group show --resource-group ruokun-dev --name getting-started \
  --query 'properties.outputs.drasiServerUrl.value' -o tsv)

echo $DRASI
```

## Initialize the Database

```bash
# Open a shell in the PostgreSQL container
az containerapp exec --name demo-postgres --resource-group ruokun-dev

# Inside the shell, run:
psql -U postgres -d getting_started < /docker-entrypoint-initdb.d/init.sql

# Verify sample data
psql -U drasi_user -d getting_started -c 'SELECT * FROM "Message";'

# Exit the shell
exit
```

## Step 3: Verify Drasi Server

```bash
# Health check
curl $DRASI/health

# List all queries
curl -s $DRASI/api/v1/queries | python3 -m json.tool

# View query results (4 messages from bootstrap)
curl -s $DRASI/api/v1/queries/all-messages/results | python3 -m json.tool

# Open Swagger UI in browser
open $DRASI/api/v1/docs/
```

## Step 3: Test CDC (Insert / Update / Delete)

```bash
# Open a shell in PostgreSQL
az containerapp exec --name demo-postgres --resource-group ruokun-dev

# Inside the shell — INSERT
psql -U drasi_user -d getting_started -c "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('You', 'My first message!');"

# UPDATE
psql -U drasi_user -d getting_started -c \
  "UPDATE \"Message\" SET \"Message\" = 'My first UPDATED message!' WHERE \"MessageId\" = 5;"

# DELETE
psql -U drasi_user -d getting_started -c \
  "DELETE FROM \"Message\" WHERE \"MessageId\" = 5;"

# Exit the shell
exit
```

After each change, verify from your local terminal:

```bash
curl -s $DRASI/api/v1/queries/all-messages/results | python3 -m json.tool
```

## Step 4: Add a Filtered Query

```bash
# Create the hello-world-senders query
curl -X POST $DRASI/api/v1/queries \
  -H "Content-Type: application/json" \
  -d '{
    "id": "hello-world-senders",
    "autoStart": true,
    "sources": [{"sourceId": "my-postgres"}],
    "query": "MATCH (m:Message) WHERE m.Message = '\''Hello World'\'' RETURN m.MessageId AS Id, m.From AS Sender",
    "queryLanguage": "Cypher"
  }'

# Update log-reaction to subscribe to both queries
curl -X DELETE $DRASI/api/v1/reactions/log-reaction

curl -X POST $DRASI/api/v1/reactions \
  -H "Content-Type: application/json" \
  -d '{
    "kind": "log",
    "id": "log-reaction",
    "queries": ["all-messages", "hello-world-senders"],
    "autoStart": true
  }'

# Check hello-world-senders results (should include Brian Kernighan)
curl -s $DRASI/api/v1/queries/hello-world-senders/results | python3 -m json.tool

# List all queries (should show all-messages and hello-world-senders)
curl -s $DRASI/api/v1/queries | python3 -m json.tool

# List all reactions (should show log-reaction subscribed to both queries)
curl -s $DRASI/api/v1/reactions | python3 -m json.tool
```

Test with inserts (run inside `az containerapp exec`):

```bash
# Matches both queries
psql -U drasi_user -d getting_started -c \
  "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('Alice', 'Hello World');"

# Matches only all-messages
psql -U drasi_user -d getting_started -c \
  "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('Bob', 'Goodbye World');"
```

## Step 5: Aggregation Query

```bash
# Create the message-counts query
curl -X POST $DRASI/api/v1/queries \
  -H "Content-Type: application/json" \
  -d '{
    "id": "message-counts",
    "autoStart": true,
    "sources": [{"sourceId": "my-postgres"}],
    "query": "MATCH (m:Message) RETURN m.Message AS MessageText, count(m) AS Count",
    "queryLanguage": "Cypher"
  }'

# Check aggregation results
curl -s $DRASI/api/v1/queries/message-counts/results | python3 -m json.tool

# Update log-reaction to also subscribe to message-counts
curl -X DELETE $DRASI/api/v1/reactions/log-reaction

curl -X POST $DRASI/api/v1/reactions \
  -H "Content-Type: application/json" \
  -d '{
    "kind": "log",
    "id": "log-reaction",
    "queries": ["all-messages", "hello-world-senders", "message-counts"],
    "autoStart": true
  }'
```

Test (run inside `az containerapp exec`):

```bash
# Insert — count for "Hello World" increases
psql -U drasi_user -d getting_started -c "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('Eve', 'Hello World');"

# Delete — count decreases
psql -U drasi_user -d getting_started -c \
  "DELETE FROM \"Message\" WHERE \"From\" = 'Eve';"
```

## Step 6: Time-Based Detection

```bash
# Create the inactive-senders query
curl -X POST $DRASI/api/v1/queries \
  -H "Content-Type: application/json" \
  -d '{
    "id": "inactive-senders",
    "autoStart": true,
    "sources": [{"sourceId": "my-postgres"}],
    "query": "MATCH (m:Message) WITH m.From AS MessageFrom, max(drasi.changeDateTime(m)) AS LastMessageTimestamp WHERE LastMessageTimestamp <= datetime.realtime() - duration({ seconds: 20 }) OR drasi.trueLater(LastMessageTimestamp <= datetime.realtime() - duration({ seconds: 20 }), LastMessageTimestamp + duration({ seconds: 20 })) RETURN MessageFrom, LastMessageTimestamp",
    "queryLanguage": "Cypher"
  }'
```

Test (run inserts inside `az containerapp exec`):

```bash
# Insert a message from Alice
psql -U drasi_user -d getting_started -c \
  "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('Alice', 'About to go inactive');"
```

Wait 20 seconds, then check:

```bash
# Alice should appear as inactive
curl -s $DRASI/api/v1/queries/inactive-senders/results | python3 -m json.tool
```

Make Alice active again (inside `az containerapp exec`):

```bash
psql -U drasi_user -d getting_started -c \
  "INSERT INTO \"Message\" (\"From\", \"Message\") VALUES ('Alice', 'Active again');"
```

## View Server Logs

```bash
az containerapp logs show --name drasi-server-getting-started \
  --resource-group ruokun-dev --follow
```

## Cleanup

```bash
az containerapp delete --name drasi-server-getting-started --resource-group ruokun-dev --yes
az containerapp delete --name demo-postgres --resource-group ruokun-dev --yes
az containerapp env delete --name drasi-getting-started-env --resource-group ruokun-dev --yes
```
