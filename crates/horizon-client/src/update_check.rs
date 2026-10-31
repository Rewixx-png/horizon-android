//! GitHub release discovery for the Android launcher's in-app updater.
//! Network and installer work stay in the Android module; this part is pure so
//! version ordering and asset selection can be tested on the host.

use serde_json::Value;

pub const REPOSITORY: &str = "Rewixx-png/horizon-android";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub apk_name: String,
    pub apk_url: String,
    pub apk_size: u64,
    pub sums_url: Option<String>,
}

/// Parse GitHub's latest-release response and return an update only when its
/// version is newer than `current`. `abi` selects the matching APK asset.
pub fn latest(json: &str, abi: &str, current: &str) -> Result<Option<Release>, String> {
    let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if let Some(message) = root["message"].as_str() {
        return Err(message.to_owned());
    }
    let tag = root["tag_name"]
        .as_str()
        .ok_or("Release has no tag_name")?
        .to_owned();
    let version = tag.strip_prefix('v').unwrap_or(&tag).to_owned();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let assets = root["assets"].as_array().ok_or("Release has no assets")?;
    let wanted = format!("horizon-android-{abi}.apk");
    let apk = assets
        .iter()
        .find(|asset| asset["name"].as_str() == Some(wanted.as_str()))
        .ok_or_else(|| format!("Release has no {wanted}"))?;
    let apk_url = apk["browser_download_url"]
        .as_str()
        .ok_or("APK asset has no download URL")?
        .to_owned();
    let apk_size = apk["size"].as_u64().ok_or("APK asset has no size")?;
    let sums_url = assets
        .iter()
        .find(|asset| asset["name"].as_str() == Some("SHA256SUMS.txt"))
        .and_then(|asset| asset["browser_download_url"].as_str())
        .map(str::to_owned);
    Ok(Some(Release {
        tag,
        version,
        apk_name: wanted,
        apk_url,
        apk_size,
        sums_url,
    }))
}

/// Numeric components in order: `v0.7.6-ui.2` and `0.7.6-ui.2` both become
/// `[0, 7, 6, 2]`. Comparing component-wise keeps UI suffixes and release
/// candidates ordered without a semver dependency.
pub fn version_key(text: &str) -> Vec<u64> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let mut candidate = version_key(candidate);
    let mut current = version_key(current);
    let length = candidate.len().max(current.len());
    candidate.resize(length, 0);
    current.resize(length, 0);
    candidate > current
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str) -> String {
        json!({
            "tag_name": tag,
            "assets": [
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://example.test/sums", "size": 96},
                {"name": "horizon-android-arm64-v8a.apk", "browser_download_url": "https://example.test/apk", "size": 42},
                {"name": "horizon-android-x86_64.apk", "browser_download_url": "https://example.test/x86", "size": 43}
            ]
        })
        .to_string()
    }

    #[test]
    fn orders_ui_suffixes_numerically() {
        assert!(is_newer("0.7.6-ui.2", "0.7.6-ui.1"));
        assert!(!is_newer("0.7.6-ui.1", "0.7.6-ui.1"));
        assert!(!is_newer("0.7.5-ui.9", "0.7.6-ui.1"));
        assert!(is_newer("v0.8.0", "0.7.6-ui.1"));
        assert!(!is_newer("v0.7.6", "0.7.6-ui.1"));
    }

    #[test]
    fn selects_only_the_requested_abi() {
        let update = latest(&release("v0.7.6-ui.2"), "arm64-v8a", "0.7.6-ui.1")
            .unwrap()
            .unwrap();
        assert_eq!(update.version, "0.7.6-ui.2");
        assert_eq!(update.apk_name, "horizon-android-arm64-v8a.apk");
        assert_eq!(update.apk_url, "https://example.test/apk");
        assert_eq!(update.apk_size, 42);
        assert_eq!(
            update.sums_url.as_deref(),
            Some("https://example.test/sums")
        );
        assert!(latest(&release("v0.7.6-ui.2"), "riscv64", "0.7.6-ui.1").is_err());
    }

    #[test]
    fn same_or_older_release_is_not_offered() {
        assert!(
            latest(&release("v0.7.6-ui.1"), "arm64-v8a", "0.7.6-ui.1")
                .unwrap()
                .is_none()
        );
        assert!(
            latest(&release("v0.7.5"), "arm64-v8a", "0.7.6-ui.1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn github_error_message_is_reported() {
        let error = latest(r#"{"message":"Not Found"}"#, "arm64-v8a", "0.7.6-ui.1").unwrap_err();
        assert_eq!(error, "Not Found");
    }
}
