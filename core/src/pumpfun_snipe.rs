use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
};

pub const PUMP_FUN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBymQj6f");

const TOKEN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");

const ASSOC_TOKEN_PROGRAM: Pubkey =
    solana_sdk::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJe1bJ");

const SYSTEM_PROGRAM: Pubkey =
    solana_sdk::pubkey!("11111111111111111111111111111111");

const FEE_RECIPIENT: Pubkey =
    solana_sdk::pubkey!("CebN5WGQ4jvEPvsVU4EoHEpgzq1VV7AbicfhtW4xC9iM");

// pump.fun fee program added in the cashback/rewards upgrade (2025).
// The buy instruction CPIs into this program — without it the runtime returns
// ProgramAccountNotFound even though the main pump program exists.
const FEE_PROGRAM: Pubkey =
    solana_sdk::pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");

// Second seed for fee_config PDA — hardcoded in the pump.fun IDL.
const FEE_CONFIG_SEED: [u8; 32] = [
    1, 86, 224, 246, 147, 102, 90, 207, 68, 219, 21, 104, 191, 23, 91, 170,
    81, 137, 203, 151, 245, 210, 255, 59, 101, 93, 43, 182, 253, 109, 24, 176,
];

// Anchor discriminators: sha256("global:buy")[..8] and sha256("global:sell")[..8]
const BUY_DISCRIMINATOR:  [u8; 8] = [102, 6, 61, 18, 1, 218, 235, 234];
const SELL_DISCRIMINATOR: [u8; 8] = [51, 230, 133, 164, 1, 127, 131, 173];

fn pump_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PUMP_FUN_PROGRAM).0
}

fn fee_program_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &FEE_PROGRAM).0
}

pub fn global_pda() -> Pubkey {
    pump_pda(&[b"global"])
}

pub fn bonding_curve_pda(mint: &Pubkey) -> Pubkey {
    pump_pda(&[b"bonding-curve", mint.as_ref()])
}

pub fn creator_vault_pda(creator: &Pubkey) -> Pubkey {
    pump_pda(&[b"creator-vault", creator.as_ref()])
}

pub fn event_authority_pda() -> Pubkey {
    pump_pda(&[b"__event_authority"])
}

pub fn global_volume_accumulator_pda() -> Pubkey {
    pump_pda(&[b"global_volume_accumulator"])
}

pub fn user_volume_accumulator_pda(user: &Pubkey) -> Pubkey {
    pump_pda(&[b"user_volume_accumulator", user.as_ref()])
}

pub fn fee_config_pda() -> Pubkey {
    fee_program_pda(&[b"fee_config", &FEE_CONFIG_SEED])
}

/// Associated token address for (wallet, mint) using the standard SPL token program.
pub fn get_ata(wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[wallet.as_ref(), TOKEN_PROGRAM.as_ref(), mint.as_ref()],
        &ASSOC_TOKEN_PROGRAM,
    )
    .0
}

