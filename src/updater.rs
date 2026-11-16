//! Signed release manifests, bounded HTTPS downloads and verified binary replacement.
use ed25519_dalek::{Signature, VerifyingKey};
use reqwest::blocking::Client;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

pub const REPOSITORY: &str = "https://github.com/SargeMadovsky/poetrhymer";
const LATEST: &str = "https://github.com/SargeMadovsky/poetrhymer/releases/latest/download";
const KEY: &str = include_str!("../release/update-public-key.hex");
const MAX_BINARY: u64 = 100 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent("Poetrhymer-updater")
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .https_only(true)
        .build()
        .map_err(|e| e.to_string())
}

fn fetch(client: &Client, url: &str, max: u64) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    if response.content_length().is_some_and(|n| n > max) {
        return Err("Файл обновления превышает допустимый размер".into());
    }
    let mut bytes = Vec::new();
    response
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("Слишком большой ответ сервера".into());
    }
    Ok(bytes)
}

fn verify_manifest(data: &[u8], signature: &[u8], key: &str) -> Result<Manifest, String> {
    let key: [u8; 32] = hex::decode(key.trim())
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "Некорректный открытый ключ")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|e| e.to_string())?;
    let signature = Signature::from_slice(signature).map_err(|e| e.to_string())?;
    key.verify_strict(data, &signature)
        .map_err(|_| "Цифровая подпись обновления недействительна")?;
    let manifest: Manifest = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    Version::parse(&manifest.version).map_err(|e| e.to_string())?;
    let expected = format!(
        "{REPOSITORY}/releases/download/v{}/Poetrhymer.exe",
        manifest.version
    );
    if manifest.target != "x86_64-pc-windows-msvc"
        || manifest.url != expected
        || manifest.size == 0
        || manifest.size > MAX_BINARY
    {
        return Err("Неподдерживаемый пакет обновления".into());
    }
    if hex::decode(&manifest.sha256)
        .map_err(|e| e.to_string())?
        .len()
        != 32
    {
        return Err("Некорректная контрольная сумма".into());
    }
    Ok(manifest)
}

pub fn check() -> Result<Option<Manifest>, String> {
    if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return Ok(None);
    }
    let client = client()?;
    let data = fetch(&client, &format!("{LATEST}/update.json"), 16384)?;
    let signature = fetch(&client, &format!("{LATEST}/update.json.sig"), 64)?;
    let manifest = verify_manifest(&data, &signature, KEY)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    Ok(
        (Version::parse(&manifest.version).map_err(|e| e.to_string())? > current)
            .then_some(manifest),
    )
}

pub fn install(manifest: &Manifest) -> Result<(), String> {
    // Re-fetch the signed document, so no UI state can substitute an unsigned URL/hash.
    let client = client()?;
    let base = format!("{REPOSITORY}/releases/download/v{}", manifest.version);
    let data = fetch(&client, &format!("{base}/update.json"), 16384)?;
    let signature = fetch(&client, &format!("{base}/update.json.sig"), 64)?;
    let verified = verify_manifest(&data, &signature, KEY)?;
    if verified.version != manifest.version
        || Version::parse(&verified.version).map_err(|e| e.to_string())?
            <= Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
    {
        return Err("Обновление устарело".into());
    }
    let bytes = fetch(&client, &verified.url, verified.size)?;
    verify_binary(&verified, &bytes)?;
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = directory.path().join("Poetrhymer.exe");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    self_replace::self_replace(&path).map_err(|e| format!("Не удалось установить обновление: {e}"))
}

fn verify_binary(manifest: &Manifest, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 != manifest.size || hex::encode(Sha256::digest(bytes)) != manifest.sha256
    {
        return Err("Контрольная сумма обновления не совпала".into());
    }
    if !bytes.starts_with(b"MZ") {
        return Err("Обновление не является Windows-приложением".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn tampering_and_wrong_keys_are_rejected() {
        let signing = SigningKey::from_bytes(&[42; 32]);
        let manifest = Manifest {
            version: "0.1.2".into(),
            target: "x86_64-pc-windows-msvc".into(),
            url: format!("{REPOSITORY}/releases/download/v0.1.2/Poetrhymer.exe"),
            sha256: hex::encode(Sha256::digest(b"MZtest")),
            size: 6,
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let signature = signing.sign(&bytes).to_bytes();
        let key = hex::encode(signing.verifying_key().to_bytes());
        assert!(verify_manifest(&bytes, &signature, &key).is_ok());
        let mut changed = bytes.clone();
        changed[12] ^= 1;
        assert!(verify_manifest(&changed, &signature, &key).is_err());
        assert!(verify_manifest(
            &bytes,
            &signature,
            &hex::encode(SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes())
        )
        .is_err());
        assert!(verify_binary(&manifest, b"MZtest").is_ok());
        assert!(verify_binary(&manifest, b"MZevil").is_err());
    }

    #[test]
    #[ignore = "requires the published public GitHub release"]
    fn published_release_is_signed_and_matches_binary() {
        let client = client().unwrap();
        let data = fetch(&client, &format!("{LATEST}/update.json"), 16384).unwrap();
        let signature = fetch(&client, &format!("{LATEST}/update.json.sig"), 64).unwrap();
        let manifest = verify_manifest(&data, &signature, KEY).unwrap();
        let bytes = fetch(&client, &manifest.url, manifest.size).unwrap();
        verify_binary(&manifest, &bytes).unwrap();
        assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
        assert!(check().unwrap().is_none());
    }
}
