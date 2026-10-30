// The configuration directory of a service, readable and writable over HTTP.
//
// - `files`: which files make up a configuration directory
// - `store`: the directory as something a running process can stage a change
//   in, validate whole, and commit file by file with atomic renames
// - `api`: the logic of the /api/v1/config routes, over a `ConfigHost` the
//   service implements (what a reload does is the service's business)

pub mod api;
pub mod files;
pub mod store;

pub use api::{
    ConfigBundle, ConfigFileInfo, ConfigHost, ConfigInventory, ConfigMetric, ConfigRejected,
    DeltaInfo, Pending, ReloadQuery, ValidationResult, WriteResult,
};
pub use files::{REQUIRED_CONFIG_FILE, config_files, is_config_path, is_safe_id};
pub use store::{
    ConfigChange, ConfigErrors, ConfigFileStat, ConfigStore, FsConfigStore, Loader, directory_etag,
    sha256_hex,
};
