use anyhow::{Error, anyhow};
use nostr_sdk::prelude::*;

/// The storage identifier for the device encryption key
const IDENTIFIER: &str = "coop:device";
/// The storage identifier for encryption keys retired by a rotation
const ARCHIVE_IDENTIFIER: &str = "coop:device-archive";
/// Keep the archived key list bounded
const ARCHIVE_LIMIT: usize = 10;
/// Seconds in a day, for rotation intervals
pub(crate) const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Device encryption key material with its creation time
#[derive(Debug, Clone)]
pub(crate) struct DeviceKeys {
    pub(crate) keys: Keys,
    pub(crate) created_at: Timestamp,
}

/// Encode the stored key content: secret hex and creation time
fn encode_keys_content(secret: &str, created_at: &Timestamp) -> String {
    format!("{secret}\n{}", created_at.as_secs())
}

/// Parse stored key content.
fn parse_keys_content(content: &str) -> Result<(SecretKey, Timestamp), Error> {
    match content.split_once('\n') {
        Some((secret, created_at)) => {
            let created_at = created_at
                .trim()
                .parse::<u64>()
                .unwrap_or_else(|_| Timestamp::now().as_secs());
            Ok((
                SecretKey::from_hex(secret)?,
                Timestamp::from_secs(created_at),
            ))
        }
        None => Ok((SecretKey::from_hex(content)?, Timestamp::now())),
    }
}

/// Parse archived secrets from stored content, one hex secret per line
fn parse_archived_keys(content: &str) -> Vec<Keys> {
    content
        .lines()
        .filter_map(|line| SecretKey::from_hex(line.trim()).ok())
        .map(Keys::new)
        .collect()
}

/// Load the device keys with their creation time from the local database.
pub(crate) async fn load_device_keys<S>(client: &Client, signer: &S) -> Result<DeviceKeys, Error>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(IDENTIFIER)
        .author(public_key);

    let Some(event) = client.database().query(filter).await?.into_iter().next() else {
        return Err(anyhow!("Key not found"));
    };

    let content = signer
        .nip44_decrypt_async(&public_key, &event.content)
        .await?;
    let (secret, created_at) = parse_keys_content(&content)?;

    Ok(DeviceKeys {
        keys: Keys::new(secret),
        created_at,
    })
}

/// Encrypt and store device keys in the local database.
pub(crate) async fn set_keys<S>(
    client: &Client,
    signer: &S,
    secret: &str,
    created_at: Timestamp,
) -> Result<(), Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;
    let content = signer
        .nip44_encrypt_async(&public_key, &encode_keys_content(secret, &created_at))
        .await?;

    // Construct the application data event
    let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
        .tag(Tag::identifier(IDENTIFIER))
        .finalize_async(signer)
        .await?;

    // Save the event to the database
    client.database().save_event(&event).await?;

    Ok(())
}

/// Append an encryption secret to the bounded archive of retired keys.
pub(crate) async fn archive_key<S>(client: &Client, signer: &S, secret: &str) -> Result<(), Error>
where
    S: AsyncGetPublicKey + AsyncSignEvent + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(ARCHIVE_IDENTIFIER)
        .author(public_key);

    let mut archived: Vec<String> = match client.database().query(filter).await?.into_iter().next()
    {
        Some(event) => {
            let content = signer
                .nip44_decrypt_async(&public_key, &event.content)
                .await?;
            content.lines().map(str::to_string).collect()
        }
        None => vec![],
    };

    archived.insert(0, secret.to_string());
    archived.truncate(ARCHIVE_LIMIT);

    let content = signer
        .nip44_encrypt_async(&public_key, &archived.join("\n"))
        .await?;

    let event = EventBuilder::new(Kind::ApplicationSpecificData, content)
        .tag(Tag::identifier(ARCHIVE_IDENTIFIER))
        .finalize_async(signer)
        .await?;

    client.database().save_event(&event).await?;

    Ok(())
}

/// Get device keys from the local database.
pub(crate) async fn get_keys<S>(client: &Client, signer: &S) -> Result<Keys, Error>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    Ok(load_device_keys(client, signer).await?.keys)
}

/// Load encryption keys retired by a rotation from the local database.
pub(crate) async fn archived_keys<S>(client: &Client, signer: &S) -> Result<Vec<Keys>, Error>
where
    S: AsyncGetPublicKey + AsyncNip44 + ?Sized,
{
    let public_key = signer.get_public_key_async().await?;

    let filter = Filter::new()
        .kind(Kind::ApplicationSpecificData)
        .identifier(ARCHIVE_IDENTIFIER)
        .author(public_key);

    let Some(event) = client.database().query(filter).await?.into_iter().next() else {
        return Ok(vec![]);
    };

    let content = signer
        .nip44_decrypt_async(&public_key, &event.content)
        .await?;

    Ok(parse_archived_keys(&content))
}
