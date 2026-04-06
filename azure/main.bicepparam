using 'main.bicep'

param location = 'eastus'
param containerImage = 'ghcr.io/ruokun-niu/drasi-server:ruokun-niu'
param appName = 'drasi-server'
param envName = 'drasi-server-env'

// Override serverConfig inline or via CLI:
//   --parameters serverConfig='<yaml content>'
