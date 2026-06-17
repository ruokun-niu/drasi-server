## Version Sync

Package version: `0.2.0` → `0.8.7`

### Changed dependency requirements

```diff
-drasi-lib = { version = "0.8.4", features = [
+drasi-lib = { version = "0.8.7", features = [
-drasi-core = "0.5.3"
+drasi-core = "0.5.5"
-drasi-bootstrap-noop = "0.2.5"
-drasi-bootstrap-application = "0.2.5"
+drasi-bootstrap-noop = "0.2.8"
+drasi-bootstrap-application = "0.2.8"
-drasi-reaction-application = "0.3.3"
+drasi-reaction-application = "0.3.6"
-drasi-index-rocksdb = "0.5.4"
+drasi-index-rocksdb = "0.5.6"
-drasi-state-store-redb = "0.2.1"
+drasi-state-store-redb = "0.2.2"
-drasi-plugin-sdk = "0.8.4"
+drasi-plugin-sdk = "0.9.0"
-drasi-host-sdk = { version = "0.8.4", features = ["registry", "fetcher", "watcher"] }
+drasi-host-sdk = { version = "0.9.0", features = ["registry", "fetcher", "watcher"] }
-drasi-core = "0.5.3"
+drasi-core = "0.5.5"
```

Merging this PR updates the version read by the **Build and Release** workflow.
After merge, run that workflow to publish the release.
