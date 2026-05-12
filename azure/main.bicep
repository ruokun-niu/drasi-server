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
persistConfig: true
persistIndex: true
verifyPlugins: false
stateStore:
  kind: redb
  path: /drasi-persist/state.redb
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
// Storage Account + File Share for /drasi-persist (durability)
// ---------------------------------------------------------------------------

var storageAccountName = take(toLower('${replace(appName, '-', '')}sa${uniqueString(resourceGroup().id)}'), 24)
var fileShareName = 'drasi-persist'

resource storage 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: storageAccountName
  location: location
  sku: { name: 'Standard_LRS' }
  kind: 'StorageV2'
  properties: {
    minimumTlsVersion: 'TLS1_2'
    allowBlobPublicAccess: false
    allowSharedKeyAccess: true
  }
}

resource fileService 'Microsoft.Storage/storageAccounts/fileServices@2023-05-01' = {
  parent: storage
  name: 'default'
}

resource share 'Microsoft.Storage/storageAccounts/fileServices/shares@2023-05-01' = {
  parent: fileService
  name: fileShareName
  properties: {
    shareQuota: 5
  }
}

resource envStorage 'Microsoft.App/managedEnvironments/storages@2024-03-01' = {
  parent: env
  name: 'drasi-persist'
  properties: {
    azureFile: {
      accountName: storage.name
      accountKey: storage.listKeys().keys[0].value
      shareName: fileShareName
      accessMode: 'ReadWrite'
    }
  }
  dependsOn: [
    share
  ]
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
          // Seed /drasi-persist/server.yaml from the secret on first boot, then
          // run Drasi against the persisted copy so API mutations and redb state
          // survive revision restarts via the Azure Files-backed volume.
          command: ['/bin/sh', '-c', 'if [ ! -f /drasi-persist/server.yaml ]; then cp /config-secret/server.yaml /drasi-persist/server.yaml; fi && mkdir -p /drasi-persist/plugins && exec drasi-server --config /drasi-persist/server.yaml --plugins-dir /drasi-persist/plugins']
          volumeMounts: [
            {
              volumeName: 'config-volume'
              mountPath: '/config-secret'
            }
            {
              volumeName: 'drasi-persist'
              mountPath: '/drasi-persist'
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
        {
          name: 'drasi-persist'
          storageType: 'AzureFile'
          storageName: envStorage.name
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
