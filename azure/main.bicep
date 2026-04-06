// Drasi Server — Minimal Azure Container Apps deployment
//
// Deploy:
//   az group create --name drasi-rg --location eastus
//   az deployment group create --resource-group drasi-rg --template-file azure/main.bicep

targetScope = 'resourceGroup'

param location string = resourceGroup().location
param containerImage string = 'ghcr.io/ruokun-niu/drasi-server:0.1.12'

// ---------------------------------------------------------------------------
// Drasi Server config — edit this inline
// ---------------------------------------------------------------------------

var serverConfig = '''
id: 68a5d185-a329-4de7-934d-4693cca1d07a
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
  routes: {}
'''

// ---------------------------------------------------------------------------
// Log Analytics
// ---------------------------------------------------------------------------

resource logAnalytics 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: 'drasi-server-logs'
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
  name: 'drasi-server-test-env'
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
  name: 'drasi-server'
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
          name: 'drasi-server-test'
          image: containerImage
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          env: [
            { name: 'RUST_LOG', value: 'info' }
          ]
          args: ['--config', '/config/server.yaml']
          volumeMounts: [
            {
              volumeName: 'config-volume'
              mountPath: '/config'
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
