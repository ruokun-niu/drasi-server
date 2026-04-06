// Drasi Server — Getting Started on Azure Container Apps
//
// This deploys the complete Getting Started tutorial environment:
//   1. PostgreSQL container with CDC enabled
//   2. Drasi Server connected to PostgreSQL
//
// Deploy:
//   az group create --name drasi-getting-started --location eastus
//   az deployment group create --resource-group drasi-getting-started \
//     --template-file azure/getting-started.bicep
//
// After deployment:
//   1. Initialize the database (see output for commands)
//   2. Follow the tutorial at https://drasi.io/drasi-server/getting-started/

targetScope = 'resourceGroup'

param location string = resourceGroup().location
param containerImage string = 'ghcr.io/ruokun-niu/drasi-server:0.1.12'

// ---------------------------------------------------------------------------
// Database init SQL — creates the Message table and sample data
// ---------------------------------------------------------------------------

var initSql = '''
SET client_min_messages = ERROR;

DO $$
BEGIN
    IF NOT EXISTS (SELECT FROM pg_user WHERE usename = 'drasi_user') THEN
        CREATE USER drasi_user WITH REPLICATION LOGIN PASSWORD 'drasi_password';
    END IF;
END
$$;

GRANT CREATE ON DATABASE getting_started TO drasi_user;
GRANT ALL PRIVILEGES ON DATABASE getting_started TO drasi_user;

DROP TABLE IF EXISTS "Message" CASCADE;

CREATE TABLE "Message" (
    "MessageId" SERIAL PRIMARY KEY,
    "From" VARCHAR(50) NOT NULL,
    "Message" VARCHAR(200) NOT NULL,
    "CreatedAt" TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

ALTER TABLE "Message" REPLICA IDENTITY FULL;
ALTER TABLE "Message" OWNER TO drasi_user;

GRANT USAGE ON SCHEMA public TO drasi_user;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO drasi_user;
GRANT ALL PRIVILEGES ON ALL TABLES IN SCHEMA public TO drasi_user;
GRANT ALL PRIVILEGES ON ALL SEQUENCES IN SCHEMA public TO drasi_user;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_publication WHERE pubname = 'drasi_pub') THEN
        CREATE PUBLICATION drasi_pub FOR TABLE "Message";
    ELSIF NOT EXISTS (
        SELECT 1 FROM pg_publication_tables
        WHERE pubname = 'drasi_pub' AND tablename = 'Message'
    ) THEN
        ALTER PUBLICATION drasi_pub ADD TABLE "Message";
    END IF;
END
$$;

INSERT INTO "Message" ("From", "Message")
SELECT * FROM (VALUES
    ('Buzz Lightyear', 'To infinity and beyond!'),
    ('Brian Kernighan', 'Hello World'),
    ('Antoninus', 'I am Spartacus'),
    ('David', 'I am Spartacus')
) AS data("From", "Message")
WHERE NOT EXISTS (SELECT 1 FROM "Message");

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = 'drasi_slot') THEN
        PERFORM pg_create_logical_replication_slot('drasi_slot', 'pgoutput');
    END IF;
END
$$;

SET client_min_messages = NOTICE;
DO $$
BEGIN
    RAISE NOTICE 'Getting Started database initialized successfully!';
    RAISE NOTICE 'Tables: Message';
    RAISE NOTICE 'Publication: drasi_pub';
    RAISE NOTICE 'Replication slot: drasi_slot';
END
$$;
'''

// ---------------------------------------------------------------------------
// Drasi Server config — matches getting-started-step-3.yaml
// ---------------------------------------------------------------------------

var serverConfig = '''
id: getting-started-aca
host: 0.0.0.0
port: 8080
logLevel: info
persistConfig: false
persistIndex: false
autoInstallPlugins: true
pluginRegistry: ghcr.io/drasi-project
plugins:
- ref: source/postgres
- ref: bootstrap/postgres
- ref: reaction/log
sources:
- kind: postgres
  id: my-postgres
  autoStart: true
  bootstrapProvider:
    kind: postgres
  host: demo-postgres
  port: 5432
  database: getting_started
  user: drasi_user
  password: drasi_password
  tables:
  - Message
  slotName: drasi_slot
  publicationName: drasi_pub
  sslMode: prefer
  tableKeys:
  - table: Message
    keyColumns:
    - MessageId
queries:
- id: all-messages
  autoStart: true
  query: |
    MATCH (m:Message)
    RETURN m.MessageId AS MessageId, m.From AS From, m.Message AS Message
  queryLanguage: GQL
  sources:
  - sourceId: my-postgres
  enableBootstrap: true
  bootstrapBufferSize: 10000
reactions:
- kind: log
  id: log-reaction
  queries:
  - all-messages
  autoStart: true
'''

