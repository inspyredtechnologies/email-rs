//! Local persistence: account list, per-folder header cache, message bodies,
//! and passwords (Windows Credential Manager via `keyring`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{AccountConfig, Folder, MessageMeta};

const KEYRING_SERVICE: &str = "outlook-rs";

pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "OutlookRs")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("OutlookRs-data"))
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Write to a temp file then rename so a crash never leaves a half-written cache.
    let tmp = path.with_extension("tmp");
    if let Ok(json) = serde_json::to_vec(value) {
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, path);
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub fn load_accounts() -> Vec<AccountConfig> {
    read_json(&data_dir().join("accounts.json")).unwrap_or_default()
}

pub fn save_accounts(accounts: &[AccountConfig]) {
    write_json(&data_dir().join("accounts.json"), &accounts);
}

pub fn set_password(account_id: &str, password: &str) -> Result<(), String> {
    keyring::Entry::new(KEYRING_SERVICE, account_id)
        .and_then(|e| e.set_password(password))
        .map_err(|e| format!("Could not save the password: {e}"))
}

pub fn get_password(account_id: &str) -> Result<String, String> {
    keyring::Entry::new(KEYRING_SERVICE, account_id)
        .and_then(|e| e.get_password())
        .map_err(|e| format!("No saved password: {e}"))
}

pub fn delete_password(account_id: &str) {
    if let Ok(e) = keyring::Entry::new(KEYRING_SERVICE, account_id) {
        let _ = e.delete_credential();
    }
}

fn account_dir(account_id: &str) -> PathBuf {
    data_dir().join("cache").join(account_id)
}

/// Remove an account's cache and password.
pub fn forget_account(account_id: &str) {
    let _ = fs::remove_dir_all(account_dir(account_id));
    delete_password(account_id);
}

/// File-system-safe name for a mailbox path.
fn folder_key(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>()
        + &format!("-{:08x}", fnv(path.as_bytes()))
}

fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c9dc5u32, |h, b| (h ^ *b as u32).wrapping_mul(0x01000193))
}

pub fn load_folders(account_id: &str) -> Vec<Folder> {
    read_json(&account_dir(account_id).join("folders.json")).unwrap_or_default()
}

pub fn save_folders(account_id: &str, folders: &[Folder]) {
    write_json(&account_dir(account_id).join("folders.json"), &folders);
}

/// Cached header list for one folder, tied to the server's UIDVALIDITY.
#[derive(Default, Serialize, Deserialize)]
pub struct FolderCache {
    pub uid_validity: u32,
    pub messages: Vec<MessageMeta>,
}

pub fn load_folder_cache(account_id: &str, folder: &str) -> FolderCache {
    read_json(&account_dir(account_id).join(format!("{}.json", folder_key(folder)))).unwrap_or_default()
}

pub fn save_folder_cache(account_id: &str, folder: &str, cache: &FolderCache) {
    write_json(&account_dir(account_id).join(format!("{}.json", folder_key(folder))), cache);
}

fn body_path(account_id: &str, folder: &str, uid_validity: u32, uid: u32) -> PathBuf {
    account_dir(account_id)
        .join("bodies")
        .join(format!("{}-{uid_validity}-{uid}.eml", folder_key(folder)))
}

pub fn load_body(account_id: &str, folder: &str, uid_validity: u32, uid: u32) -> Option<Vec<u8>> {
    fs::read(body_path(account_id, folder, uid_validity, uid)).ok()
}

pub fn save_body(account_id: &str, folder: &str, uid_validity: u32, uid: u32, raw: &[u8]) {
    let path = body_path(account_id, folder, uid_validity, uid);
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, raw);
}

pub fn delete_body(account_id: &str, folder: &str, uid_validity: u32, uid: u32) {
    let _ = fs::remove_file(body_path(account_id, folder, uid_validity, uid));
}
