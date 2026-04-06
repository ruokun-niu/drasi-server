# Deploying Drasi Server to Azure Container Apps

Deploy Drasi Server to [Azure Container Apps](https://learn.microsoft.com/azure/container-apps/) using the Bicep templates in this directory.

## Prerequisites

- [Azure CLI](https://learn.microsoft.com/cli/azure/install-azure-cli) (v2.60+)
- An Azure subscription

## Quick Start

```bash
# 1. Create a resource group
az group create --name drasi-rg --location eastus

# 2. Deploy
az deployment group create --resource-group drasi-rg \
  --template-file azure/main.bicep \
  --parameters azure/main.bicepparam

# 3. Get the server URL
az deployment group show --resource-group drasi-rg --name main \
  --query 'properties.outputs.url.value' -o tsv
```

## What Gets Deployed

| Resource | Purpose |
|----------|---------|
| Log Analytics Workspace | Container logs and monitoring |
| Container Apps Environment | Shared hosting and networking |
| Container App | Drasi Server instance |

## Configuration

### Server Config

The server config is defined inline in `main.bicep` as the `serverConfig` parameter. Edit it directly or override via CLI:

```bash
az deployment group create --resource-group drasi-rg \
  --template-file azure/main.bicep \
  --parameters azure/main.bicepparam \
  --parameters serverConfig='
host: 0.0.0.0
port: 8080
logLevel: info
persistConfig: false
sources:
- kind: postgres
  id: my-db
  autoStart: true
  host: my-database.postgres.database.azure.com
  port: 5432
  database: mydb
  user: myuser
  password: mypassword
queries:
- id: my-query
  autoStart: true
  query: "MATCH (n) RETURN n"
  queryLanguage: GQL
  sources:
  - sourceId: my-db
reactions: []
'
```

The config is mounted as a secret and copied to a writable path at startup so the REST API can create and modify components at runtime.

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `location` | Resource group location | Azure region |
| `containerImage` | `ghcr.io/drasi-project/drasi-server:latest` | Container image |
| `appName` | `drasi-server` | Name for the container app and related resources |
| `envName` | `drasi-server-env` | Name for the Container Apps Environment |
| `serverConfig` | Minimal empty config | YAML config for Drasi Server |

### Custom Image

To use a private registry or custom build:

```bash
az deployment group create --resource-group drasi-rg \
  --template-file azure/main.bicep \
  --parameters containerImage='myregistry.azurecr.io/drasi-server:v1.0'
```

## Managing the Deployment

```bash
# View logs
az containerapp logs show --name drasi-server --resource-group drasi-rg --follow

# Health check
curl https://<fqdn>/health

# Swagger UI
open https://<fqdn>/api/v1/docs/

# List queries
curl -s https://<fqdn>/api/v1/queries | python3 -m json.tool

# Create a query via REST API
curl -X POST https://<fqdn>/api/v1/queries \
  -H "Content-Type: application/json" \
  -d '{
    "id": "my-query",
    "autoStart": true,
    "sources": [{"sourceId": "my-source"}],
    "query": "MATCH (n) RETURN n",
    "queryLanguage": "GQL"
  }'

# Force a new revision (e.g., after pushing a new image with the same tag)
az containerapp revision copy --name drasi-server --resource-group drasi-rg
```

## Updating

To update the config or image, re-run the deployment:

```bash
az deployment group create --resource-group drasi-rg \
  --template-file azure/main.bicep \
  --parameters azure/main.bicepparam
```

ACA creates a new revision automatically when secrets or container settings change.

**Note:** ACA caches images by tag. If you push a new image with the same tag, force a new revision:

```bash
az containerapp revision copy --name drasi-server --resource-group drasi-rg
```

Or use unique tags (e.g., commit SHAs or version numbers) to avoid caching issues.

## Getting Started Tutorial

To run the [Getting Started tutorial](https://drasi.io/drasi-server/getting-started/) on ACA, use the dedicated template:

```bash
az deployment group create --resource-group drasi-rg \
  --template-file azure/getting-started.bicep
```

This deploys Drasi Server alongside a PostgreSQL container with CDC enabled and sample data pre-loaded. See [GETTING-STARTED-DEMO.md](GETTING-STARTED-DEMO.md) for step-by-step instructions.

## Cleanup

```bash
# Delete individual resources
az containerapp delete --name drasi-server --resource-group drasi-rg --yes
az containerapp env delete --name drasi-server-env --resource-group drasi-rg --yes

# Or delete the entire resource group
az group delete --name drasi-rg --yes
```