// ---------------------------------------------------------------------------
// Log Analytics
// ---------------------------------------------------------------------------

resource logAnalytics 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: 'drasi-getting-started-logs'
  location: location
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: 30
  }
}

// ---------------------------------------------------------------------------
// Container Apps Environment
// ---------------------------------------------------------------------------

resource env 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: 'drasi-getting-started-env'
  location: location
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logAnalytics.properties.customerId
        sharedKey: logAnalytics.listKeys().primarySharedKey
      }
    }
  }
}

// ---------------------------------------------------------------------------
// PostgreSQL Container App (CDC enabled)
// ---------------------------------------------------------------------------

resource postgres 'Microsoft.App/containerApps@2024-03-01' = {
  name: 'demo-postgres'
  location: location
  properties: {
    environmentId: env.id
    configuration: {
      ingress: {
        external: false
        targetPort: 5432
        transport: 'tcp'
      }
      secrets: [
        {
          name: 'init-sql'
          value: initSql
        }
      ]
    }
    template: {
      containers: [
        {
          name: 'postgres'
          image: 'postgres:14-alpine'
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          env: [
            { name: 'POSTGRES_DB', value: 'getting_started' }
            { name: 'POSTGRES_USER', value: 'postgres' }
            { name: 'POSTGRES_PASSWORD', value: 'postgres_admin' }
          ]
          args: [
            'postgres'
            '-c'
            'wal_level=logical'
            '-c'
            'max_replication_slots=10'
            '-c'
            'max_wal_senders=10'
          ]
          volumeMounts: [
            {
              volumeName: 'init-scripts'
              mountPath: '/docker-entrypoint-initdb.d'
            }
          ]
        }
      ]
      volumes: [
        {
          name: 'init-scripts'
          storageType: 'Secret'
          secrets: [
            {
              secretRef: 'init-sql'
              path: 'init.sql'
            }
          ]
        }
      ]
      scale: {
        minReplicas: 1
        maxReplicas: 1
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Drasi Server Container App
// ---------------------------------------------------------------------------

resource app 'Microsoft.App/containerApps@2024-03-01' = {
  name: 'drasi-server-getting-started'
  location: location
  properties: {
    environmentId: env.id
    configuration: {
      ingress: {
        external: true
        targetPort: 8080
        transport: 'http'
      }
      secrets: [
        {
          name: 'server-config'
          value: serverConfig
        }
      ]
    }
    template: {
      containers: [
        {
          name: 'drasi-server'
          image: containerImage
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          env: [
            { name: 'RUST_LOG', value: 'info' }
          ]
          // ACA secrets volumes are read-only, but Drasi Server enters read-only mode
          // (blocking API mutations) when the config file is not writable. Copy to a
          // writable path so the REST API can create queries/reactions at runtime.
          command: ['/bin/sh', '-c', 'cp /config-secret/server.yaml /app/config/server.yaml && drasi-server --config /app/config/server.yaml']
          volumeMounts: [
            {
              volumeName: 'config-volume'
              mountPath: '/config-secret'
            }
          ]
          probes: [
            {
              type: 'Liveness'
              httpGet: { path: '/health', port: 8080 }
              initialDelaySeconds: 30
              periodSeconds: 10
            }
            {
              type: 'Readiness'
              httpGet: { path: '/health', port: 8080 }
              initialDelaySeconds: 10
              periodSeconds: 5
            }
          ]
        }
      ]
      volumes: [
        {
          name: 'config-volume'
          storageType: 'Secret'
          secrets: [
            {
              secretRef: 'server-config'
              path: 'server.yaml'
            }
          ]
        }
      ]
      scale: {
        minReplicas: 1
        maxReplicas: 1
      }
    }
  }
  dependsOn: [ postgres ]
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

output drasiServerUrl string = 'https://${app.properties.configuration.ingress.fqdn}'
output swaggerUrl string = 'https://${app.properties.configuration.ingress.fqdn}/api/v1/docs/'
output postgresHost string = 'demo-postgres'
output initDbCommand string = 'az containerapp exec --name demo-postgres --resource-group ${resourceGroup().name} --command -- psql -U postgres -d getting_started'
