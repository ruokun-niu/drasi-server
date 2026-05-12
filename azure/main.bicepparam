using 'main.bicep'

param location = 'eastus'
param containerImage = 'ghcr.io/drasi-project/drasi-server:0.1.4'
param appName = 'drasi-server'
param envName = 'drasi-server-env2'

// Override serverConfig inline or via CLI:
//   --parameters serverConfig='<yaml content>'
