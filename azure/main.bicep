// Drasi Server — Azure Container Apps deployment
//
// Deploy:
//   az group create --name drasi-rg --location eastus
//   az deployment group create --resource-group drasi-rg --template-file azure/main.bicep \
//     --parameters azure/main.bicepparam

targetScope = 'resourceGroup'

param location string = resourceGroup().location
param containerImage string = 'ghcr.io/ruokun-niu/drasi-server:0.1.12'
param appName string = 'drasi-server'
param envName string = 'drasi-server-env'

// ---------------------------------------------------------------------------
// Drasi Server config — edit this inline or override via parameter
// ---------------------------------------------------------------------------

param serverConfig string = '''
host: 0.0.0.0
port: 8080
logLevel: info
persistConfig: false
persistIndex: false
autoInstallPlugins: true
pluginRegistry: ghcr.io/drasi-project
plugins:
- ref: source/mock
- ref: reaction/log
sources:
- kind: mock
  id: mock-source
  autoStart: true
  dataType:
    sensorCount: 5
    type: sensorReading
  intervalMs: 5000
queries:
- id: my-query
  autoStart: true
  query: MATCH (n) RETURN n
  queryLanguage: GQL
  sources:
  - sourceId: mock-source
  enableBootstrap: true
  bootstrapBufferSize: 10000
reactions:
- kind: log
  id: log-reaction
  queries:
  - my-query
  autoStart: true
'''

// ---------------------------------------------------------------------------
// Log Analytics
// ---------------------------------------------------------------------------

resource logAnalytics 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: '${appName}-logs'
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
  name: envName
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
// Container App
// ---------------------------------------------------------------------------

resource app 'Microsoft.App/containerApps@2024-03-01' = {
  name: appName
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
          // ACA secrets volumes are read-only, but Drasi Server enters read-only
          // mode (blocking API mutations) when the config file is not writable.
          // Copy to a writable path so the REST API can modify config at runtime.
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
}

output url string = 'https://${app.properties.configuration.ingress.fqdn}'
output docsUrl string = 'https://${app.properties.configuration.ingress.fqdn}/api/v1/docs/'