/// Create-ATA-idempotent instruction (data byte = 1 = no-op if ATA already exists).
fn create_ata_ix(funder: &Pubkey, wallet: &Pubkey, mint: &Pubkey) -> Instruction {
    Instruction {
        program_id: ASSOC_TOKEN_PROGRAM,
        accounts: vec![
            AccountMeta::new(*funder, true),
            AccountMeta::new(get_ata(wallet, mint), false),
            AccountMeta::new_readonly(*wallet, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
        ],
        data: vec![1],
    }
}

/// Build instructions for a pump.fun snipe: [create_ata, buy].
///
/// Account layout matches the current pump.fun IDL (16 accounts, post cashback upgrade).
/// Missing any of the new accounts causes ProgramAccountNotFound at simulation time
/// because the pump program CPIs into the fee_program which must be resolvable.
///
/// `max_sol_lamports` — hard cap on SOL spent. The buy instruction buys as many
/// tokens as possible up to this amount.
pub fn build_snipe_instructions(
    payer: &Pubkey,
    mint: &Pubkey,
    creator: &Pubkey,
    max_sol_lamports: u64,
) -> Vec<Instruction> {
    let global                   = global_pda();
    let bonding_curve            = bonding_curve_pda(mint);
    let assoc_bonding_curve      = get_ata(&bonding_curve, mint);
    let assoc_user               = get_ata(payer, mint);
    let creator_vault            = creator_vault_pda(creator);
    let event_authority          = event_authority_pda();
    let global_volume_accumulator = global_volume_accumulator_pda();
    let user_volume_accumulator  = user_volume_accumulator_pda(payer);
    let fee_config               = fee_config_pda();

    // Buy as many tokens as possible, constrained by max_sol_lamports.
    // track_volume: OptionBool serialized as u8 — 0 = None (opt out of volume tracking).
    let amount: u64 = 1_000_000_000_000_000;
    let mut data = Vec::with_capacity(25);
    data.extend_from_slice(&BUY_DISCRIMINATOR);
    data.extend_from_slice(&amount.to_le_bytes());
    data.extend_from_slice(&max_sol_lamports.to_le_bytes());
    data.push(0u8); // track_volume: None

    let buy_ix = Instruction {
        program_id: PUMP_FUN_PROGRAM,
        accounts: vec![
            // ── Original accounts ─────────────────────────────────────────
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(FEE_RECIPIENT, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(assoc_bonding_curve, false),
            AccountMeta::new(assoc_user, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_PROGRAM, false),
            // ── Added in cashback/rewards upgrade ─────────────────────────
            AccountMeta::new_readonly(global_volume_accumulator, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(fee_config, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
        ],
        data,
    };

    vec![create_ata_ix(payer, payer, mint), buy_ix]
}

/// Build instructions for a pump.fun sell: [sell].
///
/// `token_amount` — exact number of tokens (in base units) to sell.
/// `min_sol_lamports` — minimum SOL to receive; the instruction reverts if slippage
/// would yield less. Pass 0 to accept any price (full slippage tolerance).
pub fn build_sell_instructions(
    payer: &Pubkey,
    mint: &Pubkey,
    creator: &Pubkey,
    token_amount: u64,
    min_sol_lamports: u64,
) -> Vec<Instruction> {
    let global                   = global_pda();
    let bonding_curve            = bonding_curve_pda(mint);
    let assoc_bonding_curve      = get_ata(&bonding_curve, mint);
    let assoc_user               = get_ata(payer, mint);
    let creator_vault            = creator_vault_pda(creator);
    let event_authority          = event_authority_pda();
    let global_volume_accumulator = global_volume_accumulator_pda();
    let user_volume_accumulator  = user_volume_accumulator_pda(payer);
    let fee_config               = fee_config_pda();

    // track_volume: OptionBool — 0 = None.
    let mut data = Vec::with_capacity(25);
    data.extend_from_slice(&SELL_DISCRIMINATOR);
    data.extend_from_slice(&token_amount.to_le_bytes());
    data.extend_from_slice(&min_sol_lamports.to_le_bytes());
    data.push(0u8); // track_volume: None

    let sell_ix = Instruction {
        program_id: PUMP_FUN_PROGRAM,
        accounts: vec![
            AccountMeta::new_readonly(global, false),
            AccountMeta::new(FEE_RECIPIENT, false),
            AccountMeta::new_readonly(*mint, false),
            AccountMeta::new(bonding_curve, false),
            AccountMeta::new(assoc_bonding_curve, false),
            AccountMeta::new(assoc_user, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM, false),
            AccountMeta::new(creator_vault, false),
            AccountMeta::new_readonly(event_authority, false),
            AccountMeta::new_readonly(PUMP_FUN_PROGRAM, false),
            AccountMeta::new_readonly(global_volume_accumulator, false),
            AccountMeta::new(user_volume_accumulator, false),
            AccountMeta::new_readonly(fee_config, false),
            AccountMeta::new_readonly(FEE_PROGRAM, false),
        ],
        data,
    };

    vec![sell_ix]
}
