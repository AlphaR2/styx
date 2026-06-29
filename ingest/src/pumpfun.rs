use std::time::{SystemTime, UNIX_EPOCH};

pub const PUMP_FUN_PROGRAM: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";

// sha256("global:create")[..8]
const CREATE_DISCRIMINATOR: [u8; 8] = [24, 30, 200, 40, 5, 28, 7, 119];
// sha256("global:create_v2")[..8] — Token-2022 path introduced ~slot 419M
const CREATE_V2_DISCRIMINATOR: [u8; 8] = [214, 144, 76, 236, 95, 139, 49, 180];

pub struct LaunchInfo {
    pub mint: String,
    pub name: String,
    pub symbol: String,
    pub uri: String,
    pub creator: String,
    pub detected_at_ms: u64,
}

/// Try to parse a pump.fun token create from raw transaction data.
///
/// `account_keys` — ordered 32-byte pubkeys for the transaction.
/// `instructions` — tuples of (program_id_index, accounts_bytes, data_bytes).
///   `accounts_bytes` is a packed byte array where each byte is a u8 index into account_keys.
pub fn try_parse_launch(
    account_keys: &[Vec<u8>],
    instructions: &[(u32, Vec<u8>, Vec<u8>)],
) -> Option<LaunchInfo> {
    let pump_bytes = bs58::decode(PUMP_FUN_PROGRAM).into_vec().ok()?;
    let pump_idx = account_keys.iter().position(|k| k == &pump_bytes)? as u32;

    for (program_id_index, accounts, data) in instructions {
        if *program_id_index != pump_idx { continue; }
        if data.len() < 8 { continue; }

        let is_create    = data[..8] == CREATE_DISCRIMINATOR;
        let is_create_v2 = data[..8] == CREATE_V2_DISCRIMINATOR;
        if !is_create && !is_create_v2 { continue; }

        let mut offset = 8usize;
        let name   = read_string(data, &mut offset)?;
        let symbol = read_string(data, &mut offset)?;
        let uri    = read_string(data, &mut offset)?;

        // Both `create` and `create_v2` pass `creator: pubkey` as the 4th arg (32 bytes).
        if offset + 32 > data.len() { continue; }
        let creator_bytes_raw = &data[offset..offset + 32];
        let creator = bs58::encode(creator_bytes_raw).into_string();

        // accounts[0] in the instruction's compact account list → index into account_keys
        let mint_key_idx = *accounts.get(0)? as usize;
        let mint_bytes   = account_keys.get(mint_key_idx)?;

        return Some(LaunchInfo {
            mint: bs58::encode(mint_bytes).into_string(),
            creator,
            name,
            symbol,
            uri,
            detected_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });
    }

    None
}

/// Diagnostic: does any instruction carry the pump.fun create discriminator?
/// Returns true even if the subsequent field/account parse would fail — lets the
/// caller distinguish "no creates in the stream" from "creates present, parse failing".
pub fn has_create_ix(
    account_keys: &[Vec<u8>],
    instructions: &[(u32, Vec<u8>, Vec<u8>)],
) -> bool {
    let pump_bytes = match bs58::decode(PUMP_FUN_PROGRAM).into_vec() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let pump_idx = match account_keys.iter().position(|k| k == &pump_bytes) {
        Some(i) => i as u32,
        None => return false,
    };
    instructions.iter().any(|(pid, _accts, data)| {
        *pid == pump_idx && data.len() >= 8
            && (data[..8] == CREATE_DISCRIMINATOR || data[..8] == CREATE_V2_DISCRIMINATOR)
    })
}

fn read_string(data: &[u8], offset: &mut usize) -> Option<String> {
    if *offset + 4 > data.len() { return None; }
    let len = u32::from_le_bytes(data[*offset..*offset + 4].try_into().ok()?) as usize;
    *offset += 4;
    if *offset + len > data.len() { return None; }
    let s = String::from_utf8(data[*offset..*offset + len].to_vec()).ok()?;
    *offset += len;
    Some(s)
}
