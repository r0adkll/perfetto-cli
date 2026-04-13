# Add Cloud Provider

Add a new cloud storage provider to perfetto-cli's upload system.

## Input

The argument `$ARGUMENTS` is the name of the provider to add (e.g., "S3", "Dropbox", "Azure Blob").

## Instructions

You are adding a new cloud storage provider to perfetto-cli. The provider system is trait-based and extensible. Follow these steps precisely:

### 1. Understand the existing system

Read these files to understand the current architecture:
- `src/cloud/mod.rs` — the `CloudProvider` trait definition, `all_providers()` registry, and helper types (`UploadProgress`, `FileUploadResult`, `UploadResult`)
- `src/cloud/google_drive.rs` — the reference implementation (Google Drive provider)
- `src/cloud/oauth.rs` — shared OAuth2 helpers (PKCE, token exchange, token refresh, local redirect listener)
- `build.rs` — compile-time env var injection for OAuth credentials
- `src/tui/screens/cloud_providers.rs` — the provider management UI

### 2. Ask clarifying questions

Before writing any code, ask the user:
- **Auth method**: Does this provider use OAuth2 (reuse `oauth.rs` helpers) or a different auth mechanism (API keys, etc.)?
- **Upload API**: Does it support resumable/chunked uploads? What's the API endpoint structure?
- **Folder concept**: Does the provider have folder hierarchy (like Drive) or flat key-based storage (like S3 buckets/prefixes)?
- **Compile-time credentials**: Are there OAuth client ID/secret or similar credentials that should be injected at compile time via `build.rs`?

### 3. Create the provider file

Create `src/cloud/<provider_id>.rs` implementing the `CloudProvider` trait:

```rust
#[async_trait]
pub trait CloudProvider: Send + Sync {
    fn name(&self) -> &str;                    // Human-readable: "Amazon S3"
    fn id(&self) -> &str;                      // Settings key: "amazon_s3"
    async fn is_authenticated(&self, db: &Database) -> bool;
    async fn authenticate(&self, db: &Database) -> Result<()>;
    async fn logout(&self, db: &Database) -> Result<()>;
    async fn upload_file(&self, db: &Database, local_path: &Path, remote_folder: &str, progress_tx: &UnboundedSender<UploadProgress>, cancel: &Cancel) -> Result<FileUploadResult>;
    async fn folder_url(&self, db: &Database, remote_folder: &str) -> Result<Option<String>>;
    fn upload_folder(&self, db: &Database) -> String;
    fn folder_settings_key(&self) -> String;
}
```

Key conventions:
- Store OAuth/auth tokens in the `settings` table using keys like `cloud.<provider_id>.<field>`
- Use `oauth::ensure_valid_token()` for OAuth providers, or implement custom auth
- Upload in chunks (~5MB) and report progress via `progress_tx.send(UploadProgress { ... })`
- Check `cancel.is_cancelled()` between chunks
- Return `FileUploadResult { remote_url: Some("https://...") }` with a shareable link

### 4. Register the provider

In `src/cloud/mod.rs`:
- Add `pub mod <provider_id>;` at the top
- Add the provider to `all_providers()`:
  ```rust
  pub fn all_providers() -> Vec<Arc<dyn CloudProvider>> {
      vec![
          Arc::new(google_drive::GoogleDriveProvider),
          Arc::new(<provider_id>::<ProviderStruct>),
      ]
  }
  ```

### 5. Add compile-time credentials (if needed)

If the provider uses OAuth or embedded credentials:
- Add env var constants in the provider file using `env!("PERFETTO_<PROVIDER>_CLIENT_ID")`
- Add the env var names to the `REQUIRED` array in `build.rs`
- Add the env vars to `.github/workflows/release.yml` in the `build-local-artifacts` job's `env:` block
- Remind the user to add values to their `.env` file and GitHub repo secrets

### 6. Add dependencies (if needed)

If the provider needs new crates (unlikely — `reqwest` is already available), add them to `Cargo.toml`.

### 7. Verify

Run `cargo build` to ensure everything compiles. Run `cargo test` to check existing tests still pass.

### 8. Summary

Report what was created:
- The new provider file path
- Any new dependencies added
- Any new env vars that need to be set
- How to test: open the TUI, press `[p]` from sessions list, see the new provider, press `[l]` to login
