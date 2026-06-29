use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use solana_sdk::{
    hash::Hash,
    instruction::Instruction,
    message::{v0, AddressLookupTableAccount, VersionedMessage},
    pubkey::Pubkey,
    signature::Signature,
    transaction::VersionedTransaction,
};
use solana_system_interface::instruction;
use solana_compute_budget_interface::ComputeBudgetInstruction;

// Everything the caller passes in to describe what should be executed.
#[derive(Clone)]
pub struct BundleSpec {
    pub user_instructions: Vec<Instruction>, // the real action: swap, transfer, snipe, etc.
    pub tip_account: Pubkey,                 // one of the 8 valid Jito tip accounts
    // Always set via compute_tip() -- the clamp (floor + ceiling) lives there.
    // Never set this field directly or the safety guarantee is silently bypassed.
    pub tip_lamports: u64,
    pub compute_unit_limit: u32,             // max CUs this transaction may consume
    // Address lookup tables referenced by the user instructions (e.g. Jupiter swaps).
    // Empty for simple transactions like a memo or transfer.
    pub address_lookup_tables: Vec<AddressLookupTableAccount>,
}

/// Builds unsigned bundle transactions — payer's signature slot is zeroed.
/// The caller fills in their signature; Styx never touches the keypair.
///
/// Layout: tip → CU limit → user ixs (matches sol-trade-sdk's proven working order).
/// Tip goes first so Jito's block engine sees it immediately during validation.
pub fn build_bundle_unsigned(
    spec: &BundleSpec,
    payer: &Pubkey,
    recent_blockhash: Hash,
) -> Result<Vec<VersionedTransaction>> {
    // Jito silently drops bundles with tips below 1 000 lamports.
    let tip_lamports = spec.tip_lamports.max(1_000);

    let mut instructions = Vec::with_capacity(2 + spec.user_instructions.len());
    instructions.push(instruction::transfer(payer, &spec.tip_account, tip_lamports));
    instructions.push(ComputeBudgetInstruction::set_compute_unit_limit(spec.compute_unit_limit));
    instructions.extend_from_slice(&spec.user_instructions);

    let message = v0::Message::try_compile(
        payer,
        &instructions,
        &spec.address_lookup_tables,
        recent_blockhash,
    )
    .context("Failed to compile v0 message")?;

    let versioned_msg = VersionedMessage::V0(message);
    let n_sigs = versioned_msg.header().num_required_signatures as usize;
    let tx = VersionedTransaction {
        signatures: vec![Signature::default(); n_sigs],
        message: versioned_msg,
    };
    Ok(vec![tx])
}

/// Builds an unsigned tip-only transaction for use as tx[1] in a 2-tx Jito bundle.
/// tx[0] is the user's main transaction (e.g. Jupiter pre-built swap).
/// tx[1] (this) pays the Jito tip so the bundle is prioritised.
/// Payer's signature slot is zeroed — caller fills it in.
pub fn build_tip_tx_unsigned(
    payer: &Pubkey,
    tip_account: Pubkey,
    tip_lamports: u64,
    recent_blockhash: Hash,
) -> Result<VersionedTransaction> {
    let tip_lamports = tip_lamports.max(1_000);
    let instructions = vec![
        ComputeBudgetInstruction::set_compute_unit_limit(5_000),
        instruction::transfer(payer, &tip_account, tip_lamports),
    ];
    let message = v0::Message::try_compile(payer, &instructions, &[], recent_blockhash)
        .context("Failed to compile tip-only message")?;
    let versioned_msg = VersionedMessage::V0(message);
    let n_sigs = versioned_msg.header().num_required_signatures as usize;
    Ok(VersionedTransaction {
        signatures: vec![Signature::default(); n_sigs],
        message: versioned_msg,
    })
}

/// Serializes a transaction to base64. Jito rejects bs58-encoded transactions.
pub fn encode_transaction(tx: &VersionedTransaction) -> Result<String> {
    let bytes = bincode::serialize(tx).context("Failed to serialize transaction")?;
    Ok(general_purpose::STANDARD.encode(bytes))
}
